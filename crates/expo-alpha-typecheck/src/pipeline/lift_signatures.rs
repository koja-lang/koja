//! Lift-signatures sub-pass: resolve each function's param + return
//! `TypeExpr`s against the seeded [`GlobalRegistry`] and stamp a
//! [`FunctionSignature`] onto its registry entry. Mirror behavior
//! for struct decls — resolves each field's `TypeExpr` and stamps a
//! [`StructDefinition`] onto the [`GlobalKind::Struct`] entry.
//!
//! Runs after `collect` (so every function has a `Function(None)`
//! slot and every user struct has a `Struct(None)` slot) and before
//! `resolve` (so call sites + struct construction / field access
//! see callee signatures and field layouts).
//!
//! `TypeExpr::Named` resolves either against a preloaded stdlib
//! primitive (`Int`/`Bool`/`Unit`/`Float`/`String`) or against a
//! user struct registered earlier in the current package. Richer
//! shapes diagnose and stamp an `Unresolved` placeholder so the
//! signature / struct shape (arity, param / field names) stays
//! accurate downstream.

use expo_ast::ast::{Diagnostic, File, Function, Item, Param, PassMode, StructDecl, TypeExpr};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::{
    FunctionSignature, GlobalKind, GlobalRegistry, ResolvedParam, ResolvedStructField,
    StructDefinition,
};

pub(crate) fn lift_signatures(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &file.items {
        match item {
            Item::Function(function) => {
                lift_function(function, package, registry, diagnostics);
            }
            Item::Struct(decl) => {
                lift_struct(decl, package, registry, diagnostics);
            }
            _ => {}
        }
    }
}

fn lift_struct(
    decl: &StructDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let Some((id, entry)) = registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: struct `{identifier}` missing from registry — \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Struct(Some(_))) {
        // Duplicate decl is already diagnosed by `collect`; the
        // first one stamped its definition. Skip to avoid tripping
        // `set_struct_definition`'s panic-on-double-set invariant.
        return;
    }

    let mut fields = Vec::with_capacity(decl.fields.len());
    for field in &decl.fields {
        let ty = resolve_type_expr(&field.type_expr, package, registry, diagnostics);
        fields.push(ResolvedStructField {
            name: field.name.clone(),
            ty,
        });
    }
    registry.set_struct_definition(id, StructDefinition { fields });
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
                "alpha typecheck does not yet support generic functions (`{}` has type parameters)",
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
            package,
            registry,
            diagnostics,
        ));
    }

    let return_type = match function.return_type.as_ref() {
        Some(type_expr) => resolve_type_expr(type_expr, package, registry, diagnostics),
        None => registry.primitive("Unit"),
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
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedParam {
    match param {
        Param::Self_ { span, .. } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support `self` receivers (`{function_name}`)",
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
                        "alpha typecheck does not yet support `move` parameters \
                         (`{function_name}.{name}`)",
                    ),
                    *span,
                ));
            }
            if default.is_some() {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck does not yet support default parameter values \
                         (`{function_name}.{name}`)",
                    ),
                    *span,
                ));
            }
            let ty = resolve_type_expr(type_expr, package, registry, diagnostics);
            ResolvedParam {
                name: name.clone(),
                ty,
            }
        }
    }
}

/// Resolve a [`TypeExpr`] against the registry. Single-segment
/// `TypeExpr::Named` resolves either to a preloaded `Global.<name>`
/// stdlib stub or to a user struct registered earlier in
/// `package`. Everything else diagnoses and returns
/// [`ResolvedType::unresolved`] so the surrounding signature shape
/// (arity, param / field names) stays accurate.
pub(super) fn resolve_type_expr(
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
