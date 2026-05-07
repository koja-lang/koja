//! Statement-level resolution helpers.
//!
//! Covers `Statement::Assignment` (declaration + same-type
//! reassignment) and `Statement::CompoundAssign` (`x += rhs`,
//! reassignment-only on an arithmetic local). Multi-segment
//! [`LValue`] targets and pattern destructuring still surface as
//! feature-gap diagnostics here so later passes never see those
//! shapes.
//!
//! Declaration vs. reassignment lives in [`resolve_assignment`]:
//!
//! - First write of a name: optional type annotation must match the
//!   rhs (or infer from rhs); insert into the scope; stamp the target
//!   `LValue`'s implied [`Resolution::Local`] via the AST `Expr`
//!   shape produced for `target` lookup. If the bare name matches a
//!   package-level [`crate::registry::GlobalKind::Constant`] entry,
//!   assignment is rejected — constants are immutable and cannot share
//!   an assignment LHS with locals.
//! - Subsequent write of an existing name: type annotation is a
//!   feature gap (only legal on first decl); rhs type must equal the
//!   existing local's type; the existing [`LocalId`] stays put.
//!
//! [`resolve_compound_assignment`] is the same shape minus the
//! declaration path: the local must already exist, its type must
//! be `Int` or `Float`, and the rhs type must match.
//!
//! [`LocalId`]: expo_ast::identifier::LocalId
//! [`LValue`]: expo_ast::ast::LValue
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local

use expo_ast::ast::{AssignTarget, CompoundOp, Diagnostic, Expr, LValue, TypeExpr};
use expo_ast::identifier::Identifier;
use expo_ast::labels::compound_op_label;
use expo_ast::span::Span;

use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::registry::GlobalKind;

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::{display_resolution, is_arithmetic_type};

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
            if assigns_to_package_constant(&name, resolver) {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "cannot assign to `{name}` — package-level constants are immutable and \
                         cannot be reassigned like a local",
                    ),
                    span,
                ));
                return;
            }
            let declared_ty = match type_annotation {
                Some(annotation) => {
                    let annotated = resolve_type_expr(
                        annotation,
                        TypeParamScope::default(),
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

/// Resolve a `target op= value` statement. Reassignment-only — the
/// target must already be a declared local of arithmetic type
/// (`Int` or `Float`), and the rhs must match. On success, stamps
/// `target.local_id` so IR lower can desugar to
/// `LocalRead + BinaryOp + LocalWrite`.
pub(super) fn resolve_compound_assignment(
    target: &mut LValue,
    op: CompoundOp,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    resolve_expr(value, resolver, diagnostics);

    let Some(name) = single_segment_lvalue(target, diagnostics) else {
        return;
    };
    let name = name.to_string();
    let op_label = compound_op_label(op);

    let Some((local_id, existing_ty)) = resolver
        .scope
        .lookup(&name)
        .map(|(id, ty)| (id, ty.clone()))
    else {
        if assigns_to_package_constant(&name, resolver) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot apply `{op_label}=` to `{name}` — package-level constants are \
                     immutable",
                ),
                span,
            ));
        } else {
            diagnostics.push(Diagnostic::error(
                format!("cannot apply `{op_label}=` to undeclared variable `{name}`"),
                span,
            ));
        }
        return;
    };

    if !is_arithmetic_type(&existing_ty, resolver.registry) {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{op_label}=` requires an `Int` or `Float` lhs (got `{}` for `{name}`)",
                display_resolution(&existing_ty, resolver.registry),
            ),
            span,
        ));
        return;
    }

    let value_ty = value.resolution.clone();
    if !value_ty.is_resolved() {
        // The rhs already triggered its own diagnostic — stay quiet
        // to avoid piling on with a type-mismatch.
        return;
    }
    if value_ty != existing_ty {
        diagnostics.push(Diagnostic::error(
            format!(
                "type mismatch on `{op_label}=` for `{name}`: lhs is `{}`, rhs is `{}`",
                display_resolution(&existing_ty, resolver.registry),
                display_resolution(&value_ty, resolver.registry),
            ),
            span,
        ));
        return;
    }

    target.local_id = Some(local_id);
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
        AssignTarget::LValue(lvalue) => single_segment_lvalue(lvalue, diagnostics),
    }
}

/// Validate a bare [`LValue`] target (used by compound assignment,
/// where the AST already pinned the shape to `LValue`). Multi-segment
/// targets surface as the same field-assignment feature gap as the
/// `single_segment_target` LValue arm; pattern destructuring is
/// unreachable here because the parser only allows an `LValue` on
/// the lhs of `+=` / `-=` / `*=` / `/=`.
fn single_segment_lvalue<'a>(
    lvalue: &'a LValue,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'a str> {
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

/// True when `name` is a package-level constant in the resolver's
/// package (same namespace as locals for single-segment assignment
/// targets).
fn assigns_to_package_constant(name: &str, resolver: &Resolver<'_>) -> bool {
    let identifier = Identifier::new(resolver.package, vec![name.to_string()]);
    resolver
        .registry
        .lookup(&identifier)
        .is_some_and(|(_, entry)| matches!(entry.kind, GlobalKind::Constant(_)))
}
