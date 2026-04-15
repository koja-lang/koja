//! Struct compilation: field access, struct construction (both regular and
//! generic), and method calls on struct instances.

use std::collections::HashMap;

use expo_ast::ast::PassMode;
use expo_ast::ast::{Arg, ClosureParam, Expr, ExprKind, FieldInit, TypeParam};
use expo_typecheck::context::{FnParam, FunctionKind, TypeInfo};
use expo_typecheck::types::{
    Package, Type, TypeIdentifier, build_substitution, mangle_name, named_generic,
    resolve_type_alias_id, resolve_type_alias_name, substitute, unify, unwrap_indirect,
};
use inkwell::AddressSpace;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue,
};

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
        let ptr_ty = c.context.ptr_type(AddressSpace::default());
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

fn llvm_type_size<'ctx>(ty: BasicTypeEnum<'ctx>, c: &Compiler<'ctx>) -> IntValue<'ctx> {
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

use crate::stmt::ResolvedFieldStep;

/// Resolved chain of field accesses from a base variable.
struct ResolvedChain {
    base_name: String,
    base_type: Type,
    steps: Vec<ResolvedFieldStep>,
}

/// Resolves a field access chain to a sequence of field indices and types
/// by walking the AST recursively. No LLVM emission.
fn resolve_chain_steps(compiler: &Compiler, receiver: &Expr, field: &str) -> Option<ResolvedChain> {
    let (base_name, base_type, mut steps) = match &receiver.kind {
        ExprKind::Ident { name, .. } => {
            let (_, ty, _) = compiler.fn_state.variables.get(name.as_str()).cloned()?;
            (name.clone(), ty, Vec::new())
        }
        ExprKind::Self_ => {
            let (_, ty, _) = compiler.fn_state.variables.get("self").cloned()?;
            ("self".to_string(), ty, Vec::new())
        }
        ExprKind::FieldAccess {
            receiver: inner_recv,
            field: inner_field,
            ..
        } => {
            let inner = resolve_chain_steps(compiler, inner_recv, inner_field)?;
            let last_type = inner
                .steps
                .last()
                .map(|s| &s.field_type)
                .unwrap_or(&inner.base_type);
            if matches!(last_type, Type::Indirect(_)) {
                return None;
            }
            (inner.base_name, inner.base_type, inner.steps)
        }
        _ => return None,
    };

    let current_type = steps.last().map(|s| &s.field_type).unwrap_or(&base_type);
    let sn = struct_name_from_type(current_type)?;
    let field_idx = compiler.get_field_index(&sn.mangled, field)?;
    let field_ty = compiler.get_field_type(&sn.mangled, field)?;

    steps.push(ResolvedFieldStep {
        field_index: field_idx,
        field_type: field_ty,
    });

    Some(ResolvedChain {
        base_name,
        base_type,
        steps,
    })
}

/// Tries to resolve a field access chain to a pointer via GEP without loading
/// intermediate struct values. Returns `(pointer, field_type)` on success.
/// Works for `ident.field`, `self.field`, and chains like `self.span.start`.
fn resolve_field_chain<'ctx>(
    compiler: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
) -> Option<(PointerValue<'ctx>, Type)> {
    let resolved = resolve_chain_steps(compiler, receiver, field)?;

    let (mut ptr, _, _) = compiler
        .fn_state
        .variables
        .get(&resolved.base_name)
        .cloned()?;
    let mut current_type = resolved.base_type;

    for step in &resolved.steps {
        let struct_type =
            to_llvm_type(&current_type, compiler.context, &compiler.types)?.into_struct_type();
        ptr = compiler
            .builder
            .build_struct_gep(struct_type, ptr, step.field_index, field)
            .unwrap();
        current_type = step.field_type.clone();
    }

    Some((ptr, current_type))
}

enum ResolvedFieldAccess<'ctx> {
    Chain {
        field_pointer: PointerValue<'ctx>,
        field_type: Type,
    },
    ValueStruct {
        field_index: u32,
        field_type: Type,
        struct_value: StructValue<'ctx>,
    },
}

