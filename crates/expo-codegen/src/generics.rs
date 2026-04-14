//! Monomorphization engine: specializes generic functions, structs, and enums
//! for concrete type arguments, and manages the mangled-name encoding used to
//! distinguish each instantiation.

use std::collections::HashMap;
use std::mem;

use expo_ast::ast::{Function, ImplMember, Param, Statement, TypeExpr, TypeParam};
use expo_typecheck::context::{FunctionKind, VariantData};
use expo_typecheck::types::{
    Primitive, Type, build_substitution, mangle_name, mangle_type, named, named_generic, substitute,
};
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, PointerValue};

use crate::compiler::{Compiler, EmitResult, resolve_process_envelope_type};
use crate::drop::{Ownership, drop_live_variables};
use crate::expr::compile_expr;
use crate::hashtable::monomorphize_hashtable_struct;
use crate::intrinsics::cptr::emit_cptr_method;
use crate::list::{emit_list_method, monomorphize_list_struct};
use crate::map::emit_map_method;
use crate::process::{
    emit_ref_method, emit_reply_to_method, monomorphize_ref_struct, monomorphize_reply_to_struct,
};
use crate::registration::{build_enum_layout, build_union_layout};
use crate::set::emit_set_method;
use crate::stmt::{apply_coercion, compile_statement};
use crate::types::to_llvm_type;

/// Compiles a function body: iterates statements, handles implicit return
/// of the last expression, and inserts a terminator if missing. When
/// `is_main` is true, a missing terminator returns `0` instead of void.
pub(crate) fn compile_function_body<'ctx>(
    c: &mut Compiler<'ctx>,
    body: &[Statement],
    return_type: &Type,
    fn_value: FunctionValue<'ctx>,
    _is_main: bool,
) -> Result<(), String> {
    let saved_hint = mem::replace(
        &mut c.fn_state.return_type_hint,
        if *return_type != Type::Unit {
            Some(return_type.clone())
        } else {
            None
        },
    );

    let body_len = body.len();

    for (i, stmt) in body.iter().enumerate() {
        let is_last = i == body_len - 1;

        if c.current_block_terminated() {
            break;
        }

        if is_last && let Statement::Expr(expr) = stmt {
            c.fn_state.tco.mark_tail();
            let val = compile_expr(c, expr, fn_value)?.map(|tv| tv.value);
            c.fn_state.tco.clear_tail();
            if !c.current_block_terminated() && *return_type != Type::Unit {
                if let Some(v) = val {
                    let v = apply_coercion(c, v, expr)?;
                    c.builder.build_return(Some(&v)).unwrap();
                } else {
                    c.builder.build_unreachable().unwrap();
                }
            }
            continue;
        }

        compile_statement(c, stmt, fn_value)?;
    }

    if !c.current_block_terminated() {
        if *return_type == Type::Unit {
            drop_live_variables(c, None);
            c.builder.build_return(None).unwrap();
        } else {
            c.builder.build_unreachable().unwrap();
        }
    }

    c.fn_state.return_type_hint = saved_hint;
    Ok(())
}

