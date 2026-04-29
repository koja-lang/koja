//! Type-inference helpers shared by call-site resolvers.
//!
//! These functions support method/static-call lowering by inferring
//! argument types, unifying them with parameter signatures, and
//! resolving generic method/static type arguments. They are pure
//! semantic operations that take a [`LowerCtx`] (for `&TypeContext`,
//! `&FnLowerState`, etc.) plus a callback that bridges to the
//! LLVM-bound variables map on the codegen side.
//!
//! Lifted from `expo-codegen` in Wave 10 to break the residual
//! `<'ctx>`-coupling of method/static call resolution. See
//! `expo/design/archive/20260427-EXPOIR.md` for the Wave 10 narrative.

use std::collections::HashMap;

use expo_ast::ast::{Arg, BinOp, ClosureParam, Expr, ExprKind, Literal, TypeParam};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::{
    Primitive, Type, build_substitution, named_generic, substitute, unify,
};

use crate::lower::LowerCtx;
use crate::lower::closures::closure_info_at;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::{find_type_current, resolve_name_current, resolve_type_expr};

/// Infers the user-visible Expo type of a single argument expression
/// without compiling it. Used by argument-driven type-arg inference for
/// generic methods and static calls.
///
/// The caller supplies `var_type` to look up local-variable types from
/// codegen's `Compiler.fn_state.variables` (which carries LLVM allocas
/// alongside the `Type`); only the `Type` is used here.
///
/// Consults `expr.resolved_type` first when populated -- typecheck
/// records concrete types on literal/operator/call expressions there,
/// and ignoring it leaves user-defined-generics inference (`MyBox.new(42)`)
/// stuck on `Type::Unknown` for arguments the kind-specific arms below
/// don't recognise.
pub fn infer_arg_expo_type(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    expr: &Expr,
) -> Type {
    let kind_inferred = match &expr.kind {
        ExprKind::Ident { name, .. } => var_type(name).or_else(|| {
            let sig = ctx.type_ctx.function_sig(name)?;
            if sig.type_params.is_empty() {
                Some(Type::Function {
                    params: sig.params.iter().map(FnParam::from).collect(),
                    return_type: Box::new(sig.return_type.clone()),
                })
            } else {
                None
            }
        }),
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
                        Some(resolve_type_expr(ctx, te))
                    } else {
                        None
                    }
                })
                .collect();
            let ret = match return_type {
                Some(te) => resolve_type_expr(ctx, te),
                None => Type::Unit,
            };
            Some(Type::Function {
                params: param_types.into_iter().map(FnParam::borrow).collect(),
                return_type: Box::new(ret),
            })
        }
        ExprKind::ShortClosure { .. } => closure_info_at(ctx, expr.span).map(|ci| Type::Function {
            params: ci
                .param_types
                .iter()
                .map(|t| FnParam::borrow(t.clone()))
                .collect(),
            return_type: Box::new(ci.return_type.clone().unwrap_or(Type::Unit)),
        }),
        _ => None,
    };

    kind_inferred
        .filter(|t| *t != Type::Unknown)
        .or_else(|| expr.resolved_type.clone())
        .unwrap_or(Type::Unknown)
}

/// Expands a mangled monomorphized name (e.g. `Ref_$unit.Int$`) back to a
/// generic instance ([`Type::Named`] with non-empty `type_args`) so the
/// result can unify against unspecialized method signatures.
pub fn expand_mangled_arg_type(ctx: &LowerCtx<'_>, ty: &Type) -> Type {
    match ty {
        Type::Indirect(inner) => Type::Indirect(Box::new(expand_mangled_arg_type(ctx, inner))),
        Type::Pointer(inner) => Type::Pointer(Box::new(expand_mangled_arg_type(ctx, inner))),
        Type::Named {
            identifier,
            type_args: ta,
        } if ta.is_empty() => {
            if let Some((base, type_args)) = try_parse_mangled_name(ctx, &identifier.name) {
                named_generic(&base, type_args, ctx.type_ctx, ctx.package)
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
                    ty: expand_mangled_arg_type(ctx, &fp.ty),
                    mode: fp.mode,
                })
                .collect();
            let expanded_ret = expand_mangled_arg_type(ctx, return_type);
            Type::Function {
                params: expanded_params,
                return_type: Box::new(expanded_ret),
            }
        }
        _ => ty.clone(),
    }
}

