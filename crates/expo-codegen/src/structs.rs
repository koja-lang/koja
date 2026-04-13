//! Struct compilation: field access, struct construction (both regular and
//! generic), and method calls on struct instances.

use std::collections::HashMap;

use expo_ast::ast::PassMode;
use expo_ast::ast::{Arg, ClosureParam, Expr, ExprKind, FieldInit, TypeParam};
use expo_typecheck::context::{FnParam, FunctionKind, TypeInfo};
use expo_typecheck::types::{
    Type, TypeIdentifier, build_substitution, mangle_name, named_generic, resolve_type_alias_id,
    resolve_type_alias_name, substitute, unify, unwrap_indirect,
};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use crate::calls::invoke_closure_fat_ptr;
use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::generics::{
    ensure_types_exist, monomorphize_enum, monomorphize_impl_method, monomorphize_struct,
    try_parse_mangled_name,
};

use crate::types::to_llvm_type;

/// Loads a value from `field_ptr`. When `field_type` is [`Type::Indirect`],
/// loads the heap pointer first, then dereferences it to get the inner value.
pub(crate) fn load_maybe_indirect<'ctx>(
    c: &mut Compiler<'ctx>,
    field_ptr: PointerValue<'ctx>,
    field_type: &Type,
    label: &str,
) -> BasicValueEnum<'ctx> {
    if let Type::Indirect(inner) = field_type {
        let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
        let heap_ptr = c
            .builder
            .build_load(ptr_ty, field_ptr, &format!("{label}_ptr"))
            .unwrap()
            .into_pointer_value();
        let _ = ensure_types_exist(c, inner);
        let inner_llvm_ty = to_llvm_type(inner, c.context, &c.types)
            .expect("indirect inner type must have LLVM representation");
        c.builder
            .build_load(inner_llvm_ty, heap_ptr, &format!("{label}_deref"))
            .unwrap()
    } else {
        let llvm_ty = to_llvm_type(field_type, c.context, &c.types)
            .unwrap_or_else(|| c.context.i8_type().into());
        c.builder.build_load(llvm_ty, field_ptr, label).unwrap()
    }
}

/// Stores `val` into `field_ptr`. When `field_type` is [`Type::Indirect`],
/// heap-allocates storage via `malloc`, writes the value there, and stores the
/// resulting pointer into `field_ptr`.
pub(crate) fn store_maybe_indirect<'ctx>(
    c: &mut Compiler<'ctx>,
    field_ptr: PointerValue<'ctx>,
    val: BasicValueEnum<'ctx>,
    field_type: &Type,
    label: &str,
) {
    if let Type::Indirect(inner) = field_type {
        let _ = ensure_types_exist(c, inner);
        let inner_llvm_ty = to_llvm_type(inner, c.context, &c.types)
            .expect("indirect inner type must have LLVM representation");
        let size = llvm_type_size(inner_llvm_ty, c);
        let malloc_fn = *c.functions.get("malloc").expect("malloc not declared");
        let heap_ptr = c
            .call(malloc_fn, &[size.into()], &format!("{label}_malloc"))
            .unwrap()
            .into_pointer_value();
        c.builder.build_store(heap_ptr, val).unwrap();
        c.builder.build_store(field_ptr, heap_ptr).unwrap();
    } else {
        c.builder.build_store(field_ptr, val).unwrap();
    }
}

fn llvm_type_size<'ctx>(
    ty: BasicTypeEnum<'ctx>,
    c: &Compiler<'ctx>,
) -> inkwell::values::IntValue<'ctx> {
    match ty {
        BasicTypeEnum::StructType(st) => st
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::IntType(it) => it.size_of(),
        BasicTypeEnum::FloatType(ft) => ft.size_of(),
        BasicTypeEnum::PointerType(pt) => pt.size_of(),
        BasicTypeEnum::ArrayType(at) => at
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::VectorType(vt) => vt
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::ScalableVectorType(svt) => svt
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
    }
}