/// Shared compilation kernel for method bodies: saves/restores compiler
/// state, binds `self` and regular parameters, sets up process message
/// types, then compiles the function body. Used by both `define_function`
/// (non-generic methods) and `monomorphize_impl_method` (generic methods).
///
/// `self_type` is `Some((mangled, base))` for instance/static methods.
/// `mangled` is the LLVM-registered type name (e.g. `List_$Token$`).
/// `base` is the unmangled name for `is_enum`/`is_struct` lookups.
/// For non-generic methods both are identical.
pub(crate) fn compile_method_body<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_value: FunctionValue<'ctx>,
    func: &Function,
    self_type: Option<(&str, &str)>,
    param_types: &[Type],
    return_type: &Type,
    subst: HashMap<String, Type>,
) -> Result<(), String> {
    let file = c.debug.file();
    let linkage_name = fn_value.get_name().to_str().unwrap_or("").to_string();
    c.debug.push_function(
        fn_value,
        &func.name,
        &linkage_name,
        file,
        func.span.start.line,
    );

    let entry = c.context.append_basic_block(fn_value, "entry");
    let saved_vars = mem::take(&mut c.fn_state.variables);
    let saved_block = c.builder.get_insert_block();
    let saved_subst = mem::replace(&mut c.fn_state.type_subst, subst);

    c.builder.position_at_end(entry);
    c.debug.set_location(
        c.context,
        &c.builder,
        func.span.start.line,
        func.span.start.column,
    );

    let mut param_idx = 0u32;
    let mut param_allocas: Vec<PointerValue<'ctx>> = Vec::new();

    if func
        .params
        .first()
        .is_some_and(|p| matches!(p, Param::Self_ { .. }))
        && let Some((mangled, base)) = self_type
    {
        let self_ty = if base == "CPtr" {
            Type::Pointer(Box::new(Type::Unknown))
        } else if let Some(p) = Primitive::from_name(base) {
            Type::Primitive(p)
        } else {
            named(mangled)
        };
        if let Some(llvm_ty) = to_llvm_type(&self_ty, c.context, &c.types) {
            let alloca = c.builder.build_alloca(llvm_ty, "self").unwrap();
            let param_val = fn_value.get_nth_param(param_idx).unwrap();
            c.builder.build_store(alloca, param_val).unwrap();
            c.fn_state
                .variables
                .insert("self".to_string(), (alloca, self_ty, Ownership::Unowned));
            param_allocas.push(alloca);
            param_idx += 1;
        }
    }

    let mut type_idx = 0usize;
    for param in func.params.iter() {
        if let Param::Regular { name: pname, .. } = param
            && type_idx < param_types.len()
        {
            let ty = &param_types[type_idx];
            type_idx += 1;
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.types) {
                let alloca = c.builder.build_alloca(llvm_ty, pname).unwrap();
                let param_val = fn_value.get_nth_param(param_idx).unwrap();
                c.builder.build_store(alloca, param_val).unwrap();
                c.fn_state
                    .variables
                    .insert(pname.clone(), (alloca, ty.clone(), Ownership::Unowned));
                param_allocas.push(alloca);
                param_idx += 1;
            }
        }
    }

    let loop_header = c.context.append_basic_block(fn_value, "tco_loop");
    c.builder.build_unconditional_branch(loop_header).unwrap();
    c.builder.position_at_end(loop_header);

    let saved_process_msg = c.fn_state.process_msg_type.take();
    if let Some((mangled, _)) = self_type {
        c.fn_state.process_msg_type = resolve_process_envelope_type(c, mangled);
        if let Some(env_type) = c.fn_state.process_msg_type.clone() {
            let _ = ensure_types_exist(c, &env_type);
        }
    }

    let saved_fn = c
        .fn_state
        .tco
        .enter_fn(fn_value.get_name().to_str().unwrap_or("").to_string());
    let saved_loop = c.fn_state.tco.set_loop(loop_header, param_allocas);

    let result = compile_function_body(
        c,
        func.body.as_deref().unwrap_or(&[]),
        return_type,
        fn_value,
        false,
    );

    c.fn_state.tco.leave_fn(saved_fn);
    c.fn_state.tco.restore_loop(saved_loop);
    c.fn_state.process_msg_type = saved_process_msg;
    c.fn_state.variables = saved_vars;
    c.fn_state.type_subst = saved_subst;
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }

    c.debug.pop_scope(c.context, &c.builder);

    result
}

