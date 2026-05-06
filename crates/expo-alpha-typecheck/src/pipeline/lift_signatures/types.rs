//! Type-expression resolution + small label/span helpers shared by
//! every other submodule under `lift_signatures/`.

use expo_ast::ast::{Diagnostic, TypeExpr};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::{Dispatch, GlobalRegistry};

/// Resolve a [`TypeExpr`] against the registry. Single-segment
/// `TypeExpr::Named` resolves either to a preloaded `Global.<name>`
/// stdlib stub or to a user struct registered earlier in
/// `package`. Everything else diagnoses and returns
/// [`ResolvedType::unresolved`] so the surrounding signature shape
/// (arity, param / field names) stays accurate.
pub(crate) fn resolve_type_expr(
    type_expr: &TypeExpr,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match type_expr {
        TypeExpr::Function { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support function-typed annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Generic { path, span, .. } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support generic type annotations (`{}`)",
                    path.join("."),
                ),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Named { path, span } => {
            resolve_named(path, *span, package, registry, diagnostics)
        }
        TypeExpr::Self_ { span } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support `Self` type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Union { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support union type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Unit { .. } => registry.primitive("Unit"),
    }
}

fn resolve_named(
    path: &[String],
    span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() != 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support dotted type names (`{}`)",
                path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let name = &path[0];
    // User-defined structs in the current package shadow stdlib
    // primitives by binding lookup order. The collect sub-pass has
    // already registered every user struct, so a same-package struct
    // entry takes precedence over a `Global.<name>` primitive.
    let local = Identifier::new(package, vec![name.clone()]);
    if let Some((id, _)) = registry.lookup(&local) {
        return ResolvedType::leaf(Resolution::Global(id));
    }
    let candidate = Identifier::new("Global", vec![name.clone()]);
    let Some((id, entry)) = registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the type name `{name}` (no \
                 same-package struct or `Global.*` primitive registered)",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    if !entry.identifier.is_in_global() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only recognizes `Global.*` primitive type names; \
                 got `{name}`",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    ResolvedType::leaf(Resolution::Global(id))
}

pub(super) fn type_expr_span(type_expr: &TypeExpr) -> Span {
    match type_expr {
        TypeExpr::Function { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Named { span, .. }
        | TypeExpr::Self_ { span }
        | TypeExpr::Union { span, .. }
        | TypeExpr::Unit { span } => *span,
    }
}

pub(super) fn impl_target_name(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].as_str()),
        _ => None,
    }
}

pub(super) fn dispatch_label(dispatch: Dispatch) -> &'static str {
    match dispatch {
        Dispatch::Instance => "instance method (with `self`)",
        Dispatch::Static => "static method (no `self`)",
    }
}

pub(super) fn render_resolved(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty.resolution {
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => "<unknown>".to_string(),
        },
        Resolution::Local(_) => "<local>".to_string(),
        Resolution::Unresolved => "<unresolved>".to_string(),
    }
}
