//! Monomorphization engine: specializes generic functions, structs, and enums
//! for concrete type arguments, and manages the mangled-name encoding used to
//! distinguish each instantiation.

use std::collections::HashMap;
use std::mem;

use expo_ast::ast::{Function, Param, Statement};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Primitive, Type, mangle_method_suffix, mangle_name, mangle_type, named,
};
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, PointerValue};

use crate::compiler::{Compiler, EmitResult};
use crate::drop::{Ownership, drop_live_variables};
use crate::expr::compile_expr;
use crate::hashtable::monomorphize_hashtable_struct;
use expo_ir::lower::mangling::try_parse_mangled_name;
use expo_ir::lower::processes::resolve_process_envelope_type;
use expo_ir::lower::types::resolve_name_current;
use expo_ir::{IREnum, IRFunction, IRFunctionKind, IRStruct, IRStructKind};

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
/// the given concrete type arguments. Thin shim around the planner /
/// emitter pair: `expo_ir::lower::monomorphize::monomorphize_function`
/// appends an [`IRFunction`] to `c.ir`, then [`emit_ir_function`] walks
/// that decl to declare the LLVM function and compile its body.
pub(crate) fn monomorphize_function<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    type_args: &[Type],
) -> Result<(), String> {
    let new_id = {
        let generic_fn_asts = mem::take(&mut c.generic_fn_asts);
        let (lower_ctx, ir) = c.lower_ctx_and_ir();
        let result = expo_ir::lower::monomorphize::monomorphize_function(
            &lower_ctx,
            ir,
            &generic_fn_asts,
            name,
            type_args,
        );
        c.generic_fn_asts = generic_fn_asts;
        result?
    };
    let Some(new_id) = new_id else {
        return Ok(());
    };
    let decl =
        c.ir.functions
            .get(&new_id)
            .expect("planner just inserted")
            .clone();
    emit_ir_function(c, &decl)
}

/// Generates a monomorphized (specialized) version of a generic struct for
/// the given concrete type arguments. Plans an [`IRStruct`] via the
/// `expo-ir` monomorphize planner, then defers to [`emit_ir_struct`] for
/// LLVM emission.
pub(crate) fn monomorphize_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<(), String> {
    let new_id = {
        let (lower_ctx, ir) = c.lower_ctx_and_ir();
        expo_ir::lower::monomorphize::monomorphize_struct(&lower_ctx, ir, id, type_args)?
    };
    let Some(new_id) = new_id else {
        return Ok(());
    };
    let decl =
        c.ir.structs
            .get(&new_id)
            .expect("planner just inserted")
            .clone();
    emit_ir_struct(c, &decl)
}

/// Generates a monomorphized (specialized) version of a generic enum for
/// the given concrete type arguments. Plans an [`IREnum`] via the
/// `expo-ir` monomorphize planner, then defers to [`emit_ir_enum`] for
/// LLVM emission.
pub(crate) fn monomorphize_enum<'ctx>(
    c: &mut Compiler<'ctx>,
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<(), String> {
    let new_id = {
        let (lower_ctx, ir) = c.lower_ctx_and_ir();
        expo_ir::lower::monomorphize::monomorphize_enum(&lower_ctx, ir, id, type_args)?
    };
    let Some(new_id) = new_id else {
        return Ok(());
    };
    let decl =
        c.ir.enums
            .get(&new_id)
            .expect("planner just inserted")
            .clone();
    emit_ir_enum(c, &decl)
}

/// Generates a monomorphized version of a method from a generic impl block.
/// Stdlib intrinsic methods (`List`, `Map`, `Set`, `Ref`, `ReplyTo`, `CPtr`)
/// are dispatched directly to their backend-defined emitters; user methods
/// are planned into [`IRFunction`] (kind = `Method`) by the `expo-ir`
/// monomorphize planner and then emitted by [`emit_ir_impl_method`].
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
    if c.ir
        .contains_function(&FunctionIdentifier::new(&mangled_fn))
    {
        return Ok(());
    }

    // Stdlib intrinsic dispatch happens *before* IR planning: these
    // methods have no AST body and are emitted directly by the backend.
    // `IRFunction` does not record them today; if a future wave grows IR
    // intrinsic decls, this dispatch can move into `emit_ir_impl_method`.
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

    let new_id = {
        let (lower_ctx, ir) = c.lower_ctx_and_ir();
        expo_ir::lower::monomorphize::monomorphize_impl_method(
            &lower_ctx,
            ir,
            base_type,
            method_name,
            type_args,
            method_type_args,
        )?
    };
    let Some(new_id) = new_id else {
        return Ok(());
    };
    let decl =
        c.ir.functions
            .get(&new_id)
            .expect("planner just inserted")
            .clone();
    emit_ir_impl_method(c, &decl)
}