/// Generates a monomorphized (specialized) version of a generic function for
/// the given concrete type arguments. Declares the LLVM function, compiles its
/// body with type variables substituted, and registers it under the mangled name.
pub(crate) fn monomorphize_function<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    type_args: &[Type],
) -> Result<(), String> {
    let func_ast = c
        .generic_fn_asts
        .get(name)
        .ok_or_else(|| format!("no generic function `{name}` to monomorphize"))?
        .clone();

    let mangled = mangle_name(name, type_args);
    if c.functions.contains_key(&mangled) {
        return Ok(());
    }

    let sig = c
        .type_ctx
        .functions
        .get(name)
        .ok_or_else(|| format!("no signature for generic function `{name}`"))?;

    let subst = build_substitution(&sig.type_params, type_args);

    let return_type = substitute(&sig.return_type, &subst);

    let param_types: Vec<Type> = sig
        .params
        .iter()
        .map(|p| substitute(&p.ty, &subst))
        .collect();

    ensure_types_exist(c, &return_type)?;
    for pt in &param_types {
        ensure_types_exist(c, pt)?;
    }

    let llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = param_types
        .iter()
        .filter_map(|ty| to_llvm_type(ty, c.context, &c.types))
        .map(|t| t.into())
        .collect();

    let fn_type = match to_llvm_type(&return_type, c.context, &c.types) {
        Some(ret) => ret.fn_type(&llvm_param_types, false),
        None => c.context.void_type().fn_type(&llvm_param_types, false),
    };

    let fn_value = c.module.add_function(&mangled, fn_type, None);
    c.functions.insert(mangled.clone(), fn_value);

    let file = c.debug.file();
    c.debug
        .push_function(fn_value, name, &mangled, file, func_ast.span.start.line);

    let entry = c.context.append_basic_block(fn_value, "entry");
    let saved_vars = mem::take(&mut c.fn_state.variables);
    let saved_block = c.builder.get_insert_block();
    let saved_subst = mem::replace(&mut c.fn_state.type_subst, subst.clone());

    c.builder.position_at_end(entry);
    c.debug.set_location(
        c.context,
        &c.builder,
        func_ast.span.start.line,
        func_ast.span.start.column,
    );

    for (i, param) in func_ast.params.iter().enumerate() {
        if let Param::Regular { name: pname, .. } = param {
            let ty = &param_types[i];
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.types) {
                let alloca = c.builder.build_alloca(llvm_ty, pname).unwrap();
                let param_val = fn_value.get_nth_param(i as u32).unwrap();
                c.builder.build_store(alloca, param_val).unwrap();
                c.fn_state
                    .variables
                    .insert(pname.clone(), (alloca, ty.clone(), Ownership::Unowned));
            }
        }
    }

    compile_function_body(
        c,
        func_ast.body.as_deref().unwrap_or(&[]),
        &return_type,
        fn_value,
        false,
    )?;

    c.fn_state.variables = saved_vars;
    c.fn_state.type_subst = saved_subst;
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }

    c.debug.pop_scope(c.context, &c.builder);

    Ok(())
}

/// Generates a monomorphized (specialized) version of a generic struct for
/// the given concrete type arguments. Creates the LLVM struct type with
/// concrete field types and registers it under the mangled name.
pub(crate) fn monomorphize_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    type_args: &[Type],
) -> Result<(), String> {
    let mangled = mangle_name(name, type_args);
    if c.types.contains_monomorphized(&mangled) {
        return Ok(());
    }

    if name == "List" {
        return monomorphize_list_struct(c, &mangled);
    }
    if name == "Map" || name == "Set" {
        return monomorphize_hashtable_struct(c, &mangled);
    }
    if name == "Ref" {
        return monomorphize_ref_struct(c, &mangled);
    }
    if name == "ReplyTo" {
        return monomorphize_reply_to_struct(c, &mangled);
    }

    let info = c
        .type_ctx
        .find_type(name)
        .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;
    let fields = info
        .fields()
        .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;

    let subst = build_substitution(&info.type_params, type_args);

    let concrete_fields: Vec<(String, Type)> = fields
        .iter()
        .map(|(fname, fty)| (fname.clone(), substitute(fty, &subst)))
        .collect();

    let st = c.context.opaque_struct_type(&mangled);
    c.types.register_monomorphized(mangled.clone(), st);

    let mut deferred_indirect = Vec::new();
    for (_, fty) in &concrete_fields {
        if let Type::Indirect(inner) = fty {
            deferred_indirect.push(inner.as_ref().clone());
        } else {
            ensure_types_exist(c, fty)?;
        }
    }

    // `to_llvm_type` returns `None` for `Unit` and other ZSTs, but we must keep one
    // LLVM field per logical field so GEP indices match `mono_struct_info` (e.g.
    // `Pair<Unit, T>.second` is index 1, not 0 when `first` is Unit).
    let field_llvm_types: Vec<_> = concrete_fields
        .iter()
        .map(|(_, ty)| {
            to_llvm_type(ty, c.context, &c.types).unwrap_or_else(|| c.context.i8_type().into())
        })
        .collect();
    st.set_body(&field_llvm_types, false);

    for ty in &deferred_indirect {
        ensure_types_exist(c, ty)?;
    }

    c.types.mono_struct_info.insert(mangled, concrete_fields);

    Ok(())
}

