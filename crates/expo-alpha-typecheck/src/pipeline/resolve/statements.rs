//! Statement-level resolution helpers.
//!
//! Today this covers `Statement::Assignment` — declaration and
//! same-type reassignment. Compound assignment, multi-segment
//! [`LValue`] targets, and pattern destructuring all surface as
//! feature-gap diagnostics here so later passes never see those
//! shapes.
//!
//! Declaration vs. reassignment lives in [`resolve_assignment`]:
//!
//! - First write of a name: optional type annotation must match the
//!   rhs (or infer from rhs); insert into the scope; stamp the target
//!   `LValue`'s implied [`Resolution::Local`] via the AST `Expr`
//!   shape produced for `target` lookup.
//! - Subsequent write of an existing name: type annotation is a
//!   feature gap (only legal on first decl); rhs type must equal the
//!   existing local's type; the existing [`LocalId`] stays put.
//!
//! [`LocalId`]: expo_ast::identifier::LocalId
//! [`LValue`]: expo_ast::ast::LValue
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local

use expo_ast::ast::{AssignTarget, Diagnostic, Expr, LValue, TypeExpr};
use expo_ast::span::Span;

use crate::pipeline::lift_signatures::resolve_type_expr;

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::display_resolution;

/// Resolve a `target = value` statement. Validates target shape,
/// resolves the rhs, applies declaration-vs-reassignment rules, and
/// updates the resolver's scope accordingly.
pub(super) fn resolve_assignment(
    target: &mut AssignTarget,
    type_annotation: Option<&TypeExpr>,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    resolve_expr(value, resolver, diagnostics);

    let Some(name) = single_segment_target(target, span, diagnostics) else {
        return;
    };
    // Snapshot up front so we can stamp `target` after the resolver
    // mutably borrows scope below.
    let name = name.to_string();

    let value_ty = value.resolution.clone();
    let local_id = match resolver.scope.lookup(&name) {
        Some((existing_id, existing_ty)) => {
            let existing_ty = existing_ty.clone();
            if let Some(annotation) = type_annotation {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck only allows type annotations on the first declaration \
                         of a local (`{name}` was already declared)",
                    ),
                    annotation_span(annotation),
                ));
                return;
            }
            if !value_ty.is_resolved() {
                // The rhs already triggered its own diagnostic — stay
                // quiet to avoid piling on with a type-mismatch.
                return;
            }
            if value_ty != existing_ty {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "cannot reassign `{name}` from `{}` to `{}` — local types are fixed at \
                         declaration",
                        display_resolution(&existing_ty, resolver.registry),
                        display_resolution(&value_ty, resolver.registry),
                    ),
                    span,
                ));
            }
            existing_id
        }
        None => {
            let declared_ty = match type_annotation {
                Some(annotation) => {
                    let annotated = resolve_type_expr(
                        annotation,
                        resolver.package,
                        resolver.registry,
                        diagnostics,
                    );
                    if value_ty.is_resolved() && annotated.is_resolved() && value_ty != annotated {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "type annotation on `{name}` says `{}`, but the right-hand side \
                                 has type `{}`",
                                display_resolution(&annotated, resolver.registry),
                                display_resolution(&value_ty, resolver.registry),
                            ),
                            span,
                        ));
                    }
                    annotated
                }
                None => {
                    if !value_ty.is_resolved() {
                        // Without an annotation we can only infer
                        // from the rhs; if that failed, leave the
                        // local out of scope so later references
                        // diagnose as unknown rather than as a typed
                        // hole.
                        return;
                    }
                    value_ty
                }
            };
            resolver.scope.declare(&name, declared_ty)
        }
    };

    // Stamp the target so IR lower can read the LocalId without
    // re-walking scope. Single-segment LValue is the only shape that
    // reaches here (single_segment_target enforces it); multi-segment
    // forms diagnose and bail above.
    if let AssignTarget::LValue(lvalue) = target {
        lvalue.local_id = Some(local_id);
    }
}

/// Validate the assignment target shape. The slice supports only
/// single-segment [`LValue`]s (`x = ...`); pattern destructuring and
/// dotted lvalues (`point.x = ...`) surface as feature gaps. On
/// success, returns the local name.
fn single_segment_target<'a>(
    target: &'a AssignTarget,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'a str> {
    match target {
        AssignTarget::Pattern(_) => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support pattern destructuring assignment \
                 (`[a, b] = ...`)",
                span,
            ));
            None
        }
        AssignTarget::LValue(lvalue) => {
            if lvalue.segments.len() == 1 {
                Some(lvalue.segments[0].as_str())
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck does not yet support field assignment (got `{}`)",
                        format_lvalue(lvalue),
                    ),
                    lvalue.span,
                ));
                None
            }
        }
    }
}

fn format_lvalue(lvalue: &LValue) -> String {
    lvalue.segments.join(".")
}

fn annotation_span(annotation: &TypeExpr) -> Span {
    match annotation {
        TypeExpr::Function { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Named { span, .. }
        | TypeExpr::Self_ { span }
        | TypeExpr::Union { span, .. }
        | TypeExpr::Unit { span } => *span,
    }
}