/// Tries to resolve a field access chain to a pointer via GEP without loading
/// intermediate struct values. Returns `(pointer, field_type)` on success.
/// Works for `ident.field`, `self.field`, and chains like `self.span.start`.
fn resolve_field_chain<'ctx>(
    c: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
) -> Option<(PointerValue<'ctx>, Type)> {
    let (base_ptr, base_struct_name, base_type) = match &receiver.kind {
        ExprKind::Ident { name, .. } => {
            let (ptr, ty, _) = c.fn_state.variables.get(name.as_str()).cloned()?;
            let sn = struct_name_from_type(&ty)?;
            (ptr, sn, ty)
        }
        ExprKind::Self_ => {
            let (ptr, ty, _) = c.fn_state.variables.get("self").cloned()?;
            let sn = struct_name_from_type(&ty)?;
            (ptr, sn, ty)
        }
        ExprKind::FieldAccess {
            receiver: inner_recv,
            field: inner_field,
            ..
        } => {
            let (inner_ptr, inner_ty) = resolve_field_chain(c, inner_recv, inner_field)?;
            if let Type::Indirect(_) = &inner_ty {
                return None;
            }
            let sn = struct_name_from_type(&inner_ty)?;
            (inner_ptr, sn, inner_ty)
        }
        _ => return None,
    };

    let struct_type = to_llvm_type(&base_type, c.context, &c.types)?.into_struct_type();
    let field_idx = c.get_field_index(&base_struct_name, field)?;
    let field_ty = c.get_field_type(&base_struct_name, field)?;

    let field_ptr = c
        .builder
        .build_struct_gep(struct_type, base_ptr, field_idx, field)
        .unwrap();

    Some((field_ptr, field_ty))
}

/// Compiles a field access expression (`receiver.field`). Uses direct GEP
/// chains for variable/self receivers and their nested field accesses,
/// falling back to a temporary alloca for arbitrary expression receivers.
pub fn compile_field_access<'ctx>(
    c: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if let Some((field_ptr, field_ty)) = resolve_field_chain(c, receiver, field) {
        let val = load_maybe_indirect(c, field_ptr, &field_ty, field);
        return Ok(Some(TypedValue::new(
            val,
            unwrap_indirect(&field_ty).clone(),
        )));
    }

    {
        let recv_val = compile_expr(c, receiver, function)?
            .ok_or("field access on expression that produced no value")?
            .value;

        if !recv_val.is_struct_value() {
            return Err("field access on non-struct value".to_string());
        }

        let sv = recv_val.into_struct_value();
        let struct_name = sv
            .get_type()
            .get_name()
            .map(|n| n.to_str().unwrap_or("").to_string())
            .ok_or("cannot determine struct type for field access")?;

        let struct_type = c
            .types
            .get_stdlib(&struct_name)
            .or_else(|| c.types.get_monomorphized(&struct_name))
            .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

        let field_idx = c
            .get_field_index(&struct_name, field)
            .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

        let field_ty = c
            .get_field_type(&struct_name, field)
            .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

        let tmp_alloca = c.builder.build_alloca(struct_type, "tmp_struct").unwrap();
        c.builder.build_store(tmp_alloca, recv_val).unwrap();

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, tmp_alloca, field_idx, field)
            .unwrap();

        let val = load_maybe_indirect(c, field_ptr, &field_ty, field);
        Ok(Some(TypedValue::new(
            val,
            unwrap_indirect(&field_ty).clone(),
        )))
    }
}