/// Generates a monomorphized (specialized) version of a generic enum for
/// the given concrete type arguments. Creates the LLVM tagged union type
/// with concrete variant payloads and registers it under the mangled name.
pub(crate) fn monomorphize_enum<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    type_args: &[Type],
) -> Result<(), String> {
    let mangled = mangle_name(name, type_args);
    if c.types.contains_monomorphized(&mangled) {
        return Ok(());
    }

    let info = c
        .type_ctx
        .find_type(name)
        .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;
    let variants = info
        .variants()
        .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;

    let subst = build_substitution(&info.type_params, type_args);

    let concrete_variants: Vec<_> = variants
        .iter()
        .map(|vi| {
            let data = match &vi.data {
                VariantData::Unit => VariantData::Unit,
                VariantData::Tuple(types) => {
                    VariantData::Tuple(types.iter().map(|t| substitute(t, &subst)).collect())
                }
                VariantData::Struct(fields) => VariantData::Struct(
                    fields
                        .iter()
                        .map(|(n, t)| (n.clone(), substitute(t, &subst)))
                        .collect(),
                ),
            };
            (vi.name.clone(), data)
        })
        .collect();

    let enum_type = c.context.opaque_struct_type(&mangled);
    c.types.register_monomorphized(mangled.clone(), enum_type);

    for (_, vdata) in &concrete_variants {
        match vdata {
            VariantData::Unit => {}
            VariantData::Tuple(types) => {
                for ty in types {
                    ensure_types_exist(c, ty)?;
                }
            }
            VariantData::Struct(fields) => {
                for (_, ty) in fields {
                    ensure_types_exist(c, ty)?;
                }
            }
        }
    }

    build_enum_layout(c, &mangled, enum_type, &concrete_variants);

    c.types
        .mono_enum_variants
        .insert(mangled, concrete_variants);

    Ok(())
}

/// Fully resolved method signature: AST, types, substitutions, and self-type.
/// Produced by `resolve_method_signature` without any LLVM emission.
struct ResolvedMethodSignature {
    func_ast: Function,
    is_static: bool,
    mangled_fn: String,
    mangled_type: String,
    param_types: Vec<Type>,
    return_type: Type,
    self_type: Option<Type>,
    subst: HashMap<String, Type>,
}