/// Returns the method-level type parameters declared on
/// `<base_type>::<method>` (e.g. the `U` in `List<T>::map<U>`). Empty
/// when no method exists or it has no type parameters of its own.
pub fn lookup_method_type_params(
    ctx: &LowerCtx<'_>,
    base_type: &str,
    method: &str,
) -> Vec<TypeParam> {
    if let Some(ti) = find_type_current(ctx, base_type)
        && let Some(sig) = ti.functions.get(method)
    {
        return sig.type_params.clone();
    }
    Vec::new()
}

/// Infers concrete type arguments for a generic method's own type params
/// (e.g. `U` in `List<T>::map<U>(f: T -> U)`) by unifying call-site arg
/// types against the method's substituted parameter types.
///
/// Unresolved type parameters fall through as [`Type::Unknown`] in the
/// returned vector. `struct_type_args` are the type-args bound at the
/// receiver level (the `T` in `List<T>`); they're substituted into the
/// method signature before unification so call-site args agree with
/// the receiver instantiation.
pub fn infer_method_type_args(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    base_type: &str,
    method: &str,
    struct_type_args: &[Type],
    args: &[Arg],
) -> Result<Vec<Type>, String> {
    let (methods, type_params) = find_type_current(ctx, base_type)
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
        let arg_type =
            expand_mangled_arg_type(ctx, &infer_arg_expo_type(ctx, var_type, &arg.value));
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

/// Infers the type-args for a generic struct/enum static call (e.g.
/// `Task.async(f)` infers `T` from `f`'s type) by unifying argument
/// types against the static method's parameter signature.
///
/// Returns an error if any type parameter cannot be resolved (matches
/// the pre-lift behavior of failing fast at the call site rather than
/// silently using `Type::Unknown`).
pub fn infer_static_struct_type_args_from_args(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    type_name: &str,
    method: &str,
    args: &[Arg],
    type_params: &[TypeParam],
) -> Result<Vec<Type>, String> {
    if type_params.is_empty() {
        return Ok(vec![]);
    }
    let methods = find_type_current(ctx, type_name)
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
        let arg_ty = expand_mangled_arg_type(ctx, &infer_arg_expo_type(ctx, var_type, &arg.value));
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

/// Infers the return type of a static struct/enum method call (e.g.
/// `Task.async(...)`) for codegen variable typing when there is no
/// annotation. Returns `None` when the type or method is unknown, or
/// when type-arg inference fails.
pub fn infer_static_method_return_type(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    type_name: &str,
    method: &str,
    args: &[Arg],
) -> Option<Type> {
    let (methods, type_params) =
        find_type_current(ctx, type_name).map(|ti| (&ti.functions, &ti.type_params))?;
    let sig = methods.get(method)?;
    if type_params.is_empty() {
        return Some(sig.return_type.clone());
    }
    let inferred = infer_static_struct_type_args_from_args(
        ctx,
        var_type,
        type_name,
        method,
        args,
        type_params,
    )
    .ok()?;
    let subst = build_substitution(type_params, &inferred);
    Some(substitute(&sig.return_type, &subst))
}

/// Attempts to derive the Expo type directly from the assigned-RHS
/// expression. Used by [`crate::lower::statements`] when an
/// assignment lacks a type annotation and the compiled value's type
/// is [`Type::Unknown`].
///
/// Mirrors the legacy `expo-codegen::stmt::infer_type_from_expr`:
/// recognizes closure literals (so the variable carries a
/// [`Type::Function`] rather than the closure-fat-pointer LLVM
/// shape), bare function references, simple-call return types, and
/// the receive / concat / static-method cases that show up in
/// stdlib code today.
///
/// `var_type` looks up the local-binding type from `expo-codegen`'s
/// `Compiler.fn_state.variables` (which carries LLVM allocas the
/// LLVM-free side cannot reach into directly).
pub fn infer_type_from_expr(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    expr: &Expr,
) -> Option<Type> {
    if let Some(ty) = infer_method_call_type(ctx, var_type, expr) {
        return Some(ty);
    }
    if let Some(ty) = infer_closure_type(ctx, expr) {
        return Some(ty);
    }
    if let Some(ty) = infer_function_ref_type(ctx, expr) {
        return Some(ty);
    }
    if let Some(ty) = infer_call_return_type(ctx, expr) {
        return Some(ty);
    }
    if matches!(&expr.kind, ExprKind::Receive { .. }) {
        return ctx.fn_lower.process_msg_type.clone();
    }
    if let Some(ty) = infer_concat_type(ctx, var_type, expr) {
        return Some(ty);
    }
    None
}

/// Method-call inference: dispatches to either the static or
/// instance method return-type resolver depending on whether the
/// receiver `Ident` resolves to a type name (static) or a
/// primitive-typed local (instance). Mirrors the first branch of
/// the legacy `infer_type_from_expr`.
fn infer_method_call_type(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    expr: &Expr,
) -> Option<Type> {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        ..
    } = &expr.kind
    else {
        return None;
    };

    if let ExprKind::Ident { name, .. } = &receiver.kind {
        if resolve_name_current(ctx, name).is_some() {
            return infer_static_method_return_type(ctx, var_type, name, method, args);
        }
        if let Some(receiver_ty) = var_type(name)
            && matches!(receiver_ty, Type::Primitive(_))
            && let Some(ret) = infer_instance_method_return_type(ctx, &receiver_ty, method)
        {
            return Some(ret);
        }
    }

    let receiver_ty = infer_receiver_type(ctx, var_type, receiver)?;
    if matches!(receiver_ty, Type::Primitive(_)) {
        return infer_instance_method_return_type(ctx, &receiver_ty, method);
    }
    None
}

/// Closure-literal inference: builds a [`Type::Function`] from
/// declared parameter and return-type annotations. Untyped closure
/// parameters fall back to [`Primitive::I32`] (matches the legacy
/// `infer_type_from_expr` behavior).
fn infer_closure_type(ctx: &LowerCtx<'_>, expr: &Expr) -> Option<Type> {
    let ExprKind::Closure {
        params,
        return_type,
        ..
    } = &expr.kind
    else {
        return None;
    };
    let param_types: Vec<Type> = params
        .iter()
        .map(|p| match p {
            ClosureParam::Name {
                type_expr: Some(te),
                ..
            } => resolve_type_expr(ctx, te),
            _ => Type::Primitive(Primitive::I32),
        })
        .collect();
    let ret = match return_type {
        Some(te) => resolve_type_expr(ctx, te),
        None => Type::Unit,
    };
    Some(Type::Function {
        params: param_types.into_iter().map(FnParam::borrow).collect(),
        return_type: Box::new(ret),
    })
}

/// Bare-`Ident` inference: when the name resolves to a non-generic
/// top-level function, the value carries a [`Type::Function`] so
/// the receiving binding holds a callable.
fn infer_function_ref_type(ctx: &LowerCtx<'_>, expr: &Expr) -> Option<Type> {
    let ExprKind::Ident { name, .. } = &expr.kind else {
        return None;
    };
    let sig = ctx.type_ctx.function_sig(name)?;
    if !sig.type_params.is_empty() {
        return None;
    }
    Some(Type::Function {
        params: sig.params.iter().map(FnParam::from).collect(),
        return_type: Box::new(sig.return_type.clone()),
    })
}

/// Free-function call inference: when the callee is a non-generic
/// top-level `Ident`, the call's static return type is its
/// signature's return type.
fn infer_call_return_type(ctx: &LowerCtx<'_>, expr: &Expr) -> Option<Type> {
    let ExprKind::Call { callee, .. } = &expr.kind else {
        return None;
    };
    let ExprKind::Ident { name, .. } = &callee.kind else {
        return None;
    };
    let sig = ctx.type_ctx.function_sig(name)?;
    if !sig.type_params.is_empty() {
        return None;
    }
    Some(sig.return_type.clone())
}

/// `<>` concat inference: prefers the LHS's inferred type;
/// otherwise falls back to a bare-ident lookup or
/// [`Primitive::Binary`] for a binary literal LHS.
fn infer_concat_type(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    expr: &Expr,
) -> Option<Type> {
    let ExprKind::Binary {
        op: BinOp::Concat,
        left,
        ..
    } = &expr.kind
    else {
        return None;
    };
    if let Some(ty) = infer_type_from_expr(ctx, var_type, left) {
        return Some(ty);
    }
    match &left.kind {
        ExprKind::Ident { name, .. } => var_type(name),
        ExprKind::BinaryLiteral { .. } => Some(Type::Primitive(Primitive::Binary)),
        _ => None,
    }
}

/// Looks up the return type of an instance method on a given
/// receiver type. Handles primitives (looked up by display name)
/// and named generics (substituting type-args into the method's
/// signature).
pub fn infer_instance_method_return_type(
    ctx: &LowerCtx<'_>,
    receiver_type: &Type,
    method: &str,
) -> Option<Type> {
    match receiver_type {
        Type::Primitive(primitive) => ctx
            .type_ctx
            .find_type(primitive.display())
            .and_then(|info| info.functions.get(method))
            .map(|sig| sig.return_type.clone()),
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                return ctx
                    .type_ctx
                    .get_type(identifier)
                    .and_then(|info| info.functions.get(method))
                    .map(|sig| sig.return_type.clone());
            }
            let info = ctx.type_ctx.get_type(identifier)?;
            let sig = info.functions.get(method)?;
            let subst: HashMap<String, Type> = info
                .type_params
                .iter()
                .zip(type_args.iter())
                .map(|(tp, ta)| (tp.name.clone(), ta.clone()))
                .collect();
            Some(substitute(&sig.return_type, &subst))
        }
        _ => None,
    }
}