/// Compiles a method call (`receiver.method(args)`).
pub fn compile_method_call<'ctx>(
    c: &mut Compiler<'ctx>,
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let was_tail = c.fn_state.tco.save_tail();

    if let ExprKind::Ident { name, .. } = &receiver.kind {
        let resolved = resolve_type_alias_name(name, &c.type_ctx.type_aliases);
        if let Some(id) = resolve_type_alias_id(name, &c.type_ctx.type_aliases) {
            if c.type_ctx.get_type(&id).is_some() {
                return compile_static_call(c, &resolved, method, args, function);
            }
        } else if c.type_ctx.find_type(&resolved).is_some() {
            return compile_static_call(c, &resolved, method, args, function);
        }
    }

    let recv_tv = compile_expr(c, receiver, function)?
        .ok_or("method call on expression that produced no value")?;
    let recv_val = recv_tv.value;

    if method == "clone" && args.is_empty() {
        return Ok(Some(recv_tv));
    }

    let struct_name = resolve_struct_name(c, receiver, &recv_val, &recv_tv.expo_type)?;

    let base = try_parse_mangled_name(&struct_name, c)
        .map(|(b, _)| b)
        .unwrap_or_else(|| struct_name.clone());
    let has_impl_method = c
        .type_ctx
        .find_type(&base)
        .and_then(|ti| ti.functions.get(method))
        .is_some();
    if !has_impl_method && let Some(field_ty) = c.get_field_type(&struct_name, method) {
        let inner = unwrap_indirect(&field_ty);
        if let Type::Function {
            params,
            return_type,
        } = inner.clone()
        {
            let field_val = compile_field_access(c, receiver, method, function)?
                .ok_or_else(|| format!("field `{method}` produced no value"))?
                .value;
            let fat = field_val.into_struct_value();
            return invoke_closure_fat_ptr(
                c,
                fat,
                &params,
                return_type.as_ref(),
                args,
                function,
                &format!("field_{method}"),
            );
        }
    }

    let mut mangled = format!("{}_{}", struct_name, method);
    let mut resolved_method_type_args: Vec<Type> = Vec::new();

    if let Some((base, type_args)) = try_parse_mangled_name(&struct_name, c) {
        let method_type_params = lookup_method_type_params(c, &base, method);

        if !method_type_params.is_empty() {
            let method_type_args = infer_method_type_args(c, &base, method, &type_args, args)?;
            resolved_method_type_args = method_type_args.clone();
            let method_suffix = mangle_name(method, &method_type_args);
            mangled = format!("{}_{}", struct_name, method_suffix);

            if !c.functions.contains_key(&mangled) {
                monomorphize_impl_method(c, &base, method, &type_args, &method_type_args)?;
            }
        } else if !c.functions.contains_key(&mangled) {
            monomorphize_impl_method(c, &base, method, &type_args, &[])?;
        }
    }

    let callee = *c
        .functions
        .get(&mangled)
        .ok_or_else(|| format!("undefined method `{method}` on `{struct_name}`"))?;

    let (method_param_types, return_type) = if let Some(sig) = c.type_ctx.functions.get(&mangled) {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else if let Some((base_name, ta)) = try_parse_mangled_name(&struct_name, c)
        && let Some(ti) = c.type_ctx.find_type(&base_name)
        && let Some(sig) = ti.functions.get(method)
    {
        let mut subst = build_substitution(&ti.type_params, &ta);
        let method_tp = &sig.type_params;
        for (mp, ma) in method_tp.iter().zip(resolved_method_type_args.iter()) {
            subst.insert(mp.name.clone(), ma.clone());
        }
        (
            sig.params
                .iter()
                .map(|p| substitute(&p.ty, &subst))
                .collect(),
            substitute(&sig.return_type, &subst),
        )
    } else if let Some(ti) = c.type_ctx.find_type(&base)
        && let Some(sig) = ti.functions.get(method)
    {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else if let Some((base_name, ta)) = try_parse_mangled_name(&struct_name, c)
        && let Some(spec_id) = c.type_ctx.resolve_name(&base_name).cloned()
        && let Some(entries) = c.type_ctx.specialized_methods.get(&spec_id)
        && let Some((_, sigs)) = entries.iter().find(|(args, _)| *args == ta)
        && let Some(sig) = sigs.get(method)
    {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else {
        (Vec::new(), Type::Unknown)
    };

    let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
    llvm_args.push(recv_val.into());

    for (i, arg) in args.iter().enumerate() {
        let val = if i < method_param_types.len() {
            let expected = &method_param_types[i];
            if matches!(expected, Type::Unit) {
                c.context.i8_type().const_int(0, false).into()
            } else {
                compile_expr_coerced(c, &arg.value, expected, function)?
                    .ok_or_else(|| "method argument produced no value".to_string())?
            }
        } else {
            compile_expr(c, &arg.value, function)?
                .ok_or_else(|| "method argument produced no value".to_string())?
                .value
        };
        llvm_args.push(val.into());
    }

    c.fn_state.tco.restore_tail(was_tail);

    let is_tail = c.fn_state.tco.is_self_tail_call(&mangled, was_tail);

    if is_tail && let Some(loop_header) = c.fn_state.tco.loop_header {
        crate::drop::drop_live_variables(c, Some("self"));
        for (arg, alloca) in llvm_args.iter().zip(c.fn_state.tco.param_allocas.iter()) {
            let val: BasicValueEnum = (*arg).try_into().unwrap();
            c.builder.build_store(*alloca, val).unwrap();
        }
        c.builder.build_unconditional_branch(loop_header).unwrap();
        return Ok(None);
    }

    let result = c.call(callee, &llvm_args, &format!("{mangled}_ret"));

    if let Some(ti) = c.type_ctx.find_type(&base)
        && let Some(sig) = ti.functions.get(method)
        && sig.kind == FunctionKind::Instance(PassMode::Move)
    {
        let recv_name = match &receiver.kind {
            ExprKind::Ident { name, .. } => Some(name.as_str()),
            ExprKind::Self_ => Some("self"),
            _ => None,
        };
        if let Some(name) = recv_name
            && let Some((ptr, ty, _)) = c.fn_state.variables.get(name)
        {
            let entry = (*ptr, ty.clone(), crate::drop::Ownership::Unowned);
            c.fn_state.variables.insert(name.to_string(), entry);
        }
    }

    Ok(result.map(|v| TypedValue::new(v, return_type)))
}

fn lookup_method_type_params(c: &Compiler, base_type: &str, method: &str) -> Vec<TypeParam> {
    let methods = c.type_ctx.find_type(base_type).map(|ti| &ti.functions);
    if let Some(methods) = methods
        && let Some(sig) = methods.get(method)
    {
        return sig.type_params.clone();
    }
    Vec::new()
}

fn infer_method_type_args(
    c: &Compiler,
    base_type: &str,
    method: &str,
    struct_type_args: &[Type],
    args: &[Arg],
) -> Result<Vec<Type>, String> {
    let (methods, type_params) = c
        .type_ctx
        .find_type(base_type)
        .map(|ti| (&ti.functions, &ti.type_params))
        .ok_or_else(|| format!("no type info for `{base_type}`"))?;

    let sig = methods
        .get(method)
        .ok_or_else(|| format!("no method `{method}` on `{base_type}`"))?;

    let struct_subst = build_substitution(type_params, struct_type_args);
    let substituted_params: Vec<_> = sig
        .params
        .iter()
        .map(|p| substitute(&p.ty, &struct_subst))
        .collect();

    let mut method_subst = HashMap::new();
    for (i, arg) in args.iter().enumerate() {
        if i >= substituted_params.len() {
            break;
        }
        let arg_type = expand_mangled_arg_type(c, &infer_arg_expo_type(c, &arg.value));
        if arg_type != Type::Unknown {
            unify(&substituted_params[i], &arg_type, &mut method_subst);
        }
    }

    Ok(sig
        .type_params
        .iter()
        .map(|tp| method_subst.get(&tp.name).cloned().unwrap_or(Type::Unknown))
        .collect())
}

/// Expands a mangled monomorphized name (e.g. `Ref_$unit.Int$`) to [`Type::GenericInstance`]
/// so it can unify with generic method signatures.
fn expand_mangled_arg_type(c: &Compiler, ty: &Type) -> Type {
    match ty {
        Type::Indirect(inner) => Type::Indirect(Box::new(expand_mangled_arg_type(c, inner))),
        Type::Pointer(inner) => Type::Pointer(Box::new(expand_mangled_arg_type(c, inner))),
        Type::Named {
            identifier,
            type_args: ta,
        } if ta.is_empty() => {
            if let Some((base, type_args)) = try_parse_mangled_name(&identifier.name, c) {
                named_generic(&base, type_args, c.type_ctx)
            } else {
                ty.clone()
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            let expanded_params = params
                .iter()
                .map(|fp| FnParam {
                    ty: expand_mangled_arg_type(c, &fp.ty),
                    mode: fp.mode,
                })
                .collect();
            let expanded_ret = expand_mangled_arg_type(c, return_type);
            Type::Function {
                params: expanded_params,
                return_type: Box::new(expanded_ret),
            }
        }
        _ => ty.clone(),
    }
}

fn infer_static_struct_type_args_from_args(
    c: &Compiler,
    type_name: &str,
    method: &str,
    args: &[Arg],
    type_params: &[TypeParam],
) -> Result<Vec<Type>, String> {
    if type_params.is_empty() {
        return Ok(vec![]);
    }
    let methods = c
        .type_ctx
        .find_type(type_name)
        .map(|ti| &ti.functions)
        .ok_or_else(|| format!("unknown type `{type_name}`"))?;
    let sig = methods
        .get(method)
        .ok_or_else(|| format!("no method `{method}` on `{type_name}`"))?;
    let mut subst = HashMap::new();
    for (i, arg) in args.iter().enumerate() {
        if i >= sig.params.len() {
            break;
        }
        let arg_ty = expand_mangled_arg_type(c, &infer_arg_expo_type(c, &arg.value));
        if arg_ty != Type::Unknown && !unify(&sig.params[i].ty, &arg_ty, &mut subst) {
            return Err(format!(
                "argument `{}` to `{type_name}.{method}` does not match expected type",
                sig.params[i].name
            ));
        }
    }
    type_params
        .iter()
        .map(|tp| {
            subst.get(&tp.name).cloned().ok_or_else(|| {
                format!(
                    "cannot infer type parameter `{}` for `{type_name}.{method}`",
                    tp.name
                )
            })
        })
        .collect()
}

/// Infers the return type of a static struct/enum method call (e.g. `Task.async(...)`) for
/// codegen variable typing when there is no annotation.
pub fn infer_static_method_return_type(
    c: &Compiler,
    type_name: &str,
    method: &str,
    args: &[Arg],
) -> Option<Type> {
    let (methods, type_params) = c
        .type_ctx
        .find_type(type_name)
        .map(|ti| (&ti.functions, &ti.type_params))?;
    let sig = methods.get(method)?;
    if type_params.is_empty() {
        return Some(sig.return_type.clone());
    }
    let inferred =
        infer_static_struct_type_args_from_args(c, type_name, method, args, type_params).ok()?;
    let subst = build_substitution(type_params, &inferred);
    Some(substitute(&sig.return_type, &subst))
}

fn infer_arg_expo_type(c: &Compiler, expr: &Expr) -> Type {
    match &expr.kind {
        ExprKind::Ident { name, .. } => c
            .fn_state
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
            .or_else(|| {
                let sig = c.type_ctx.functions.get(name)?;
                if sig.type_params.is_empty() {
                    Some(Type::Function {
                        params: sig.params.iter().map(FnParam::from).collect(),
                        return_type: Box::new(sig.return_type.clone()),
                    })
                } else {
                    None
                }
            })
            .unwrap_or(Type::Unknown),
        ExprKind::Closure {
            params,
            return_type,
            ..
        } => {
            let param_types: Vec<Type> = params
                .iter()
                .filter_map(|p| {
                    if let ClosureParam::Name {
                        type_expr: Some(te),
                        ..
                    } = p
                    {
                        Some(c.resolve_type_expr(te))
                    } else {
                        None
                    }
                })
                .collect();
            let ret = match return_type {
                Some(te) => c.resolve_type_expr(te),
                None => Type::Unit,
            };
            Type::Function {
                params: param_types.into_iter().map(FnParam::borrow).collect(),
                return_type: Box::new(ret),
            }
        }
        ExprKind::ShortClosure { .. } => c
            .closure_info_at(expr.span)
            .map(|ci| Type::Function {
                params: ci
                    .param_types
                    .iter()
                    .map(|t| FnParam::borrow(t.clone()))
                    .collect(),
                return_type: Box::new(ci.return_type.clone().unwrap_or(Type::Unit)),
            })
            .unwrap_or(Type::Unknown),
        _ => Type::Unknown,
    }
}

/// Temporarily pushes type-parameter substitutions for a [`GenericInstance`]
/// field type so that empty collection literals (`[]`, `{}`) monomorphize to
/// the correct element type instead of falling back to `I32`.
fn push_generic_type_subst<'ctx>(
    c: &mut Compiler<'ctx>,
    field_type: &Type,
) -> Option<HashMap<String, Type>> {
    let ty = unwrap_indirect(field_type);
    if let Type::Named {
        identifier,
        type_args,
    } = ty
        && !type_args.is_empty()
    {
        let type_params = c
            .type_ctx
            .get_type(identifier)
            .map(|ti| ti.type_params.clone())?;
        let saved = c.fn_state.type_subst.clone();
        for (param, arg) in type_params.iter().zip(type_args.iter()) {
            let concrete = substitute(arg, &c.fn_state.type_subst);
            c.fn_state.type_subst.insert(param.name.clone(), concrete);
        }
        Some(saved)
    } else {
        None
    }
}

/// Compiles a struct literal (`StructName { field: value, ... }`).
pub fn compile_struct_construction<'ctx>(
    c: &mut Compiler<'ctx>,
    type_path: &[String],
    fields: &[FieldInit],
    resolved_type: Option<&TypeIdentifier>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let raw_name = type_path
        .first()
        .ok_or("empty type path in struct construction")?;
    let struct_name = &resolve_type_alias_name(raw_name, &c.type_ctx.type_aliases);

    let type_info_lookup = resolved_type
        .and_then(|id| c.type_ctx.get_type(id))
        .or_else(|| {
            resolve_type_alias_id(raw_name, &c.type_ctx.type_aliases)
                .and_then(|id| c.type_ctx.get_type(&id))
        })
        .or_else(|| c.type_ctx.find_type(struct_name));

    // For generic structs, compile field values first, infer type args, and monomorphize
    if let Some(info) = type_info_lookup
        && info.is_struct()
        && !info.type_params.is_empty()
    {
        return compile_generic_struct_construction(c, struct_name, info.clone(), fields, function);
    }

    let struct_type = c
        .types
        .get_stdlib(struct_name)
        .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

    let struct_info = c
        .type_ctx
        .find_type(struct_name)
        .filter(|ti| ti.is_struct())
        .ok_or_else(|| format!("unknown struct: {struct_name}"))?;

    let struct_fields = struct_info
        .fields()
        .ok_or_else(|| format!("internal: `{struct_name}` is not a struct"))?;

    let alloca = c.build_entry_alloca(struct_type, &format!("{struct_name}_tmp"));

    for field_init in fields {
        let (field_idx, field_type) = struct_fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == &field_init.name)
            .map(|(i, (_, ty))| (i as u32, ty.clone()))
            .ok_or_else(|| {
                format!(
                    "unknown field `{}` in struct `{}`",
                    field_init.name, struct_name
                )
            })?;

        let saved_subst = push_generic_type_subst(c, &field_type);

        let coerce_ty = unwrap_indirect(&field_type);
        let val = compile_expr_coerced(c, &field_init.value, coerce_ty, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;

        if let Some(saved) = saved_subst {
            c.fn_state.type_subst = saved;
        }

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, alloca, field_idx, &field_init.name)
            .unwrap();
        store_maybe_indirect(c, field_ptr, val, &field_type, &field_init.name);
    }

    let struct_val = c
        .builder
        .build_load(struct_type, alloca, struct_name)
        .unwrap();
    Ok(Some(TypedValue::new(
        struct_val,
        Type::Named {
            identifier: TypeIdentifier::unresolved(struct_name),
            type_args: vec![],
        },
    )))
}