/// Resolves the method signature for a generic impl method by looking up
/// the AST (specialized or generic path), building type substitutions,
/// and computing parameter/return types. No LLVM emission.
///
/// Returns `None` if the method was already compiled (cached in `functions`).
fn resolve_method_signature(
    compiler: &Compiler,
    base_type: &str,
    method_name: &str,
    type_args: &[Type],
    method_type_args: &[Type],
) -> Result<Option<ResolvedMethodSignature>, String> {
    let mangled_type = mangle_name(base_type, type_args);
    let mangled_fn = if method_type_args.is_empty() {
        format!("{}_{}", mangled_type, method_name)
    } else {
        let mangled_method = mangle_name(method_name, method_type_args);
        format!("{}_{}", mangled_type, mangled_method)
    };
    if compiler.functions.contains_key(&mangled_fn) {
        return Ok(None);
    }

    let spec_id = compiler.type_ctx.resolve_name(base_type).cloned();
    let specialized_match = spec_id.as_ref().and_then(|id| {
        compiler
            .type_ctx
            .specialized_impl_asts
            .get(id)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(concrete_args, _)| concrete_args == type_args)
                    .cloned()
            })
    });

    let (func_ast, subst, return_type, param_types, is_static) =
        if let Some((concrete_args, spec_block)) = specialized_match {
            let mut method_ast = None;
            for member in &spec_block.members {
                if let ImplMember::Function(f) = member
                    && f.name == method_name
                {
                    method_ast = Some(f.clone());
                    break;
                }
            }
            let func_ast = method_ast.ok_or_else(|| {
                format!("method `{method_name}` not found in specialized impl for `{base_type}`")
            })?;

            let mut subst = HashMap::new();
            for (tp, ta) in func_ast.type_params.iter().zip(method_type_args.iter()) {
                subst.insert(tp.name.clone(), ta.clone());
            }

            let spec_sig = spec_id
            .as_ref()
            .and_then(|id| {
                compiler
                    .type_ctx
                    .specialized_methods
                    .get(id)
                    .and_then(|entries| {
                        entries
                            .iter()
                            .find(|(args, _)| *args == concrete_args)
                            .and_then(|(_, sigs)| sigs.get(method_name))
                    })
            })
            .ok_or_else(|| {
                format!(
                    "no signature for method `{method_name}` in specialized impl for `{base_type}`"
                )
            })?;

            let ret = substitute(&spec_sig.return_type, &subst);
            let pts: Vec<Type> = spec_sig
                .params
                .iter()
                .map(|p| substitute(&p.ty, &subst))
                .collect();
            let is_static = spec_sig.kind == FunctionKind::Static;
            (func_ast, subst, ret, pts, is_static)
        } else {
            let impl_blocks = compiler
                .type_ctx
                .generic_impl_asts
                .get(base_type)
                .ok_or_else(|| format!("no generic impl for `{base_type}`"))?
                .clone();

            let mut method_ast = None;
            let mut impl_type_params: Vec<TypeParam> = Vec::new();
            for block in &impl_blocks {
                if let TypeExpr::Generic { args, .. } = &block.target {
                    let impl_tps: Vec<TypeParam> = args
                        .iter()
                        .filter_map(|a| {
                            if let TypeExpr::Named { path, span, .. } = a
                                && path.len() == 1
                            {
                                return Some(TypeParam {
                                    name: path[0].clone(),
                                    bounds: Vec::new(),
                                    span: *span,
                                });
                            }
                            None
                        })
                        .collect();
                    for member in &block.members {
                        if let ImplMember::Function(f) = member
                            && f.name == method_name
                        {
                            method_ast = Some(f.clone());
                            impl_type_params = impl_tps;
                            break;
                        }
                    }
                    if method_ast.is_some() {
                        break;
                    }
                }
            }

            let func_ast = method_ast.ok_or_else(|| {
                format!("method `{method_name}` not found in impl for `{base_type}`")
            })?;

            let mut subst = build_substitution(&impl_type_params, type_args);
            for (tp, ta) in func_ast.type_params.iter().zip(method_type_args.iter()) {
                subst.insert(tp.name.clone(), ta.clone());
            }

            let info = compiler
                .type_ctx
                .find_type(base_type)
                .map(|ti| (&ti.functions, &ti.type_params));

            let (return_type, param_types, is_static) = if let Some((methods, _)) = info {
                if let Some(sig) = methods.get(method_name) {
                    let ret = substitute(&sig.return_type, &subst);
                    let pts: Vec<Type> = sig
                        .params
                        .iter()
                        .map(|p| substitute(&p.ty, &subst))
                        .collect();
                    let is_static = sig.kind == FunctionKind::Static;
                    (ret, pts, is_static)
                } else {
                    return Err(format!(
                        "no signature for method `{method_name}` on `{base_type}`"
                    ));
                }
            } else {
                return Err(format!("no type info for `{base_type}`"));
            };
            (func_ast, subst, return_type, param_types, is_static)
        };

    let self_type = if is_static {
        None
    } else if base_type == "CPtr" {
        Some(Type::Pointer(Box::new(
            type_args.first().cloned().unwrap_or(Type::Unknown),
        )))
    } else {
        Some(named_generic(
            base_type,
            type_args.to_vec(),
            compiler.type_ctx,
        ))
    };

    Ok(Some(ResolvedMethodSignature {
        func_ast,
        is_static,
        mangled_fn,
        mangled_type,
        param_types,
        return_type,
        self_type,
        subst,
    }))
}