/// Emits the LLVM struct type for a planned [`IRStruct`]. Stdlib
/// intrinsics (`List`, `Map`/`Set`, `Ref`, `ReplyTo`) defer to their
/// hard-coded backend layouts; user structs build their LLVM struct from
/// the resolved fields and register the matching `TypeLayouts` entry.
///
/// Idempotent on the LLVM cache: a no-op if the struct is already
/// registered (the IR planner has its own idempotency, but defensive
/// here in case of out-of-band emission).
pub(crate) fn emit_ir_struct<'ctx>(c: &mut Compiler<'ctx>, decl: &IRStruct) -> Result<(), String> {
    if c.llvm_types.contains_monomorphized(&decl.mangled) {
        return Ok(());
    }
    let mangled = decl.mangled.as_str();
    match decl.kind {
        IRStructKind::StdList => return monomorphize_list_struct(c, mangled),
        IRStructKind::StdHashtable => return monomorphize_hashtable_struct(c, mangled),
        IRStructKind::StdRef => return monomorphize_ref_struct(c, mangled),
        IRStructKind::StdReplyTo => return monomorphize_reply_to_struct(c, mangled),
        IRStructKind::User => {}
    }

    let st = c.context.opaque_struct_type(mangled);
    c.llvm_types
        .register_monomorphized(decl.mangled.clone(), st);

    let mut deferred_indirect = Vec::new();
    for (_, fty) in &decl.fields {
        if let Type::Indirect(inner) = fty {
            deferred_indirect.push(inner.as_ref().clone());
        } else {
            ensure_types_exist(c, fty)?;
        }
    }

    // `to_llvm_type` returns `None` for `Unit` and other ZSTs, but we must keep one
    // LLVM field per logical field so GEP indices match `TypeLayouts` (e.g.
    // `Pair<Unit, T>.second` is index 1, not 0 when `first` is Unit).
    let field_llvm_types: Vec<_> = decl
        .fields
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
        .register_struct_layout(decl.mangled.clone(), decl.fields.clone());

    Ok(())
}

