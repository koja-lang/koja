//! Statement-level resolution helpers.
//!
//! Covers `Statement::Assignment` (declaration + same-type
//! reassignment + multi-segment field write) and
//! `Statement::CompoundAssign` (`x += rhs`, reassignment-only on
//! an arithmetic local; supports field-path targets like `p.x += 1`).
//! Pattern destructuring still surfaces as a feature-gap diagnostic
//! here so later passes never see that shape.
//!
//! Declaration vs. reassignment vs. field write lives in
//! [`resolve_assignment`]:
//!
//! - **Declaration / reassignment** (`segments.len() == 1`):
//!   - First write of a name: optional type annotation must match the
//!     rhs (or infer from rhs); insert into the scope; stamp the
//!     target `LValue`'s implied [`Resolution::Local`] via the AST
//!     `Expr` shape produced for `target` lookup. If the bare name
//!     matches a package-level
//!     [`crate::registry::GlobalKind::Constant`] entry, assignment
//!     is rejected — constants are immutable and cannot share an
//!     assignment LHS with locals.
//!   - Subsequent write of an existing name: type annotation is a
//!     feature gap (only legal on first decl); rhs type must equal
//!     the existing local's type; the existing [`LocalId`] stays put.
//!
//! - **Field write** (`segments.len() >= 2`): the head segment must
//!   resolve to a declared local and (when it is `self`) the
//!   enclosing fn must have `move self`. Each subsequent segment
//!   projects through a struct definition's field roster, applying
//!   the receiver's type-args at every step (so `self.entries` on
//!   `Headers { entries: List<Header> }` types as `List<Header>`
//!   rather than the raw declared type-param). The rhs validates
//!   against the leaf field type. Type annotations are a feature
//!   gap on field writes (`x: T = …` makes no sense once `x` is a
//!   field path).
//!
//! [`resolve_compound_assignment`] is the same shape minus the
//! declaration path: the head local must already exist, the leaf
//! field type must be `Int` or `Float`, and the rhs type must match.
//!
//! [`LocalId`]: expo_ast::identifier::LocalId
//! [`LValue`]: expo_ast::ast::LValue
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local

use expo_ast::ast::{AssignTarget, CompoundOp, Diagnostic, Expr, LValue, PassMode, TypeExpr};
use expo_ast::identifier::{Identifier, LocalId, Resolution, ResolvedType};
use expo_ast::labels::compound_op_label;
use expo_ast::span::Span;

use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::GlobalKind;

use super::coercion::{Compatible, check_compatible, coercion_annotation_mut};
use super::ctx::Resolver;
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::idents::diagnose_if_moved;
use super::moves::move_source_local;
use super::types::{display_resolution, is_arithmetic_type};
use expo_ast::coercion::Coercion;