/// Generates a monomorphized version of a method from a generic impl block.
/// Finds the method AST via `resolve_method_signature`, then creates the LLVM
/// function and compiles the body.
///
/// When `method_type_args` is non-empty, method-level type parameters
/// (e.g. `U` in `map<U>`) are also substituted into the mangled name
/// and type substitution map.
pub(crate) fn monomorphize_impl_method<'ctx>(
    c: &mut Compiler<'ctx>,
    base_type: &str,
    method_name: &str,
    type_args: &[Type],
    method_type_args: &[Type],
) -> Result<(), String> {
    let mangled_type = mangle_name(base_type, type_args);
    let mangled_fn = if method_type_args.is_empty() {
        format!("{}_{}", mangled_type, method_name)
    } else {
        let mangled_method = mangle_name(method_name, method_type_args);
        format!("{}_{}", mangled_type, mangled_method)
    };
    if c.functions.contains_key(&mangled_fn) {
        return Ok(());
    }

    if method_type_args.is_empty() {
        match base_type {
            "CPtr" => {
                if let EmitResult::Emitted =
                    emit_cptr_method(c, &mangled_type, &mangled_fn, method_name, type_args)?
                {
                    return Ok(());
                }
            }
            "List" => {
                if let EmitResult::Emitted =
                    emit_list_method(c, &mangled_type, &mangled_fn, method_name, type_args)?
                {
                    return Ok(());
                }
            }
            "Map" => {
                if let EmitResult::Emitted =
                    emit_map_method(c, &mangled_type, &mangled_fn, method_name, type_args)?
                {
                    return Ok(());
                }
            }
            "Ref" => {
                if let EmitResult::Emitted =
                    emit_ref_method(c, &mangled_type, &mangled_fn, method_name, type_args)?
                {
                    return Ok(());
                }
            }
            "ReplyTo" => {
                if let EmitResult::Emitted =
                    emit_reply_to_method(c, &mangled_type, &mangled_fn, method_name, type_args)?
                {
                    return Ok(());
                }
            }
            "Set" => {
                if let EmitResult::Emitted =
                    emit_set_method(c, &mangled_type, &mangled_fn, method_name, type_args)?
                {
                    return Ok(());
                }
            }
            _ => {}
        }
    }

    let Some(sig) =
        resolve_method_signature(c, base_type, method_name, type_args, method_type_args)?
    else {
        return Ok(());
    };

    ensure_types_exist(c, &sig.return_type)?;
    for pt in &sig.param_types {
        ensure_types_exist(c, pt)?;
    }

    let mut llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = Vec::new();

    if let Some(self_expo_type) = &sig.self_type {
        let self_llvm_type = c
            .types
            .get_monomorphized(&sig.mangled_type)
            .map(|st| -> BasicTypeEnum { st.into() })
            .or_else(|| to_llvm_type(self_expo_type, c.context, &c.types))
            .ok_or_else(|| format!("no LLVM type for `{}`", sig.mangled_type))?;
        llvm_param_types.push(self_llvm_type.into());
    }

    for ty in &sig.param_types {
        let lt = to_llvm_type(ty, c.context, &c.types).ok_or_else(|| {
            format!(
                "no LLVM type for method parameter type `{ty:?}` in `{}`",
                sig.mangled_fn
            )
        })?;
        llvm_param_types.push(lt.into());
    }

    let fn_type = match to_llvm_type(&sig.return_type, c.context, &c.types) {
        Some(ret) => ret.fn_type(&llvm_param_types, false),
        None => c.context.void_type().fn_type(&llvm_param_types, false),
    };

    let fn_value = c.module.add_function(&sig.mangled_fn, fn_type, None);
    c.functions.insert(sig.mangled_fn.clone(), fn_value);

    let self_type = if sig.is_static {
        None
    } else {
        Some((sig.mangled_type.as_str(), base_type))
    };
    compile_method_body(
        c,
        fn_value,
        &sig.func_ast,
        self_type,
        &sig.param_types,
        &sig.return_type,
        sig.subst,
    )
}