fn resolve_field_access<'ctx>(
    compiler: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
    function: FunctionValue<'ctx>,
) -> Result<ResolvedFieldAccess<'ctx>, String> {
    if let Some((field_pointer, field_type)) = resolve_field_chain(compiler, receiver, field) {
        return Ok(ResolvedFieldAccess::Chain {
            field_pointer,
            field_type,
        });
    }

    let recv_val = compile_expr(compiler, receiver, function)?
        .ok_or("field access on expression that produced no value")?
        .value;

    if !recv_val.is_struct_value() {
        return Err("field access on non-struct value".to_string());
    }

    let struct_value = recv_val.into_struct_value();
    let struct_name = struct_value
        .get_type()
        .get_name()
        .map(|n| n.to_str().unwrap_or("").to_string())
        .ok_or("cannot determine struct type for field access")?;

    let field_index = compiler
        .get_field_index(&struct_name, field)
        .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

    let field_type = compiler
        .get_field_type(&struct_name, field)
        .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

    Ok(ResolvedFieldAccess::ValueStruct {
        field_index,
        field_type,
        struct_value,
    })
}

/// Compiles a field access expression (`receiver.field`). Uses direct GEP
/// chains for variable/self receivers and their nested field accesses,
/// falling back to a temporary alloca for arbitrary expression receivers.
pub fn compile_field_access<'ctx>(
    compiler: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_field_access(compiler, receiver, field, function)?;

    match resolved {
        ResolvedFieldAccess::Chain {
            field_pointer,
            field_type,
        } => {
            let val = load_maybe_indirect(compiler, field_pointer, &field_type, field);
            Ok(Some(TypedValue::new(
                val,
                unwrap_indirect(&field_type).clone(),
            )))
        }
        ResolvedFieldAccess::ValueStruct {
            field_index,
            field_type,
            struct_value,
        } => {
            let struct_llvm_type = struct_value.get_type();
            let tmp_alloca = compiler
                .builder
                .build_alloca(struct_llvm_type, "tmp_struct")
                .unwrap();
            compiler
                .builder
                .build_store(tmp_alloca, struct_value)
                .unwrap();

            let field_ptr = compiler
                .builder
                .build_struct_gep(struct_llvm_type, tmp_alloca, field_index, field)
                .unwrap();

            let val = load_maybe_indirect(compiler, field_ptr, &field_type, field);
            Ok(Some(TypedValue::new(
                val,
                unwrap_indirect(&field_type).clone(),
            )))
        }
    }
}