/// Emits the LLVM tagged-union representation for a planned [`IREnum`]:
/// creates the opaque struct, ensures all variant payload types exist,
/// builds the union body via `build_enum_layout`, and registers the
/// canonical variant list with `TypeLayouts`.
pub(crate) fn emit_ir_enum<'ctx>(c: &mut Compiler<'ctx>, decl: &IREnum) -> Result<(), String> {
    if c.llvm_types.contains_monomorphized(&decl.mangled) {
        return Ok(());
    }

    let enum_type = c.context.opaque_struct_type(decl.mangled.as_str());
    c.llvm_types
        .register_monomorphized(decl.mangled.clone(), enum_type);

    for (_, vdata) in &decl.variants {
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

    build_enum_layout(c, decl.mangled.as_str(), enum_type, &decl.variants);

    c.layouts
        .register_enum_variants(decl.mangled.clone(), decl.variants.clone());

    Ok(())
}

/// Emits an LLVM free function from a planned [`IRFunction`]
/// (kind = `Free`): declares the function with the resolved
/// signature, binds parameter allocas, and compiles the body under
/// the kind's `subst`.
pub(crate) fn emit_ir_function<'ctx>(
    c: &mut Compiler<'ctx>,
    decl: &IRFunction,
) -> Result<(), String> {
    let IRFunctionKind::Free { func_ast, subst } = &decl.kind else {
        return Err(format!(
            "emit_ir_function called with non-free IRFunction `{}`",
            decl.mangled
        ));
    };

    // `decl` is already in `c.ir` (the planner inserted it before
    // calling us). The right "already emitted?" question is whether
    // the LLVM handle has been bound -- that's `c.functions`, populated
    // by the `c.functions.insert(...)` at the bottom of this function.
    if c.functions.contains_key(&decl.mangled) {
        return Ok(());
    }

    ensure_types_exist(c, &decl.return_type)?;
    for pt in &decl.param_types {
        ensure_types_exist(c, pt)?;
    }

    let llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = decl
        .param_types
        .iter()
        .filter_map(|ty| to_llvm_type(ty, c.context, &c.llvm_types))
        .map(|t| t.into())
        .collect();

    let fn_type = match to_llvm_type(&decl.return_type, c.context, &c.llvm_types) {
        Some(ret) => ret.fn_type(&llvm_param_types, false),
        None => c.context.void_type().fn_type(&llvm_param_types, false),
    };

    let mangled_str = decl.mangled.as_str();
    let fn_value = c.module.add_function(mangled_str, fn_type, None);
    // `decl` is already in `c.ir` (the planner inserted it before emission),
    // so this is a pure LLVM-handle binding, not a `register_function` site.
    c.functions.insert(decl.mangled.clone(), fn_value);

    let file = c.debug.file();
    c.debug.push_function(
        fn_value,
        &func_ast.name,
        mangled_str,
        file,
        func_ast.span.start.line,
    );

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
            let ty = &decl.param_types[i];
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
        &decl.return_type,
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

/// Emits an LLVM impl method from a planned [`IRFunction`] (kind =
/// `Method`): declares the function with `self` (when not static) plus
/// regular parameters, then defers to [`compile_method_body`] for body
/// emission.
pub(crate) fn emit_ir_impl_method<'ctx>(
    c: &mut Compiler<'ctx>,
    decl: &IRFunction,
) -> Result<(), String> {
    let IRFunctionKind::Method {
        func_ast,
        subst,
        base_type,
        mangled_type,
        self_type,
        is_static,
    } = &decl.kind
    else {
        return Err(format!(
            "emit_ir_impl_method called with non-method IRFunction `{}`",
            decl.mangled
        ));
    };

    // `decl` is already in `c.ir` (the planner inserted it before
    // calling us). The right "already emitted?" question is whether
    // the LLVM handle has been bound -- that's `c.functions`, populated
    // by the `c.functions.insert(...)` at the bottom of this function.
    if c.functions.contains_key(&decl.mangled) {
        return Ok(());
    }

    ensure_types_exist(c, &decl.return_type)?;
    for pt in &decl.param_types {
        ensure_types_exist(c, pt)?;
    }

    let mut llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = Vec::new();

    if let Some(self_expo_type) = self_type {
        let self_llvm_type = c
            .llvm_types
            .get_monomorphized(mangled_type)
            .map(|st| -> BasicTypeEnum { st.into() })
            .or_else(|| to_llvm_type(self_expo_type, c.context, &c.llvm_types))
            .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;
        llvm_param_types.push(self_llvm_type.into());
    }

    for ty in &decl.param_types {
        let lt = to_llvm_type(ty, c.context, &c.llvm_types).ok_or_else(|| {
            format!(
                "no LLVM type for method parameter type `{ty:?}` in `{}`",
                decl.mangled
            )
        })?;
        llvm_param_types.push(lt.into());
    }

    let fn_type = match to_llvm_type(&decl.return_type, c.context, &c.llvm_types) {
        Some(ret) => ret.fn_type(&llvm_param_types, false),
        None => c.context.void_type().fn_type(&llvm_param_types, false),
    };

    let fn_value = c.module.add_function(decl.mangled.as_str(), fn_type, None);
    // Pure LLVM-handle binding (the planner already inserted `decl` into
    // `c.ir`); not a `register_function` call site.
    c.functions.insert(decl.mangled.clone(), fn_value);

    let body_self_type = if *is_static {
        None
    } else {
        Some((mangled_type.as_str(), base_type.as_str()))
    };
    compile_method_body(
        c,
        fn_value,
        func_ast,
        body_self_type,
        &decl.param_types,
        &decl.return_type,
        subst.clone(),
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
