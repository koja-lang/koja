//! Lift-signatures sub-pass: walk every `Item::Function` in each file,
//! resolve its param and return `TypeExpr`s against the already-seeded
//! [`GlobalRegistry`], and stamp a [`FunctionSignature`] onto each
//! function's registry entry.
//!
//! Runs **after** `collect` (so every top-level function already has
//! a `Function(None)` slot in the registry) and **before** `resolve`
//! (so `resolve_call` can look up a callee's signature without
//! re-walking the source).
//!
//! POC scope: only `TypeExpr::Named` pointing at a preloaded stdlib
//! primitive (`Int`, `Bool`, `Unit`, `Float`, `String`) resolves
//! successfully. Anything richer (generics, unions, function types,
//! user types) emits a diagnostic and stamps an `Unresolved`
//! placeholder so downstream resolve / seal still see a signature
//! shape of the right arity. Named-parameter defaults, `move`, and
//! `Self` receivers also diagnose.

use expo_ast::ast::{Diagnostic, File, Function, Item, Param, PassMode, TypeExpr};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::{FunctionSignature, GlobalKind, GlobalRegistry, ResolvedParam};

pub(crate) fn lift_signatures(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &file.items {
        if let Item::Function(function) = item {
            lift_function(function, package, registry, diagnostics);
        }
    }
}

fn lift_function(
    function: &Function,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !function.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck POC does not yet support generic functions (`{}` has type parameters)",
                function.name,
            ),
            function.span,
        ));
    }

    let mut params = Vec::with_capacity(function.params.len());
    for param in &function.params {
        params.push(lift_param(
            param,
            function.name.as_str(),
            registry,
            diagnostics,
        ));
    }

    let return_type = match function.return_type.as_ref() {
        Some(type_expr) => resolve_type_expr(type_expr, registry, diagnostics),
        None => primitive(registry, "Unit"),
    };

    let signature = FunctionSignature {
        params,
        return_type,
    };

    let identifier = Identifier::new(package, vec![function.name.clone()]);
    let Some((id, entry)) = registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: function `{identifier}` missing from registry — \
             collect invariant violation",
        );
    };
    // A duplicate function declaration in the same package is
    // already diagnosed by `collect`; the registry keeps the first
    // entry. If we still see a second `Item::Function` for this
    // identifier, its signature has already been stamped by the
    // first walk — skip to avoid tripping `set_signature`'s
    // panic-on-double-set invariant. The downstream diagnostic
    // surface stays the "already defined" message from collect.
    if matches!(entry.kind, GlobalKind::Function(Some(_))) {
        return;
    }
    registry.set_signature(id, signature);
}

fn lift_param(
    param: &Param,
    function_name: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedParam {
    match param {
        Param::Self_ { span, .. } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck POC does not yet support `self` receivers (`{function_name}`)",
                ),
                *span,
            ));
            ResolvedParam {
                name: "self".to_string(),
                ty: ResolvedType::unresolved(),
            }
        }
        Param::Regular {
            mode,
            name,
            type_expr,
            default,
            span,
        } => {
            if !matches!(mode, PassMode::Borrow) {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck POC does not yet support `move` parameters \
                         (`{function_name}.{name}`)",
                    ),
                    *span,
                ));
            }
            if default.is_some() {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck POC does not yet support default parameter values \
                         (`{function_name}.{name}`)",
                    ),
                    *span,
                ));
            }
            let ty = resolve_type_expr(type_expr, registry, diagnostics);
            ResolvedParam {
                name: name.clone(),
                ty,
            }
        }
    }
}

/// Resolve a [`TypeExpr`] against the registry. POC scope supports
/// only bare `TypeExpr::Named` with a single-segment path that
/// resolves to a preloaded `Global.<name>` stdlib stub. Everything
/// else diagnoses and returns [`ResolvedType::unresolved`] so the
/// signature shape (arity, param names) stays accurate for downstream
/// consumers.
fn resolve_type_expr(
    type_expr: &TypeExpr,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match type_expr {
        TypeExpr::Named { path, span } => resolve_named(path, *span, registry, diagnostics),
        TypeExpr::Unit { .. } => primitive(registry, "Unit"),
        TypeExpr::Generic { path, span, .. } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck POC does not yet support generic type annotations (`{}`)",
                    path.join("."),
                ),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Function { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck POC does not yet support function-typed annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Self_ { span } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck POC does not yet support `Self` type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Union { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck POC does not yet support union type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
    }
}

fn resolve_named(
    path: &[String],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() != 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck POC does not yet support dotted type names (`{}`)",
                path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let name = &path[0];
    let candidate = Identifier::new("Global", vec![name.clone()]);
    let Some((id, entry)) = registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck POC only recognizes primitive type names \
                 (`Int`, `Bool`, `Unit`, `Float`, `String`); got `{name}`",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    if !entry.identifier.is_in_global() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck POC only recognizes `Global.*` primitive type names; \
                 got `{name}`",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    ResolvedType::leaf(Resolution::Global(id))
}

fn primitive(registry: &GlobalRegistry, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = registry.lookup(&ident).unwrap_or_else(|| {
        panic!(
            "stdlib stub `Global.{name}` missing from registry — \
             alpha pipeline must seed it via `GlobalRegistry::with_stdlib_stubs`",
        )
    });
    ResolvedType::leaf(Resolution::Global(id))
}