/// Compiles a method call (`receiver.method(args)`).
/// The resolved method call target. Captures everything needed to emit the
/// call without further type lookups.
struct ResolvedMethodCall<'ctx> {
    callee: FunctionValue<'ctx>,
    is_move: bool,
    mangled_name: String,
    param_types: Vec<Type>,
    return_type: Type,
}

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
        let resolved_id = resolve_type_alias_id(name, &c.type_ctx.type_aliases)
            .or_else(|| c.type_ctx.resolve_name(&resolved).cloned());
        if let Some(ref id) = resolved_id
            && c.type_ctx.get_type(id).is_some()
        {
            return compile_static_call(c, &resolved, Some(id), method, args, function);
        }
    }

    let recv_tv = compile_expr(c, receiver, function)?
        .ok_or("method call on expression that produced no value")?;
    let recv_val = recv_tv.value;

    if method == "clone" && args.is_empty() {
        return Ok(Some(recv_tv));
    }

    let resolved_name = resolve_struct_name(c, receiver, &recv_val, &recv_tv.expo_type)?;

    let has_impl_method = resolved_name
        .identifier
        .as_ref()
        .filter(|id| id.package != Package::Unresolved)
        .or_else(|| c.type_ctx.resolve_name(&resolved_name.base))
        .and_then(|id| c.type_ctx.get_type(id))
        .and_then(|ti| ti.functions.get(method))
        .is_some();
    if !has_impl_method && let Some(field_ty) = c.get_field_type(&resolved_name.mangled, method) {
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

    let resolved = resolve_method_call(
        c,
        &resolved_name.mangled,
        &resolved_name.base,
        resolved_name.identifier.as_ref(),
        &resolved_name.type_args,
        method,
        args,
    )?;

    let mut llvm_args: Vec<BasicMetadataValueEnum> = Vec::new();
    llvm_args.push(recv_val.into());

    for (i, arg) in args.iter().enumerate() {
        let val = if i < resolved.param_types.len() {
            let expected = &resolved.param_types[i];
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

    let is_tail = c
        .fn_state
        .tco
        .is_self_tail_call(&resolved.mangled_name, was_tail);

    if is_tail && let Some(loop_header) = c.fn_state.tco.loop_header {
        crate::drop::drop_live_variables(c, Some("self"));
        for (arg, alloca) in llvm_args.iter().zip(c.fn_state.tco.param_allocas.iter()) {
            let val: BasicValueEnum = (*arg).try_into().unwrap();
            c.builder.build_store(*alloca, val).unwrap();
        }
        c.builder.build_unconditional_branch(loop_header).unwrap();
        return Ok(None);
    }

    let result = c.call(
        resolved.callee,
        &llvm_args,
        &format!("{}_ret", resolved.mangled_name),
    );

    if resolved.is_move {
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

    Ok(result.map(|v| TypedValue::new(v, resolved.return_type)))
}

/// Resolves which method to call: computes the mangled name, triggers
/// monomorphization if needed, and looks up the param/return types.
fn resolve_method_call<'ctx>(
    c: &mut Compiler<'ctx>,
    struct_name: &str,
    base: &str,
    type_id: Option<&TypeIdentifier>,
    type_args: &[Type],
    method: &str,
    args: &[Arg],
) -> Result<ResolvedMethodCall<'ctx>, String> {
    let resolved_id = type_id
        .filter(|id| id.package != Package::Unresolved)
        .or_else(|| c.type_ctx.resolve_name(base));
    let is_generic = !type_args.is_empty();

    let mut mangled = format!("{}_{}", struct_name, method);
    let mut resolved_method_type_args: Vec<Type> = Vec::new();

    if is_generic {
        let method_type_params = lookup_method_type_params(c, base, method);

        if !method_type_params.is_empty() {
            let method_type_args = infer_method_type_args(c, base, method, type_args, args)?;
            resolved_method_type_args = method_type_args.clone();
            let method_suffix = mangle_name(method, &method_type_args);
            mangled = format!("{}_{}", struct_name, method_suffix);

            if !c.functions.contains_key(&mangled) {
                monomorphize_impl_method(c, base, method, type_args, &method_type_args)?;
            }
        } else if !c.functions.contains_key(&mangled) {
            monomorphize_impl_method(c, base, method, type_args, &[])?;
        }
    }

    let callee = *c
        .functions
        .get(&mangled)
        .ok_or_else(|| format!("undefined method `{method}` on `{struct_name}`"))?;

    let (param_types, return_type) = if let Some(sig) = c.type_ctx.functions.get(&mangled) {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else if is_generic
        && let Some(ti) = resolved_id.and_then(|id| c.type_ctx.get_type(id))
        && let Some(sig) = ti.functions.get(method)
    {
        let mut subst = build_substitution(&ti.type_params, type_args);
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
    } else if let Some(ti) = resolved_id.and_then(|id| c.type_ctx.get_type(id))
        && let Some(sig) = ti.functions.get(method)
    {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else if is_generic
        && let Some(spec_id) = resolved_id
        && let Some(entries) = c.type_ctx.specialized_methods.get(spec_id)
        && let Some((_, sigs)) = entries.iter().find(|(a, _)| *a == type_args)
        && let Some(sig) = sigs.get(method)
    {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else {
        (Vec::new(), Type::Unknown)
    };

    let is_move = resolved_id
        .and_then(|id| c.type_ctx.get_type(id))
        .and_then(|ti| ti.functions.get(method))
        .is_some_and(|sig| sig.kind == FunctionKind::Instance(PassMode::Move));

    Ok(ResolvedMethodCall {
        callee,
        is_move,
        mangled_name: mangled,
        param_types,
        return_type,
    })
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

struct ResolvedStructField {
    field_type: Type,
    index: u32,
    name: String,
}

struct ResolvedStructConstruction<'ctx> {
    fields: Vec<ResolvedStructField>,
    result_type: Type,
    struct_name: String,
    struct_type: StructType<'ctx>,
}

fn resolve_struct_construction<'ctx>(
    compiler: &Compiler<'ctx>,
    type_path: &[String],
    field_inits: &[FieldInit],
) -> Result<ResolvedStructConstruction<'ctx>, String> {
    let raw_name = type_path
        .first()
        .ok_or("empty type path in struct construction")?;
    let struct_name = resolve_type_alias_name(raw_name, &compiler.type_ctx.type_aliases);

    let struct_type = compiler
        .types
        .get_stdlib(&struct_name)
        .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

    let struct_info = compiler
        .type_ctx
        .find_type(&struct_name)
        .filter(|ti| ti.is_struct())
        .ok_or_else(|| format!("unknown struct: {struct_name}"))?;

    let struct_fields = struct_info
        .fields()
        .ok_or_else(|| format!("internal: `{struct_name}` is not a struct"))?;

    let mut fields = Vec::new();
    for field_init in field_inits {
        let (idx, field_type) = struct_fields
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
        fields.push(ResolvedStructField {
            field_type,
            index: idx,
            name: field_init.name.clone(),
        });
    }

    let result_type = Type::Named {
        identifier: TypeIdentifier::unresolved(&struct_name),
        type_args: vec![],
    };

    Ok(ResolvedStructConstruction {
        fields,
        result_type,
        struct_name,
        struct_type,
    })
}

/// Compiles a struct literal (`StructName { field: value, ... }`).
pub fn compile_struct_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    type_path: &[String],
    fields: &[FieldInit],
    resolved_type: Option<&TypeIdentifier>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let raw_name = type_path
        .first()
        .ok_or("empty type path in struct construction")?;
    let struct_name = resolve_type_alias_name(raw_name, &compiler.type_ctx.type_aliases);

    let type_info_lookup = resolved_type
        .filter(|id| id.package != Package::Unresolved)
        .and_then(|id| compiler.type_ctx.get_type(id))
        .or_else(|| {
            resolve_type_alias_id(raw_name, &compiler.type_ctx.type_aliases)
                .and_then(|id| compiler.type_ctx.get_type(&id))
        })
        .or_else(|| {
            compiler
                .type_ctx
                .resolve_name(&struct_name)
                .and_then(|id| compiler.type_ctx.get_type(id))
        });

    if let Some(info) = type_info_lookup
        && info.is_struct()
        && !info.type_params.is_empty()
    {
        return compile_generic_struct_construction(
            compiler,
            &struct_name,
            info.clone(),
            fields,
            function,
        );
    }

    let resolved = resolve_struct_construction(compiler, type_path, fields)?;
    let alloca = compiler.build_entry_alloca(
        resolved.struct_type,
        &format!("{}_tmp", resolved.struct_name),
    );

    for (resolved_field, field_init) in resolved.fields.iter().zip(fields.iter()) {
        let saved_subst = push_generic_type_subst(compiler, &resolved_field.field_type);

        let coerce_ty = unwrap_indirect(&resolved_field.field_type);
        let val = compile_expr_coerced(compiler, &field_init.value, coerce_ty, function)?
            .ok_or_else(|| format!("field `{}` produced no value", resolved_field.name))?;

        if let Some(saved) = saved_subst {
            compiler.fn_state.type_subst = saved;
        }

        let field_ptr = compiler
            .builder
            .build_struct_gep(
                resolved.struct_type,
                alloca,
                resolved_field.index,
                &resolved_field.name,
            )
            .unwrap();
        store_maybe_indirect(
            compiler,
            field_ptr,
            val,
            &resolved_field.field_type,
            &resolved_field.name,
        );
    }

    let struct_val = compiler
        .builder
        .build_load(resolved.struct_type, alloca, &resolved.struct_name)
        .unwrap();
    Ok(Some(TypedValue::new(struct_val, resolved.result_type)))
}

/// For generic struct literals, infer the field expression type for unification.
fn concrete_type_for_field_init<'ctx>(
    compiler: &Compiler<'ctx>,
    expr: &Expr,
    compiled_type: &Type,
) -> Type {
    if *compiled_type != Type::Unknown {
        return compiled_type.clone();
    }
    match &expr.kind {
        ExprKind::Ident { name, .. } => compiler
            .fn_state
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
            .unwrap_or(Type::Unknown),
        ExprKind::FieldAccess {
            receiver, field, ..
        } => {
            if let ExprKind::Ident { name, .. } = &receiver.as_ref().kind
                && let Some((_, recv_ty, _)) = compiler.fn_state.variables.get(name)
                && let Some(sn) = struct_name_from_type(recv_ty)
                && let Some(ft) = compiler.get_field_type(&sn.mangled, field)
            {
                substitute(&ft, &compiler.fn_state.type_subst)
            } else {
                Type::Unknown
            }
        }
        _ => Type::Unknown,
    }
}

