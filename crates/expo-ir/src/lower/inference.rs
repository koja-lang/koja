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

use expo_ast::ast::{Arg, ClosureParam, Expr, ExprKind, TypeParam};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::{Type, build_substitution, named_generic, substitute, unify};

use crate::lower::LowerCtx;
use crate::lower::closures::closure_info_at;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::{find_type_current, resolve_type_expr};

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
