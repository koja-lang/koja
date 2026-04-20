//! Monomorphization engine: specializes generic functions, structs, and enums
//! for concrete type arguments, and manages the mangled-name encoding used to
//! distinguish each instantiation.

use std::collections::HashMap;
use std::mem;

use expo_ast::ast::{Function, Param, Statement};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Primitive, Type, build_substitution, mangle_method_suffix, mangle_name, mangle_type, named,
    substitute,
};
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, PointerValue};

use crate::compiler::{Compiler, EmitResult};
use crate::drop::{Ownership, drop_live_variables};
use crate::expr::compile_expr;
use crate::hashtable::monomorphize_hashtable_struct;
use expo_ir::lower::mangling::try_parse_mangled_name;
use expo_ir::lower::methods::resolve_method_signature;
use expo_ir::lower::processes::resolve_process_envelope_type;
use expo_ir::lower::types::resolve_name_current;

use crate::intrinsics::cptr::emit_cptr_method;
use crate::list::{emit_list_method, monomorphize_list_struct};
use crate::map::emit_map_method;
use crate::process::{
    emit_ref_method, emit_reply_to_method, monomorphize_ref_struct, monomorphize_reply_to_struct,
};
use crate::registration::build_enum_layout;
use crate::set::emit_set_method;
use crate::stmt::{apply_coercion, compile_statement};
use crate::types::to_llvm_type;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};

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
        &mut c.fn_lower.return_type_hint,
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
            c.fn_lower.mark_tail();
            let val = compile_expr(c, expr, fn_value)?.map(|tv| tv.value);
            c.fn_lower.clear_tail();
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

    c.fn_lower.return_type_hint = saved_hint;
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
    let saved_subst = mem::replace(&mut c.fn_lower.type_subst, subst);

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
        } else if mangled != base {
            named(mangled)
        } else if let Some(id) = resolve_name_current(&c.lower_ctx(), base) {
            Type::Named {
                identifier: id.clone(),
                type_args: vec![],
            }
        } else {
            named(mangled)
        };
        if let Some(llvm_ty) = to_llvm_type(&self_ty, c.context, &c.llvm_types) {
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
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.llvm_types) {
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

    let saved_process_msg = c.fn_lower.process_msg_type.take();
    if let Some((mangled, _)) = self_type {
        c.fn_lower.process_msg_type = resolve_process_envelope_type(&c.lower_ctx(), mangled);
        if let Some(env_type) = c.fn_lower.process_msg_type.clone() {
            let _ = ensure_types_exist(c, &env_type);
        }
    }

    let saved_fn = c
        .fn_lower
        .enter_fn(fn_value.get_name().to_str().unwrap_or("").to_string());
    let saved_loop = c.fn_state.set_loop(loop_header, param_allocas);

    let result = compile_function_body(
        c,
        func.body.as_deref().unwrap_or(&[]),
        return_type,
        fn_value,
        false,
    );

    c.fn_lower.leave_fn(saved_fn);
    c.fn_state.restore_loop(saved_loop);
    c.fn_lower.process_msg_type = saved_process_msg;
    c.fn_state.variables = saved_vars;
    c.fn_lower.type_subst = saved_subst;
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

    let mangled = mangle_method_suffix(name, type_args);
    if c.functions.contains_key(&FunctionIdentifier::new(&mangled)) {
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
        .filter_map(|ty| to_llvm_type(ty, c.context, &c.llvm_types))
        .map(|t| t.into())
        .collect();

    let fn_type = match to_llvm_type(&return_type, c.context, &c.llvm_types) {
        Some(ret) => ret.fn_type(&llvm_param_types, false),
        None => c.context.void_type().fn_type(&llvm_param_types, false),
    };

    let fn_value = c.module.add_function(&mangled, fn_type, None);
    c.functions
        .insert(FunctionIdentifier::new(mangled.clone()), fn_value);

    let file = c.debug.file();
    c.debug
        .push_function(fn_value, name, &mangled, file, func_ast.span.start.line);

    let entry = c.context.append_basic_block(fn_value, "entry");
    let saved_vars = mem::take(&mut c.fn_state.variables);
    let saved_block = c.builder.get_insert_block();
    let saved_subst = mem::replace(&mut c.fn_lower.type_subst, subst.clone());

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
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.llvm_types) {
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
    c.fn_lower.type_subst = saved_subst;
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
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<(), String> {
    let mangled = mangle_name(id, type_args);
    if c.llvm_types
        .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&mangled))
    {
        return Ok(());
    }

    let name = id.name.as_str();
    if id.is_std() {
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
    }

    let info = c
        .type_ctx
        .get_type(id)
        .cloned()
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
    c.llvm_types
        .register_monomorphized(MonomorphizedTypeIdentifier::new(mangled.clone()), st);

    let mut deferred_indirect = Vec::new();
    for (_, fty) in &concrete_fields {
        if let Type::Indirect(inner) = fty {
            deferred_indirect.push(inner.as_ref().clone());
        } else {
            ensure_types_exist(c, fty)?;
        }
    }

    // `to_llvm_type` returns `None` for `Unit` and other ZSTs, but we must keep one
    // LLVM field per logical field so GEP indices match `TypeLayouts` (e.g.
    // `Pair<Unit, T>.second` is index 1, not 0 when `first` is Unit).
    let field_llvm_types: Vec<_> = concrete_fields
        .iter()
        .map(|(_, ty)| {
            to_llvm_type(ty, c.context, &c.llvm_types).unwrap_or_else(|| c.context.i8_type().into())
        })
        .collect();
    st.set_body(&field_llvm_types, false);

    for ty in &deferred_indirect {
        ensure_types_exist(c, ty)?;
    }

    c.layouts
        .register_struct_layout(MonomorphizedTypeIdentifier::new(&mangled), concrete_fields);

    Ok(())
}