struct ResolvedGenericStruct<'ctx> {
    mangled_name: String,
    result_type: Type,
    struct_type: StructType<'ctx>,
}

fn resolve_generic_struct<'ctx>(
    compiler: &mut Compiler<'ctx>,
    struct_name: &str,
    info: &TypeInfo,
    fields: &[FieldInit],
    compiled_fields: &[(String, BasicValueEnum<'ctx>, Type)],
) -> Result<ResolvedGenericStruct<'ctx>, String> {
    let struct_fields = info
        .fields()
        .ok_or_else(|| format!("internal: generic construction expected struct `{struct_name}`"))?;

    let mut subst = HashMap::new();
    for (i, (field_init_name, _field_val, compiled_type)) in compiled_fields.iter().enumerate() {
        if let Some((_, field_ty)) = struct_fields.iter().find(|(n, _)| n == field_init_name) {
            let concrete = concrete_type_for_field_init(compiler, &fields[i].value, compiled_type);
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

    let mangled_name = mangle_name(struct_name, &type_args);

    if !compiler.types.contains_monomorphized(&mangled_name) {
        monomorphize_struct(compiler, struct_name, &type_args)?;
    }

    let struct_type = compiler
        .types
        .get_monomorphized(&mangled_name)
        .ok_or_else(|| format!("monomorphized struct `{mangled_name}` not found"))?;

    let result_type = named_generic(struct_name, type_args, compiler.type_ctx);

    Ok(ResolvedGenericStruct {
        mangled_name,
        result_type,
        struct_type,
    })
}

fn compile_generic_struct_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    struct_name: &str,
    info: TypeInfo,
    fields: &[FieldInit],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let mut compiled_fields: Vec<(String, BasicValueEnum<'ctx>, Type)> = Vec::new();
    for field_init in fields {
        let tv = compile_expr(compiler, &field_init.value, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
        compiled_fields.push((field_init.name.clone(), tv.value, tv.expo_type));
    }

    let resolved = resolve_generic_struct(compiler, struct_name, &info, fields, &compiled_fields)?;

    let alloca = compiler.build_entry_alloca(
        resolved.struct_type,
        &format!("{}_tmp", resolved.mangled_name),
    );

    for (field_name, field_val, _) in &compiled_fields {
        let field_idx = compiler
            .get_field_index(&resolved.mangled_name, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` in struct `{struct_name}`"))?;
        let field_type = compiler.get_field_type(&resolved.mangled_name, field_name);
        let field_ptr = compiler
            .builder
            .build_struct_gep(resolved.struct_type, alloca, field_idx, field_name)
            .unwrap();
        if let Some(ref ft) = field_type {
            store_maybe_indirect(compiler, field_ptr, *field_val, ft, field_name);
        } else {
            compiler.builder.build_store(field_ptr, *field_val).unwrap();
        }
    }

    let struct_val = compiler
        .builder
        .build_load(resolved.struct_type, alloca, &resolved.mangled_name)
        .unwrap();
    Ok(Some(TypedValue::new(struct_val, resolved.result_type)))
}

/// The resolved struct name for a method call receiver, carrying the base
/// name, mangled name, and type args so callers never need to re-parse.
struct ResolvedStructName {
    base: String,
    identifier: Option<TypeIdentifier>,
    mangled: String,
    type_args: Vec<Type>,
}

fn resolve_struct_name<'ctx>(
    c: &Compiler<'ctx>,
    receiver: &Expr,
    recv_val: &BasicValueEnum<'ctx>,
    recv_type: &Type,
) -> Result<ResolvedStructName, String> {
    let mut result = None;

    if let Some(sn) = struct_name_from_type(recv_type) {
        result = Some(sn);
    } else if let ExprKind::Ident { name, .. } = &receiver.kind
        && let Some((_, ty, _)) = c.fn_state.variables.get(name)
        && let Some(sn) = struct_name_from_type(ty)
    {
        result = Some(sn);
    } else if recv_val.is_struct_value() {
        let sv = recv_val.into_struct_value();
        let st = sv.get_type();
        if let Some(n) = st.get_name()
            && let Ok(s) = n.to_str()
        {
            let name = s.to_string();
            let identifier = c.type_ctx.resolve_name(&name).cloned();
            result = Some(ResolvedStructName {
                base: name.clone(),
                identifier,
                mangled: name,
                type_args: vec![],
            });
        }
    }

    let mut sn = result.ok_or("cannot determine struct type for method call")?;

    if sn.type_args.is_empty()
        && let Some((base, type_args)) = try_parse_mangled_name(&sn.mangled, c)
    {
        sn.identifier = c.type_ctx.resolve_name(&base).cloned();
        sn.base = base;
        sn.type_args = type_args;
    }

    Ok(sn)
}

fn struct_name_from_type(ty: &Type) -> Option<ResolvedStructName> {
    match ty {
        Type::Indirect(inner) => struct_name_from_type(inner),
        Type::Pointer(inner) => {
            let base = "CPtr".to_string();
            let mangled = mangle_name(&base, &[*inner.clone()]);
            Some(ResolvedStructName {
                base,
                identifier: None,
                mangled,
                type_args: vec![*inner.clone()],
            })
        }
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Some(ResolvedStructName {
            base: identifier.name.clone(),
            identifier: Some(identifier.clone()),
            mangled: mangle_name(&identifier.name, type_args),
            type_args: type_args.clone(),
        }),
        Type::Named { identifier, .. } => Some(ResolvedStructName {
            base: identifier.name.clone(),
            identifier: Some(identifier.clone()),
            mangled: identifier.name.clone(),
            type_args: vec![],
        }),
        Type::Primitive(p) => {
            let name = p.display().to_string();
            Some(ResolvedStructName {
                base: name.clone(),
                identifier: None,
                mangled: name,
                type_args: vec![],
            })
        }
        _ => None,
    }
}

struct ResolvedStaticCall<'ctx> {
    callee: FunctionValue<'ctx>,
    mangled_name: String,
    param_types: Vec<Type>,
    return_type: Type,
}

fn resolve_static_call<'ctx>(
    c: &mut Compiler<'ctx>,
    type_name: &str,
    resolved_type: Option<&TypeIdentifier>,
    method: &str,
    args: &[Arg],
) -> Result<ResolvedStaticCall<'ctx>, String> {
    let resolved_id = resolved_type
        .filter(|id| id.package != Package::Unresolved)
        .or_else(|| c.type_ctx.resolve_name(type_name));
    let type_params = resolved_id
        .and_then(|id| c.type_ctx.get_type(id))
        .map(|ti| &ti.type_params);

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

    let mangled_name = format!("{}_{}", mangled_type, method);

    if !c.functions.contains_key(&mangled_name) {
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
        .get(&mangled_name)
        .ok_or_else(|| format!("undefined static function `{method}` on `{mangled_type}`"))?;

    let (param_types, return_type) = c
        .type_ctx
        .functions
        .get(&mangled_name)
        .map(|sig| {
            let pts: Vec<Type> = sig.params.iter().map(|p| p.ty.clone()).collect();
            (pts, sig.return_type.clone())
        })
        .or_else(|| {
            let ti = resolved_id.and_then(|id| c.type_ctx.get_type(id))?;
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

    Ok(ResolvedStaticCall {
        callee,
        mangled_name,
        param_types,
        return_type,
    })
}

fn compile_static_call<'ctx>(
    c: &mut Compiler<'ctx>,
    type_name: &str,
    resolved_type: Option<&TypeIdentifier>,
    method: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_static_call(c, type_name, resolved_type, method, args)?;

    let mut llvm_args: Vec<BasicMetadataValueEnum> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let val = if i < resolved.param_types.len() {
            compile_expr_coerced(c, &arg.value, &resolved.param_types[i], function)?
        } else {
            compile_expr(c, &arg.value, function)?.map(|tv| tv.value)
        }
        .ok_or_else(|| "static call argument produced no value".to_string())?;
        llvm_args.push(val.into());
    }

    Ok(c.call(
        resolved.callee,
        &llvm_args,
        &format!("{}_ret", resolved.mangled_name),
    )
    .map(|v| TypedValue::new(v, resolved.return_type)))
}