/// Infers the Expo type of a receiver expression without compiling
/// it. Mirrors the legacy `expo-codegen::stmt::infer_receiver_type`:
/// recognizes literal kinds, ident-typed locals, chained method
/// calls, and free-function call results.
pub fn infer_receiver_type(
    ctx: &LowerCtx<'_>,
    var_type: &dyn Fn(&str) -> Option<Type>,
    expr: &Expr,
) -> Option<Type> {
    match &expr.kind {
        ExprKind::Call { callee, .. } => {
            let ExprKind::Ident { name, .. } = &callee.kind else {
                return None;
            };
            ctx.type_ctx
                .functions
                .get(name)
                .map(|sig| sig.return_type.clone())
        }
        ExprKind::Ident { name, .. } => var_type(name),
        ExprKind::Literal { value, .. } => Some(literal_type(value)),
        ExprKind::MethodCall {
            receiver, method, ..
        } => {
            let receiver_ty = infer_receiver_type(ctx, var_type, receiver)?;
            infer_instance_method_return_type(ctx, &receiver_ty, method)
        }
        ExprKind::String { .. } => Some(Type::Primitive(Primitive::String)),
        _ => None,
    }
}

/// Maps an [`expo_ast::ast::Literal`] to its conventional Expo type
/// for receiver-type inference. `Int`s default to `I64`, `Float`s
/// to `F64`.
fn literal_type(value: &Literal) -> Type {
    match value {
        Literal::Bool(_) => Type::Primitive(Primitive::Bool),
        Literal::Float(_) => Type::Primitive(Primitive::F64),
        Literal::Int(_) => Type::Primitive(Primitive::I64),
        Literal::String(_) => Type::Primitive(Primitive::String),
        Literal::Unit => Type::Unit,
    }
}
