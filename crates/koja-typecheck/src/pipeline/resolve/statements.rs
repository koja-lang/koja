//! Statement-level resolution helpers.
//!
//! Covers `Statement::Assignment` (declaration + same-type
//! reassignment + multi-segment field write) and
//! `Statement::CompoundAssign` (`x += rhs`, reassignment-only on
//! an arithmetic local, supports field-path targets like `p.x += 1`).
//! Pattern destructuring still surfaces as a feature-gap diagnostic
//! here so later passes never see that shape.
//!
//! Declaration vs. reassignment vs. field write lives in
//! [`resolve_assignment`]:
//!
//! - **Declaration / reassignment** (`segments.len() == 1`):
//!   - First write of a name: optional type annotation must match the
//!     rhs (or infer from rhs), insert into the scope, and stamp the
//!     target `LValue`'s implied [`Resolution::Local`] via the AST
//!     `Expr` shape produced for `target` lookup. If the bare name
//!     matches a package-level
//!     [`crate::registry::GlobalKind::Constant`] entry, assignment
//!     is rejected: constants are immutable and cannot share an
//!     assignment LHS with locals.
//!   - Subsequent write of an existing name: type annotation is a
//!     feature gap (only legal on first decl). Rhs type must equal
//!     the existing local's type, and the existing [`LocalId`] stays put.
//!
//! - **Field write** (`segments.len() >= 2`): the head segment must
//!   resolve to a declared local (`self` included). Each subsequent
//!   segment projects through a struct definition's field roster, applying
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
//! [`LocalId`]: koja_ast::identifier::LocalId
//! [`LValue`]: koja_ast::ast::LValue
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local

use koja_ast::ast::{CompoundOp, Diagnostic, Expr, LValue, Pattern, TypeExpr};
use koja_ast::identifier::{Identifier, LocalId, Resolution, ResolvedType};
use koja_ast::labels::{
    compound_op_label, pattern_kind_label, pattern_span, type_expr_span as annotation_span,
};
use koja_ast::span::Span;

use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::GlobalKind;

use super::coercion::check_compatible_stamping;
use super::ctx::Resolver;
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::patterns::resolve_pattern;
use super::types::{display_resolution, is_arithmetic_type};

/// Resolve a `target = value` statement. Validates target shape,
/// resolves the rhs, applies declaration-vs-reassignment rules, and
/// updates the resolver's scope accordingly. Multi-segment field
/// writes route through [`resolve_field_assignment`].
pub(super) fn resolve_assignment(
    lvalue: &mut LValue,
    type_annotation: Option<&TypeExpr>,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
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
    // shapes like `result: List<T> = List.new()`, where the annotation's
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
                // The rhs already triggered its own diagnostic. Stay
                // quiet to avoid piling on with a type-mismatch.
                return;
            }
            if value_ty != existing_ty {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "cannot reassign `{name}` from `{}` to `{}`: local types are fixed at \
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
                        "cannot assign to `{name}`: package-level constants are immutable and \
                         cannot be reassigned like a local",
                    ),
                    span,
                ));
                return;
            }
            let declared_ty = match expected_ty {
                Some(annotated) => {
                    if value_ty.is_resolved()
                        && check_compatible_stamping(
                            value,
                            &value_ty,
                            &annotated,
                            resolver.registry,
                        )
                        .is_some()
                    {
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
                    annotated
                }
                None => {
                    if !value_ty.is_resolved() {
                        // Without an annotation we can only infer
                        // from the rhs. If that failed, leave the
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
    // here. Multi-segment field writes routed to
    // `resolve_field_assignment` above and bailed early.
    lvalue.local_id = Some(local_id);
}

/// Resolve a `(a, b) = value` statement. The value resolves first,
/// then the pattern binds element-wise against its tuple type. Only
/// bindings, wildcards, and nested tuples are allowed, so the
/// pattern can never fail at runtime.
pub(super) fn resolve_destructure(
    pattern: &mut Pattern,
    value: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    resolve_expr(value, resolver, diagnostics);
    if !destructure_pattern_is_irrefutable(pattern, diagnostics) {
        return;
    }
    if !value.resolution.is_resolved() {
        return;
    }
    let subject_ty = value.resolution.clone();
    resolve_pattern(pattern, &subject_ty, resolver, diagnostics);
}

/// True when every leaf is a binding, wildcard, or nested tuple.
/// Any other shape is refutable and diagnosed. Walks the whole
/// pattern so one statement reports every offending leaf at once.
fn destructure_pattern_is_irrefutable(
    pattern: &Pattern,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    match pattern {
        Pattern::Binding { .. } | Pattern::Wildcard { .. } => true,
        Pattern::Tuple { elements, .. } => {
            let mut irrefutable = true;
            for element in elements {
                irrefutable &= destructure_pattern_is_irrefutable(element, diagnostics);
            }
            irrefutable
        }
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "destructuring assignment only allows names, `_`, and nested \
                     tuples, found a {} pattern",
                    pattern_kind_label(other),
                ),
                pattern_span(other),
            ));
            false
        }
    }
}

/// Resolve a `target op= value` statement. Reassignment only, so
/// the target (or its leaf field, on a multi-segment path) must
/// already resolve to an arithmetic type (`Int` or `Float`), and
/// the rhs must match. On success, stamps `target.local_id` (the
/// head local) so IR lower can desugar to `LocalRead + (FieldGet*)
/// + BinaryOp + (FieldSet*) + LocalWrite`.
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
                    "cannot apply `{op_label}=` to `{name}`: package-level constants are \
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
        // The rhs already triggered its own diagnostic. Stay quiet
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
/// The head segment must name a declared local (`self` included).
/// Each subsequent segment projects through a struct field roster
/// while substituting the receiver's type-args
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
        diagnostics.push(Diagnostic::error_with_hint(
            format!("cannot assign to `{}`", format_lvalue(lvalue)),
            format!("`{head_name}` is not a declared local"),
            span,
        ));
        return;
    };
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

/// Always permits a `self.<field> = …` write. Under value semantics
/// `self` is an independent local value, so reassigning a field
/// produces a new value the method returns (`self -> Self`). There is
/// no borrowed/owned distinction to gate on.
fn require_self_mutable(
    _head_name: &str,
    _span: Span,
    _resolver: &Resolver<'_>,
    _diagnostics: &mut [Diagnostic],
) -> bool {
    true
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
                    "cannot project field `{segment}` on `{}`: field assignment requires \
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
                    "cannot project field `{segment}` on `{}` ({}): field assignment \
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
