//! Free-function lowering helpers for type and name resolution. Each
//! function takes a [`LowerCtx`] borrow bundle (or no context, for the
//! self-contained ones) and produces semantic results — no LLVM types,
//! no `Compiler` dependency.
//!
//! These were lifted off `Compiler` in Wave 6; their bodies are direct
//! translations of the original inherent methods.

use std::collections::BTreeSet;

use expo_ast::ast::TypeExpr;
use expo_ast::identifier::{Package, TypeIdentifier};
use expo_typecheck::context::TypeInfo;
use expo_typecheck::types::{Type, resolve_type_expr_full, substitute_preserving};

use crate::lower::ctx::LowerCtx;

/// Package-aware replacement for `type_ctx.find_type`. Use this anywhere
/// lowering looks up a type by bare name so user-defined types in the
/// current project's package are found alongside stdlib bare-entry types.
/// Plain `type_ctx.find_type` only consults the global bare index, which
/// silently skips user types registered under their package-qualified key.
pub fn find_type_current<'a>(ctx: &LowerCtx<'a>, name: &str) -> Option<&'a TypeInfo> {
    resolve_name_current(ctx, name).and_then(|id| ctx.type_ctx.get_type(id))
}

/// Returns a fully-resolved [`TypeIdentifier`] for `name`, preferring the
/// typecheck-supplied `resolved` identifier when it carries a real package,
/// and falling back to the package-aware bare-name resolver. This collapses
/// the recurring `resolved_type.filter(...).cloned().or_else(||
/// resolve_name_current(...).cloned())` pattern at enum/struct construction
/// sites.
pub fn id_for(
    ctx: &LowerCtx<'_>,
    name: &str,
    resolved: Option<&TypeIdentifier>,
) -> Option<TypeIdentifier> {
    resolved
        .filter(|id| id.package != Package::Unresolved)
        .cloned()
        .or_else(|| resolve_name_current(ctx, name).cloned())
}

/// Applies the surrounding function's type-parameter substitution -- and,
/// when inside an `impl` block, the `Self -> <self type>` binding -- to an
/// already-resolved [`Type`]. Use this on typecheck-supplied
/// `Expr::resolved_type` values that need to be interpreted in the current
/// monomorphization context (e.g. lowering match subjects whose type is
/// `Step<Self>` to the concrete `Step<MyProcess>`).
pub fn monomorphize_type(ctx: &LowerCtx<'_>, ty: &Type) -> Type {
    if let Some(name) = ctx.fn_lower.self_type_name.as_deref()
        && let Some(id) = resolve_name_current(ctx, name)
    {
        let self_ty = Type::Named {
            identifier: id.clone(),
            type_args: vec![],
        };
        let mut subst = ctx.fn_lower.type_subst.clone();
        subst.insert("Self".to_string(), self_ty);
        substitute_preserving(ty, &subst)
    } else {
        substitute_preserving(ty, &ctx.fn_lower.type_subst)
    }
}

/// Package-aware replacement for `type_ctx.resolve_name` that honours the
/// current package when set. Bare lookups resolve only within the current
/// package or to `std`; dependency types must be qualified or imported via
/// `alias` upstream.
pub fn resolve_name_current<'a>(ctx: &LowerCtx<'a>, name: &str) -> Option<&'a TypeIdentifier> {
    match ctx.package {
        Some(pkg) => ctx.type_ctx.resolve_name_scoped(name, pkg),
        None => ctx.type_ctx.resolve_name(name),
    }
}

/// Resolves a type expression AST node into an Expo type, using the
/// currently registered struct and enum names for lookup. When inside an
/// `impl` block (`fn_lower.self_type_name` is set), `Self` is automatically
/// substituted with the concrete target type.
pub fn resolve_type_expr(ctx: &LowerCtx<'_>, type_expr: &TypeExpr) -> Type {
    let struct_names: Vec<&str> = ctx
        .type_ctx
        .types
        .values()
        .filter(|ti| ti.is_struct())
        .map(|ti| ti.identifier.name.as_str())
        .collect();
    let enum_names: Vec<&str> = ctx
        .type_ctx
        .types
        .values()
        .filter(|ti| ti.is_enum())
        .map(|ti| ti.identifier.name.as_str())
        .collect();
    let mut type_params: Vec<&str> = ctx.fn_lower.type_subst.keys().map(|s| s.as_str()).collect();
    if ctx.fn_lower.self_type_name.is_some() && !type_params.contains(&"Self") {
        type_params.push("Self");
    }
    let known_packages: BTreeSet<Package> = ctx.type_ctx.package_types.keys().cloned().collect();
    let mut ty = resolve_type_expr_full(
        type_expr,
        &struct_names,
        &enum_names,
        &type_params,
        &ctx.type_ctx.type_aliases,
        &known_packages,
        &ctx.type_ctx.module_aliases,
    );
    match ctx.package {
        Some(pkg) => expo_typecheck::resolve::resolve_type_inline_scoped(
            &mut ty,
            &ctx.type_ctx.name_index,
            pkg,
        ),
        None => ctx.type_ctx.resolve_type(&mut ty),
    }
    if let Some(name) = ctx.fn_lower.self_type_name.as_deref() {
        let Some(id) = resolve_name_current(ctx, name) else {
            return substitute_preserving(&ty, &ctx.fn_lower.type_subst);
        };
        let self_ty = Type::Named {
            identifier: id.clone(),
            type_args: vec![],
        };
        let mut subst = ctx.fn_lower.type_subst.clone();
        subst.insert("Self".to_string(), self_ty);
        substitute_preserving(&ty, &subst)
    } else {
        substitute_preserving(&ty, &ctx.fn_lower.type_subst)
    }
}

/// Returns the bare type name from a single-segment `Named` type expression.
/// Returns `None` for multi-segment paths or non-`Named` expressions; the
/// caller is expected to consult `resolved_type` for those.
pub fn type_name_from_expr(te: &TypeExpr) -> Option<String> {
    if let TypeExpr::Named { path, .. } = te
        && path.len() == 1
    {
        return Some(path[0].clone());
    }
    None
}