/// For generic struct literals, infer the field expression type for unification.
fn concrete_type_for_field_init<'ctx>(
    c: &Compiler<'ctx>,
    expr: &Expr,
    compiled_type: &Type,
) -> Type {
    if *compiled_type != Type::Unknown {
        return compiled_type.clone();
    }
    match &expr.kind {
        ExprKind::Ident { name, .. } => c
            .fn_state
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
            .unwrap_or(Type::Unknown),
        ExprKind::FieldAccess {
            receiver, field, ..
        } => {
            if let ExprKind::Ident { name, .. } = &receiver.as_ref().kind
                && let Some((_, recv_ty, _)) = c.fn_state.variables.get(name)
                && let Some(sn) = struct_name_from_type(recv_ty)
                && let Some(ft) = c.get_field_type(&sn, field)
            {
                substitute(&ft, &c.fn_state.type_subst)
            } else {
                Type::Unknown
            }
        }
        _ => Type::Unknown,
    }
}

fn compile_generic_struct_construction<'ctx>(
    c: &mut Compiler<'ctx>,
    struct_name: &str,
    info: TypeInfo,
    fields: &[FieldInit],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let struct_fields = info
        .fields()
        .ok_or_else(|| format!("internal: generic construction expected struct `{struct_name}`"))?;
    let mut compiled_fields: Vec<(String, BasicValueEnum<'ctx>, Type)> = Vec::new();
    for field_init in fields {
        let tv = compile_expr(c, &field_init.value, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
        compiled_fields.push((field_init.name.clone(), tv.value, tv.expo_type));
    }

    let mut subst = HashMap::new();
    for (i, (field_init_name, _field_val, compiled_type)) in compiled_fields.iter().enumerate() {
        if let Some((_, field_ty)) = struct_fields.iter().find(|(n, _)| n == field_init_name) {
            let concrete = concrete_type_for_field_init(c, &fields[i].value, compiled_type);
            if !unify(field_ty, &concrete, &mut subst) {
                return Err(format!(
                    "type mismatch for field `{field_init_name}` in generic struct `{struct_name}`"
                ));
            }
        }
    }

    let type_args: Vec<Type> = info
        .type_params
        .iter()
        .map(|tp| subst.get(&tp.name).cloned().unwrap_or(Type::Unknown))
        .collect();

    let mangled = mangle_name(struct_name, &type_args);

    if !c.types.contains_monomorphized(&mangled) {
        monomorphize_struct(c, struct_name, &type_args)?;
    }

    let struct_type = c
        .types
        .get_monomorphized(&mangled)
        .ok_or_else(|| format!("monomorphized struct `{mangled}` not found"))?;

    let alloca = c.build_entry_alloca(struct_type, &format!("{mangled}_tmp"));

    for (field_name, field_val, _) in &compiled_fields {
        let field_idx = c
            .get_field_index(&mangled, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` in struct `{struct_name}`"))?;
        let field_type = c.get_field_type(&mangled, field_name);
        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, alloca, field_idx, field_name)
            .unwrap();
        if let Some(ref ft) = field_type {
            store_maybe_indirect(c, field_ptr, *field_val, ft, field_name);
        } else {
            c.builder.build_store(field_ptr, *field_val).unwrap();
        }
    }

    let struct_val = c.builder.build_load(struct_type, alloca, &mangled).unwrap();
    let result_type = named_generic(struct_name, type_args.clone(), c.type_ctx);
    Ok(Some(TypedValue::new(struct_val, result_type)))
}

fn resolve_struct_name<'ctx>(
    c: &Compiler<'ctx>,
    receiver: &Expr,
    recv_val: &BasicValueEnum<'ctx>,
    recv_type: &Type,
) -> Result<String, String> {
    if let Some(sn) = struct_name_from_type(recv_type) {
        return Ok(sn);
    }

    if let ExprKind::Ident { name, .. } = &receiver.kind
        && let Some((_, ty, _)) = c.fn_state.variables.get(name)
        && let Some(sn) = struct_name_from_type(ty)
    {
        return Ok(sn);
    }

    if recv_val.is_struct_value() {
        let sv = recv_val.into_struct_value();
        let st = sv.get_type();
        if let Some(n) = st.get_name()
            && let Ok(s) = n.to_str()
        {
            return Ok(s.to_string());
        }
    }

    Err("cannot determine struct type for method call".to_string())
}

fn struct_name_from_type(ty: &Type) -> Option<String> {
    match ty {
        Type::Indirect(inner) => struct_name_from_type(inner),
        Type::Pointer(inner) => Some(mangle_name("CPtr", &[*inner.clone()])),
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Some(mangle_name(&identifier.name, type_args)),
        Type::Named { identifier, .. } => Some(identifier.name.clone()),
        Type::Primitive(p) => Some(p.display().to_string()),
        _ => None,
    }
}

fn compile_static_call<'ctx>(
    c: &mut Compiler<'ctx>,
    type_name: &str,
    method: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let type_params = c.type_ctx.find_type(type_name).map(|ti| &ti.type_params);

    let mut type_args: Vec<Type> = if let Some(tp) = type_params
        && !tp.is_empty()
    {
        tp.iter()
            .filter_map(|param| c.fn_state.type_subst.get(&param.name).cloned())
            .collect()
    } else {
        Vec::new()
    };

    if let Some(tp) = type_params
        && !tp.is_empty()
        && type_args.len() != tp.len()
    {
        type_args = infer_static_struct_type_args_from_args(c, type_name, method, args, tp)?;
    }

    let mangled_type = if type_args.is_empty() {
        type_name.to_string()
    } else {
        let m = mangle_name(type_name, &type_args);
        if !c.types.contains_monomorphized(&m) {
            if c.type_ctx.is_struct(type_name) {
                monomorphize_struct(c, type_name, &type_args)?;
            } else {
                monomorphize_enum(c, type_name, &type_args)?;
            }
        }
        m
    };

    let mangled_fn = format!("{}_{}", mangled_type, method);

    if !c.functions.contains_key(&mangled_fn) {
        if !type_args.is_empty() {
            monomorphize_impl_method(c, type_name, method, &type_args, &[])?;
        } else {
            return Err(format!(
                "undefined static function `{method}` on `{type_name}`"
            ));
        }
    }

    let callee = *c
        .functions
        .get(&mangled_fn)
        .ok_or_else(|| format!("undefined static function `{method}` on `{mangled_type}`"))?;

    let (param_types, return_type) = c
        .type_ctx
        .functions
        .get(&mangled_fn)
        .map(|sig| {
            let pts: Vec<Type> = sig.params.iter().map(|p| p.ty.clone()).collect();
            (pts, sig.return_type.clone())
        })
        .or_else(|| {
            let ti = c.type_ctx.find_type(type_name)?;
            let sig = ti.functions.get(method)?;
            if !type_args.is_empty() {
                let subst = build_substitution(&ti.type_params, &type_args);
                let pts = sig
                    .params
                    .iter()
                    .map(|p| substitute(&p.ty, &subst))
                    .collect();
                Some((pts, substitute(&sig.return_type, &subst)))
            } else {
                let pts = sig.params.iter().map(|p| p.ty.clone()).collect();
                Some((pts, sig.return_type.clone()))
            }
        })
        .unwrap_or_else(|| (Vec::new(), Type::Unknown));

    let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let val = if i < param_types.len() {
            compile_expr_coerced(c, &arg.value, &param_types[i], function)?
        } else {
            compile_expr(c, &arg.value, function)?.map(|tv| tv.value)
        }
        .ok_or_else(|| "static call argument produced no value".to_string())?;
        llvm_args.push(val.into());
    }

    Ok(c.call(callee, &llvm_args, &format!("{mangled_fn}_ret"))
        .map(|v| TypedValue::new(v, return_type)))
}