/// Generates a monomorphized (specialized) version of a generic enum for
/// the given concrete type arguments. Creates the LLVM tagged union type
/// with concrete variant payloads and registers it under the mangled name.
pub(crate) fn monomorphize_enum<'ctx>(
    c: &mut Compiler<'ctx>,
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<(), String> {
    let mangled = mangle_name(id, type_args);
    if c.llvm_types
        .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&mangled))
    {
        return Ok(());
    }

    let name = id.name.as_str();
    let info = c
        .type_ctx
        .get_type(id)
        .cloned()
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
    c.llvm_types
        .register_monomorphized(MonomorphizedTypeIdentifier::new(mangled.clone()), enum_type);

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

    c.layouts.register_enum_variants(
        MonomorphizedTypeIdentifier::new(&mangled),
        concrete_variants,
    );

    Ok(())
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
    let base_id = resolve_name_current(&c.lower_ctx(), base_type)
        .cloned()
        .ok_or_else(|| format!("cannot resolve package for generic method base `{base_type}`"))?;
    let mangled_type = mangle_name(&base_id, type_args);
    let mangled_fn = if method_type_args.is_empty() {
        format!("{}_{}", mangled_type, method_name)
    } else {
        let mangled_method = mangle_method_suffix(method_name, method_type_args);
        format!("{}_{}", mangled_type, mangled_method)
    };
    if c.functions
        .contains_key(&FunctionIdentifier::new(&mangled_fn))
    {
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

    let Some(sig) = resolve_method_signature(
        &c.lower_ctx(),
        base_type,
        method_name,
        type_args,
        method_type_args,
        |fn_name| c.functions.contains_key(&FunctionIdentifier::new(fn_name)),
    )?
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
            .llvm_types
            .get_monomorphized(&sig.mangled_type)
            .map(|st| -> BasicTypeEnum { st.into() })
            .or_else(|| to_llvm_type(self_expo_type, c.context, &c.llvm_types))
            .ok_or_else(|| format!("no LLVM type for `{}`", sig.mangled_type))?;
        llvm_param_types.push(self_llvm_type.into());
    }

    for ty in &sig.param_types {
        let lt = to_llvm_type(ty, c.context, &c.llvm_types).ok_or_else(|| {
            format!(
                "no LLVM type for method parameter type `{ty:?}` in `{}`",
                sig.mangled_fn
            )
        })?;
        llvm_param_types.push(lt.into());
    }

    let fn_type = match to_llvm_type(&sig.return_type, c.context, &c.llvm_types) {
        Some(ret) => ret.fn_type(&llvm_param_types, false),
        None => c.context.void_type().fn_type(&llvm_param_types, false),
    };

    let fn_value = c
        .module
        .add_function(sig.mangled_fn.as_str(), fn_type, None);
    c.functions
        .insert(FunctionIdentifier::new(sig.mangled_fn.clone()), fn_value);

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
                if c.llvm_types.get_concrete(identifier).is_none()
                    && !c
                        .llvm_types
                        .contains_monomorphized(&MonomorphizedTypeIdentifier::new(name))
                    && let Some((base, args)) = try_parse_mangled_name(&c.lower_ctx(), name)
                    && let Some(base_id) = resolve_name_current(&c.lower_ctx(), &base).cloned()
                {
                    if c.type_ctx.is_enum(&base) {
                        monomorphize_enum(c, &base_id, &args)?;
                    } else {
                        monomorphize_struct(c, &base_id, &args)?;
                    }
                }
            } else {
                for arg in type_args {
                    ensure_types_exist(c, arg)?;
                }
                let mangled = mangle_name(identifier, type_args);
                if !c
                    .llvm_types
                    .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&mangled))
                {
                    if c.type_ctx.is_enum(name) {
                        monomorphize_enum(c, identifier, type_args)?;
                    } else {
                        monomorphize_struct(c, identifier, type_args)?;
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
            if !c
                .llvm_types
                .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&mangled))
            {
                let opaque = c.context.opaque_struct_type(&mangled);
                c.llvm_types.register_monomorphized(
                    MonomorphizedTypeIdentifier::new(mangled.clone()),
                    opaque,
                );
                // Defer body sizing: at this point member enum/struct bodies
                // may still be opaque (Pass 1b runs before Pass 2/3 set
                // bodies), which would size the union to `[i8 tag]` only.
                // `finalize_pending_unions` lays out the body once member
                // bodies are known.
                c.llvm_types
                    .pending_union_layouts
                    .push((opaque, members.clone()));
            }
        }
        _ => {}
    }
    Ok(())
}