/// Ensures that all concrete types referenced by `ty` have been registered.
/// For mangled generic names, triggers monomorphization if needed.
pub(crate) fn ensure_types_exist<'ctx>(c: &mut Compiler<'ctx>, ty: &Type) -> Result<(), String> {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            let name = &identifier.name;
            if type_args.is_empty() {
                if c.types.get_stdlib(name).is_none()
                    && !c.types.contains_monomorphized(name)
                    && let Some((base, args)) = parse_mangled_name(name, c)
                {
                    if c.type_ctx.is_enum(&base) {
                        monomorphize_enum(c, &base, &args)?;
                    } else {
                        monomorphize_struct(c, &base, &args)?;
                    }
                }
            } else {
                for arg in type_args {
                    ensure_types_exist(c, arg)?;
                }
                let mangled = mangle_name(name, type_args);
                if !c.types.contains_monomorphized(&mangled) {
                    if c.type_ctx.is_enum(name) {
                        monomorphize_enum(c, name, type_args)?;
                    } else {
                        monomorphize_struct(c, name, type_args)?;
                    }
                }
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for fp in params {
                ensure_types_exist(c, &fp.ty)?;
            }
            ensure_types_exist(c, return_type)?;
        }
        Type::Indirect(inner) => {
            ensure_types_exist(c, inner)?;
        }
        Type::Pointer(inner) => {
            ensure_types_exist(c, inner)?;
        }
        Type::Union(members) => {
            for m in members {
                ensure_types_exist(c, m)?;
            }
            let mangled = mangle_type(ty);
            if !c.types.contains_monomorphized(&mangled) {
                let opaque = c.context.opaque_struct_type(&mangled);
                c.types.register_monomorphized(mangled.clone(), opaque);
                build_union_layout(c, &mangled, opaque, members);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Public entry point for parsing a mangled name from call sites outside this
/// module (e.g. method call dispatch in `structs.rs`).
pub fn try_parse_mangled_name(mangled: &str, c: &Compiler) -> Option<(String, Vec<Type>)> {
    parse_mangled_name(mangled, c)
}

/// Attempts to recover the base name and concrete type args from a mangled
/// name like `Pair_$i32.string$`. Returns `None` if the name doesn't match
/// a known generic struct or enum template.
fn parse_mangled_name(mangled: &str, c: &Compiler) -> Option<(String, Vec<Type>)> {
    let sep_pos = mangled.find("_$")?;
    let base = &mangled[..sep_pos];
    if !c.type_ctx.generic_struct_asts.contains_key(base)
        && !c.type_ctx.generic_enum_asts.contains_key(base)
    {
        return None;
    }
    if !mangled.ends_with('$') {
        return None;
    }
    let inner = &mangled[sep_pos + 2..mangled.len() - 1];
    let parts = split_mangled_args(inner);
    let type_args: Vec<Type> = parts.iter().map(|s| parse_mangled_type(s)).collect();
    Some((base.to_string(), type_args))
}

/// Splits a mangled args string on `.` at depth 0, respecting nested `_$...$`.
fn split_mangled_args(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'$' {
            depth += 1;
            current.push('_');
            current.push('$');
            i += 2;
        } else if bytes[i] == b'$' {
            depth -= 1;
            current.push('$');
            i += 1;
        } else if bytes[i] == b'.' && depth == 0 {
            parts.push(mem::take(&mut current));
            i += 1;
        } else {
            current.push(bytes[i] as char);
            i += 1;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn parse_mangled_type(s: &str) -> Type {
    if s == "unit" {
        return Type::Unit;
    }
    if let Some(p) = Primitive::from_name(s) {
        return Type::Primitive(p);
    }
    named(s)
}
