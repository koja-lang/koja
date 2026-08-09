//! Bare identifier, qualified member, and `self` resolution.

use koja_ast::ast::{Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{AnonymousKind, Identifier, LocalId, Resolution, ResolvedType};
use koja_ast::span::Span;

use crate::pipeline::visibility::check_reference_visibility;
use crate::registry::{FunctionSignature, GlobalKind};

use super::ctx::Resolver;
use super::paths::{PackageMember, lookup_package_member, static_dotted_path};

/// Resolve a bare identifier expression. Locals win first. Package-
/// level constants resolve through a global lookup so an
/// `EARTH_RADIUS` reference at a use site stamps `Resolution::Global`
/// and returns the constant's stamped type, with auto-imported
/// `Global` constants (`STDOUT`) as the fallback when the current
/// package has no match. Non-generic functions also resolve here as
/// first-class values: the bare name lifts to an
/// [`AnonymousKind::Function`] type so call-site code (the
/// fn-as-value adapter in IR lower) can wrap them in a closure value.
/// Generic functions diagnose, since first-class references would need an
/// inference site that doesn't exist for a bare ident. (The static-
/// method receiver and `Type.method(...)` call paths each handle
/// struct-name resolution directly so they don't go through this
/// helper.)
pub(super) fn resolve_ident(
    name: &str,
    resolution: &mut Resolution,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some((local_id, ty)) = resolver.scope.lookup(name) {
        *resolution = Resolution::Local(local_id);
        return ty.clone();
    }
    let global_id = Identifier::new(resolver.package, vec![name.to_string()]);
    if let Some((id, entry)) = resolver.registry.lookup(&global_id) {
        match &entry.kind {
            GlobalKind::Constant(Some(def)) => {
                *resolution = Resolution::Global(id);
                return def.ty.clone();
            }
            GlobalKind::Function(Some(sig)) => {
                let Some(ty) =
                    function_value_type(name, sig, &entry.type_params, span, diagnostics)
                else {
                    return ResolvedType::unresolved();
                };
                *resolution = Resolution::Global(id);
                return ty;
            }
            _ => {}
        }
    }
    let fallback = Identifier::new("Global", vec![name.to_string()]);
    if let Some((id, entry)) = resolver.registry.lookup(&fallback)
        && let GlobalKind::Constant(Some(def)) = &entry.kind
    {
        *resolution = Resolution::Global(id);
        return def.ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        format!("unknown identifier `{name}` in this scope"),
        span,
    ));
    ResolvedType::unresolved()
}

/// Resolve a package-qualified member read: a constant
/// (`Pkg.MAX_SIZE`) or a function value (`Pkg.helper`). The
/// capitalized form parses as a unit enum construction and the
/// lowercase form as a field access, so neither shape reaches
/// [`resolve_ident`]. When the dotted path names a constant or a
/// function, the node is rewritten to an identifier with a stamped
/// `Resolution::Global` so IR lowering reads it through the same
/// path as a bare name. Locals and types win over package prefixes
/// and bail to the normal resolution paths. Members are top-level
/// declarations, so only two-segment paths apply. Deeper chains like
/// `Pkg.ORIGIN.x` fall through and resolve their prefix recursively.
pub(super) fn resolve_qualified_member(
    expr: &mut Expr,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let path = static_dotted_path(&expr.kind)?;
    let [package, name] = path.as_slice() else {
        return None;
    };
    let (id, entry) = match lookup_package_member(package, name, resolver) {
        PackageMember::Found(id, entry) => (id, entry),
        PackageMember::NotAPackage => return None,
        PackageMember::UnknownMember => {
            diagnostics.push(Diagnostic::error(
                format!("package `{package}` has no constant or function `{name}`"),
                expr.span,
            ));
            return Some(ResolvedType::unresolved());
        }
    };
    let ty = match &entry.kind {
        GlobalKind::Constant(Some(definition)) => definition.ty.clone(),
        GlobalKind::Function(Some(signature)) => {
            let label = format!("{package}.{name}");
            let Some(ty) = function_value_type(
                &label,
                signature,
                &entry.type_params,
                expr.span,
                diagnostics,
            ) else {
                return Some(ResolvedType::unresolved());
            };
            ty
        }
        GlobalKind::Constant(None) | GlobalKind::Function(None) => panic!(
            "resolve_qualified_member: `{}` has no stamped definition, \
             lifting runs before body resolution",
            entry.identifier,
        ),
        other => {
            diagnostics.push(Diagnostic::error(
                format!("`{package}.{name}` is a {}, not a value", other.label()),
                expr.span,
            ));
            return Some(ResolvedType::unresolved());
        }
    };
    check_reference_visibility(entry, resolver.package, expr.span, diagnostics);
    expr.kind = ExprKind::Ident {
        name: path.join("."),
        resolution: Resolution::Global(id),
    };
    Some(ty)
}

/// Type a named function used as a first-class value. Generic
/// functions diagnose and return `None`, since a bare reference has
/// no inference site for the type args.
fn function_value_type(
    name: &str,
    signature: &FunctionSignature,
    type_params: &[String],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    if !type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot reference generic function `{name}` as a value \
                 (typecheck has no inference site for the type args)",
            ),
            span,
        ));
        return None;
    }
    Some(ResolvedType::Anonymous(AnonymousKind::Function {
        params: signature.params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(signature.return_type.clone()),
    }))
}

/// Resolve a `self` keyword expression. `self` is bound by the
/// enclosing instance method's `Param::Self_`, which the walker
/// seeds into the [`Resolver`]'s [`LocalScope`] under the name
/// `"self"`. A hit returns the receiver's struct type and stamps the
/// AST node's `local_id` slot so IR lower can read the slot through
/// the same `LocalRead` path body-declared locals use. A miss surfaces
/// as a diagnostic: `self` outside an instance method is invalid.
///
/// Note: `expr.resolution` keeps the receiver's *struct type* (not a
/// `Resolution::Local`). The `local_id` slot is the binding info,
/// the resolution slot is the static type. Same split as `ExprKind::Ident`,
/// where the inner `resolution` names the binding and the outer
/// `expr.resolution` carries the type.
///
/// [`LocalScope`]: crate::pipeline::local_scope::LocalScope
pub(super) fn resolve_self(
    local_id_slot: &mut Option<LocalId>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some((local_id, ty)) = resolver.scope.lookup("self") {
        *local_id_slot = Some(local_id);
        return ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        "`self` is only valid inside instance methods",
        span,
    ));
    ResolvedType::unresolved()
}