/// Resolve a `target = value` statement. Validates target shape,
/// resolves the rhs, applies declaration-vs-reassignment rules, and
/// updates the resolver's scope accordingly. Multi-segment field
/// writes route through [`resolve_field_assignment`].
pub(super) fn resolve_assignment(
    target: &mut AssignTarget,
    type_annotation: Option<&TypeExpr>,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let lvalue = match target {
        AssignTarget::LValue(lvalue) => lvalue,
        AssignTarget::Pattern(_) => {
            diagnostics.push(Diagnostic::error(
                "typecheck does not yet support pattern destructuring assignment \
                 (`[a, b] = ...`)",
                span,
            ));
            return;
        }
    };

    if lvalue.segments.len() >= 2 {
        if let Some(annotation) = type_annotation {
            diagnostics.push(Diagnostic::error(
                format!(
                    "typecheck does not allow type annotations on field-write \
                     targets (got `{}: …`)",
                    format_lvalue(lvalue),
                ),
                annotation_span(annotation),
            ));
        }
        resolve_field_assignment(lvalue, value, span, resolver, diagnostics);
        return;
    }

    // Resolve the annotation up front so it can flow into the rhs as
    // an expected type. Bidirectional inference uses this to drive
    // shapes like `result: List<T> = List.new()` — the annotation's
    // `T` constrains `List.new`'s otherwise-unconstrained type param.
    let expected_ty: Option<ResolvedType> = type_annotation.and_then(|annotation| {
        let resolved = resolve_type_expr(
            annotation,
            TypeParamScope::new(resolver.type_param_owners),
            resolver.resolution_scope(),
            diagnostics,
        );
        resolved.is_resolved().then_some(resolved)
    });
    resolve_expr_with_expected(value, expected_ty.as_ref(), resolver, diagnostics);

    // The RHS expression has been resolved; if it bottoms out at a
    // bare-ident read of a non-`Copy` local, the assignment moves
    // that local. Mark it before recording the LHS write so a
    // self-aliasing case like `x = x` stamps `x` as moved first and
    // then clears it as the LHS write resets the slot.
    if let Some(source_local) = move_source_local(value, resolver) {
        resolver.moves.mark_moved(source_local, value.span);
    }

    let name = lvalue.segments[0].clone();

    let value_ty = value.resolution.clone();
    let local_id = match resolver.scope.lookup(&name) {
        Some((existing_id, existing_ty)) => {
            let existing_ty = existing_ty.clone();
            if let Some(annotation) = type_annotation {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "typecheck only allows type annotations on the first declaration \
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
            let declared_ty = match expected_ty {
                Some(annotated) => {
                    if value_ty.is_resolved() {
                        match check_compatible(value, &value_ty, &annotated, resolver.registry) {
                            Compatible::Strict | Compatible::Coerced(_) => {}
                            Compatible::UnionWiden { target } => {
                                *coercion_annotation_mut(value) =
                                    Some(Coercion::UnionWiden(target));
                            }
                            Compatible::Incompatible | Compatible::OutOfRange { .. } => {
                                diagnostics.push(Diagnostic::error(
                                    format!(
                                        "type annotation on `{name}` says `{}`, but the \
                                         right-hand side has type `{}`",
                                        display_resolution(&annotated, resolver.registry),
                                        display_resolution(&value_ty, resolver.registry),
                                    ),
                                    span,
                                ));
                            }
                        }
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
    // re-walking scope. Single-segment is the only shape that reaches
    // here — multi-segment field writes routed to
    // `resolve_field_assignment` above and bailed early.
    lvalue.local_id = Some(local_id);

    // Fresh write resets the slot's move state: reassigning a
    // previously-moved name (`x = ...; consume(x); x = ...`) makes
    // the slot live again.
    resolver.moves.clear(local_id);
}

/// Resolve a `target op= value` statement. Reassignment-only — the
/// target (or its leaf field, on a multi-segment path) must already
/// resolve to an arithmetic type (`Int` or `Float`), and the rhs
/// must match. On success, stamps `target.local_id` (the head local)
/// so IR lower can desugar to `LocalRead + (FieldGet*) + BinaryOp +
/// (FieldSet*) + LocalWrite`.
pub(super) fn resolve_compound_assignment(
    target: &mut LValue,
    op: CompoundOp,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    resolve_expr(value, resolver, diagnostics);

    let op_label = compound_op_label(op);
    let name = target.segments[0].clone();

    let Some(head) = resolve_head_local(&name, target.span, resolver, diagnostics) else {
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
    diagnose_if_moved(&name, head.local_id, target.span, resolver, diagnostics);

    let leaf_ty = if target.segments.len() == 1 {
        head.ty
    } else {
        if !require_self_mutable(&name, target.span, resolver, diagnostics) {
            return;
        }
        let Some(leaf) = walk_field_segments(&head.ty, target, resolver, diagnostics) else {
            return;
        };
        target.head_resolved_type = Some(head.ty);
        leaf
    };

    if !is_arithmetic_type(&leaf_ty, resolver.registry) {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{op_label}=` requires an `Int` or `Float` lhs (got `{}` for `{}`)",
                display_resolution(&leaf_ty, resolver.registry),
                format_lvalue(target),
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
    if value_ty != leaf_ty {
        diagnostics.push(Diagnostic::error(
            format!(
                "type mismatch on `{op_label}=` for `{}`: lhs is `{}`, rhs is `{}`",
                format_lvalue(target),
                display_resolution(&leaf_ty, resolver.registry),
                display_resolution(&value_ty, resolver.registry),
            ),
            span,
        ));
        return;
    }

    target.local_id = Some(head.local_id);
}

/// Resolve a multi-segment `local.field1.field2 = value` assignment.
/// The head segment must name a declared local (and must be `move
/// self` when it is `self`); each subsequent segment projects through
/// a struct field roster while substituting the receiver's type-args
/// at every step. On success, the rhs validates against the leaf
/// field type and the head local's `LocalId` is stamped on
/// `lvalue.local_id` so IR lower can find the slot.
fn resolve_field_assignment(
    lvalue: &mut LValue,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let head_name = lvalue.segments[0].clone();
    let Some(head) = resolve_head_local(&head_name, lvalue.span, resolver, diagnostics) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot assign to `{}` — `{head_name}` is not a declared local",
                format_lvalue(lvalue),
            ),
            span,
        ));
        return;
    };
    diagnose_if_moved(
        &head_name,
        head.local_id,
        lvalue.span,
        resolver,
        diagnostics,
    );
    if !require_self_mutable(&head_name, lvalue.span, resolver, diagnostics) {
        return;
    }
    let Some(leaf_ty) = walk_field_segments(&head.ty, lvalue, resolver, diagnostics) else {
        return;
    };

    resolve_expr_with_expected(value, Some(&leaf_ty), resolver, diagnostics);

    let value_ty = value.resolution.clone();
    if !value_ty.is_resolved() {
        return;
    }
    if value_ty != leaf_ty {
        diagnostics.push(Diagnostic::error(
            format!(
                "type mismatch assigning to `{}`: field has type `{}`, but the right-hand \
                 side has type `{}`",
                format_lvalue(lvalue),
                display_resolution(&leaf_ty, resolver.registry),
                display_resolution(&value_ty, resolver.registry),
            ),
            span,
        ));
        return;
    }

    lvalue.head_resolved_type = Some(head.ty);
    lvalue.local_id = Some(head.local_id);
}

/// Bundle of the head-local resolution: the local id and its
/// resolved type. Returned by [`resolve_head_local`] so the field-
/// assignment / compound-assignment helpers can share the lookup
/// without each re-walking scope.
struct HeadLocal {
    local_id: LocalId,
    ty: ResolvedType,
}

/// Look up the head segment as a declared local in the current
/// scope. Returns `None` (without emitting a diagnostic) on miss so
/// each caller can attach its own framing message.
fn resolve_head_local(
    name: &str,
    _span: Span,
    resolver: &Resolver<'_>,
    _diagnostics: &mut [Diagnostic],
) -> Option<HeadLocal> {
    let (local_id, ty) = resolver.scope.lookup(name)?;
    Some(HeadLocal {
        local_id,
        ty: ty.clone(),
    })
}

/// Reject a `self.<field> = …` write when the enclosing fn's `self`
/// is borrowed (or there is no enclosing `self` at all). Mirrors v1's
/// `expo-typecheck::stmt::resolve_assignment` self-mutation gate.
/// Other head-local names trivially pass — any local declared via a
/// `let` or as a `move`/`borrow` regular param is mutable in the pipeline's
/// reassignment-keeps-type model.
fn require_self_mutable(
    head_name: &str,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if head_name != "self" {
        return true;
    }
    if matches!(resolver.self_pass_mode, Some(PassMode::Move)) {
        return true;
    }
    diagnostics.push(Diagnostic::error(
        "cannot mutate `self` — `self` is borrowed (read-only); use `move self` and \
         return the modified value to mutate"
            .to_string(),
        span,
    ));
    false
}

/// Walk `lvalue.segments[1..]` through nested struct definitions,
/// substituting each receiver's type-args at every step, and return
/// the leaf field's resolved type. Emits a diagnostic and returns
/// `None` on any non-struct intermediary or unknown field.
fn walk_field_segments(
    head_ty: &ResolvedType,
    lvalue: &LValue,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let mut current_ty = head_ty.clone();
    for segment in &lvalue.segments[1..] {
        let ResolvedType::Named {
            resolution: Resolution::Global(struct_id),
            type_args,
        } = &current_ty
        else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot project field `{segment}` on `{}` — field assignment requires \
                     a struct receiver",
                    display_resolution(&current_ty, resolver.registry),
                ),
                lvalue.span,
            ));
            return None;
        };
        let struct_id = *struct_id;
        let entry = resolver.registry.get(struct_id)?;
        let GlobalKind::Struct(Some(definition)) = &entry.kind else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot project field `{segment}` on `{}` ({}) — field assignment \
                     requires a struct receiver",
                    entry.identifier,
                    entry.kind.label(),
                ),
                lvalue.span,
            ));
            return None;
        };
        let Some((_, declared)) = definition.lookup_field(segment) else {
            diagnostics.push(Diagnostic::error(
                format!("`{}` has no field `{segment}`", entry.identifier),
                lvalue.span,
            ));
            return None;
        };
        let subst = Substitution::from_args(struct_id, type_args);
        current_ty = substitute(&declared.ty, &subst);
    }
    Some(current_ty)
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
