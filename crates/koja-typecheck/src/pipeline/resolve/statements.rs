//! Statement-level resolution helpers.
//!
//! Covers `Statement::Assignment` (declaration + same-type
//! reassignment + multi-segment field write),
//! `Statement::Destructure` (`(a, b) = rhs`, each name following the
//! same declaration-vs-reassignment rules), and
//! `Statement::CompoundAssign` (`x += rhs`, reassignment-only on
//! an arithmetic local, supports field-path targets like `p.x += 1`).
//!
//! Declaration vs. reassignment vs. field write lives in
//! [`resolve_assignment`]:
//!
//! - **Declaration / reassignment** (`segments.len() == 1`):
//!   - First write of a name. Optional type annotation must match the
//!     rhs (or infer from rhs), insert into the scope, and stamp the
//!     target `LValue`'s implied [`Resolution::Local`] via the AST
//!     `Expr` shape produced for `target` lookup. If the bare name
//!     matches a package-level
//!     [`crate::registry::GlobalKind::Constant`] entry, assignment
//!     is rejected. Constants are immutable and cannot share an
//!     assignment LHS with locals.
//!   - Subsequent write of an existing name. Type annotation is a
//!     feature gap (only legal on first decl). The rhs must flow
//!     into the existing local's type. A union member widens and a
//!     number literal takes the width. The existing [`LocalId`]
//!     stays put.
//!   - `_` as the target is a discard. The rhs resolves (against the
//!     annotation when one is written), no local is declared, and the
//!     target keeps no [`LocalId`].
//!
//! - **Field write** (`segments.len() >= 2`). The head segment must
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
//! declaration path. The head local must already exist, the leaf
//! field type must be `Int` or `Float`, and the rhs must be a value
//! that the binary operator accepts next to that type.
//!
//! [`LocalId`]: koja_ast::identifier::LocalId
//! [`LValue`]: koja_ast::ast::LValue
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local

use koja_ast::ast::{
    CompoundOp, Diagnostic, Expr, LValue, Pattern, Statement, TypeExpr, path_text,
};
use koja_ast::identifier::{AnonymousKind, Identifier, LocalId, Resolution, ResolvedType};
use koja_ast::labels::{
    compound_op_label, pattern_kind_label, pattern_span, type_expr_span as annotation_span,
};
use koja_ast::span::Span;

use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::{DisplayStyle, GlobalKind, display_type};

use super::coercion::{
    Compatible, Mismatch, check_compatible, check_compatible_stamping, mismatch_message,
    tuple_literal_elements, tuple_literal_elements_mut,
};
use super::ctx::Resolver;
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::ops::{coerce_literal_to, resolve_operands_with_sibling_hint};
use super::patterns::tuple_element_types;
use super::types::{is_arithmetic_type, types_equivalent};

/// Resolve a `target = value` statement. Validates target shape,
/// resolves the rhs, applies declaration-vs-reassignment rules, and
/// updates the resolver's scope accordingly. Multi-segment field
/// writes route through [`resolve_field_assignment`].
pub(super) fn resolve_assignment(
    lvalue: &mut LValue,
    type_annotation: Option<&mut TypeExpr>,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if lvalue.segments.len() >= 2 {
        if let Some(annotation) = &type_annotation {
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
    let annotation_span = type_annotation.as_deref().map(annotation_span);
    let expected_ty: Option<ResolvedType> = type_annotation.and_then(|annotation| {
        let resolved = resolve_type_expr(
            annotation,
            TypeParamScope::new(resolver.type_param_owners),
            resolver.resolution_scope(),
            diagnostics,
        );
        resolved.is_resolved().then_some(resolved)
    });
    // A reassignment has no annotation. The type of the existing
    // local is then the expected type, so `found = Option.None` reads
    // its type argument from `found`.
    let existing_ty = existing_local_type(&lvalue.segments[0].text, resolver);
    let hint = expected_ty.as_ref().or(existing_ty.as_ref());
    resolve_expr_with_expected(value, hint, resolver, diagnostics);
    declare_assignment_target(
        lvalue,
        annotation_span,
        expected_ty,
        value,
        span,
        resolver,
        diagnostics,
    );
}

/// Resolve the two operand bindings of a desugared `assert`
/// comparison the way `==` types its operands. The values resolve
/// together, so a unit variant or generic constructor on either side
/// takes its type from the other, and an integer or float literal
/// that fits the other side's width takes that width. Any other value
/// keeps its own type, so a mismatch surfaces at the comparison, not
/// as an annotation error on a synthesized name.
pub(super) fn resolve_comparison_operands(
    left: &mut Statement,
    right: &mut Statement,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (left_target, left_value, left_span) = assignment_parts(left);
    let (right_target, right_value, right_span) = assignment_parts(right);
    resolve_operands_with_sibling_hint(left_value, right_value, resolver, diagnostics);
    let left_ty = left_value.resolution.clone();
    let right_ty = right_value.resolution.clone();
    declare_operand(
        left_target,
        left_value,
        &right_ty,
        left_span,
        resolver,
        diagnostics,
    );
    declare_operand(
        right_target,
        right_value,
        &left_ty,
        right_span,
        resolver,
        diagnostics,
    );
}

fn assignment_parts(statement: &mut Statement) -> (&mut LValue, &mut Expr, Span) {
    let Statement::Assignment {
        target,
        value,
        span,
        ..
    } = statement
    else {
        unreachable!("assert comparison desugars its operands to assignments");
    };
    (target, value, *span)
}

/// Declare one operand temporary. A value that failed to resolve has
/// already reported, so the name still enters scope as an unresolved
/// local and the reads the desugar emits stay quiet. A literal that
/// coerces to the other operand's type is declared with that type, so
/// the comparison and the rendering both see the width.
fn declare_operand(
    lvalue: &mut LValue,
    value: &mut Expr,
    other: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !value.resolution.is_resolved() {
        let name = lvalue.segments[0].text.clone();
        lvalue.local_id = Some(resolver.scope.declare(&name, ResolvedType::Failed));
        return;
    }
    let coerced = matches!(
        check_compatible(value, &value.resolution, other, resolver.registry),
        Compatible::Coerced(_)
    );
    declare_assignment_target(
        lvalue,
        None,
        coerced.then(|| other.clone()),
        value,
        span,
        resolver,
        diagnostics,
    );
}

/// Declare or rebind the single-segment target of an assignment whose
/// value has already resolved. `annotated` is the resolved form of
/// the written type annotation, and `annotation_span` locates it.
fn declare_assignment_target(
    lvalue: &mut LValue,
    annotation_span: Option<Span>,
    annotated: Option<ResolvedType>,
    value: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let name = lvalue.segments[0].text.clone();

    let value_ty = value.resolution.clone();
    let already_declared = resolver.scope.lookup(&name).is_some();
    if let Some(span) = annotation_span.filter(|_| already_declared) {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck only allows type annotations on the first declaration \
                 of a local (`{name}` was already declared)",
            ),
            span,
        ));
        return;
    }
    let declared_ty = match annotated {
        Some(annotated) => {
            if value_ty.is_resolved() {
                check_write(
                    value,
                    &annotated,
                    &format!("local `{name}`"),
                    || {
                        format!(
                            "type annotation on `{name}` says `{}`, but the \
                             right-hand side has type `{}`",
                            display_type(&annotated, resolver.registry, DisplayStyle::Source),
                            display_type(&value_ty, resolver.registry, DisplayStyle::Source),
                        )
                    },
                    span,
                    resolver,
                    diagnostics,
                );
            }
            annotated
        }
        None => {
            if !value_ty.is_resolved() {
                // The rhs already triggered its own diagnostic. On a
                // first declaration, leave the local out of scope so
                // later references diagnose as unknown rather than as
                // a typed hole.
                return;
            }
            check_reassignment(&name, value, span, resolver, diagnostics).unwrap_or(value_ty)
        }
    };

    // `_ = value` discards the value. The rhs was checked above, and
    // nothing enters scope, so `_` never carries a type between
    // statements. IR lower reads the missing LocalId as the discard.
    if lvalue.is_discard() {
        return;
    }

    // Stamp the target so IR lower can read the LocalId without
    // re-walking scope. Single-segment is the only shape that reaches
    // here. Multi-segment field writes routed to
    // `resolve_field_assignment` above and bailed early.
    lvalue.local_id = rebind_or_declare_local(&name, declared_ty, span, resolver, diagnostics);
}

/// Check `value` against the type of the target it is written to and
/// record the coercion on it. Returns `false` after it reports. A
/// literal that does not fit reports its range under `subject`. Any
/// other mismatch reports the text of `incompatible`.
fn check_write(
    value: &mut Expr,
    target_ty: &ResolvedType,
    subject: &str,
    incompatible: impl FnOnce() -> String,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let value_ty = value.resolution.clone();
    let Some(mismatch) = check_compatible_stamping(value, &value_ty, target_ty, resolver.registry)
    else {
        return true;
    };
    let message = match mismatch {
        Mismatch::OutOfRange { .. } => {
            mismatch_message(subject, &mismatch, target_ty, &value_ty, resolver.registry)
        }
        Mismatch::Incompatible => incompatible(),
    };
    diagnostics.push(Diagnostic::error(message, span));
    false
}

/// The type of the local `name`, when the local exists and its type
/// resolved.
fn existing_local_type(name: &str, resolver: &Resolver<'_>) -> Option<ResolvedType> {
    resolver
        .scope
        .lookup(name)
        .map(|(_, ty)| ty.clone())
        .filter(ResolvedType::is_resolved)
}

/// Check `value` as a write to the existing local `name` and return
/// the type of the local. A local type is fixed at declaration, so
/// the value must flow into it. A union member widens, and a number
/// literal takes the width. `None` when `name` is not such a local.
fn check_reassignment(
    name: &str,
    value: &mut Expr,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let existing_ty = existing_local_type(name, resolver)?;
    let value_ty = value.resolution.clone();
    check_write(
        value,
        &existing_ty,
        &format!("local `{name}`"),
        || reassign_message(name, &existing_ty, &value_ty, resolver),
        span,
        resolver,
        diagnostics,
    );
    Some(existing_ty)
}

/// The diagnostic text for a write that does not fit the type of
/// the existing local `name`.
fn reassign_message(
    name: &str,
    existing_ty: &ResolvedType,
    ty: &ResolvedType,
    resolver: &Resolver<'_>,
) -> String {
    format!(
        "cannot reassign `{name}` from `{}` to `{}` because local types are fixed at declaration",
        display_type(existing_ty, resolver.registry, DisplayStyle::Source),
        display_type(ty, resolver.registry, DisplayStyle::Source),
    )
}

/// Bind `name` to a value of type `ty` the way `name = value` does.
/// An existing local is rebound and keeps its `LocalId` (local types
/// are fixed at declaration, so `ty` must match), a new name is
/// declared. Shared by plain assignment and destructuring.
fn rebind_or_declare_local(
    name: &str,
    ty: ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<LocalId> {
    if let Some((existing_id, existing_ty)) = resolver.scope.lookup(name) {
        if ty != *existing_ty {
            diagnostics.push(Diagnostic::error(
                reassign_message(name, existing_ty, &ty, resolver),
                span,
            ));
        }
        return Some(existing_id);
    }
    if assigns_to_package_constant(name, resolver) {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot assign to `{name}` because package-level constants are immutable and \
                 cannot be reassigned like a local",
            ),
            span,
        ));
        return None;
    }
    Some(resolver.scope.declare(name, ty))
}

/// Resolve a `(a, b) = value` statement. The value resolves first,
/// then the pattern binds element-wise. Only bindings, wildcards,
/// and nested tuples are allowed, so the pattern can never fail at
/// runtime. Each name follows assignment rules, so
/// `(n, ok) = step(n)` inside a loop body writes the enclosing `n`
/// instead of declaring a shadow.
///
/// A tuple literal on the right pairs each name with one element
/// expression, and that pair follows the rules of `name = element`.
/// Any other value binds against its tuple type, and each element
/// type must equal the type of its existing local.
pub(super) fn resolve_destructure(
    pattern: &mut Pattern,
    value: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let expected_ty = Some(destructure_expected_type(pattern, value, resolver))
        .filter(|ty| !matches!(ty, ResolvedType::Unresolved));
    resolve_expr_with_expected(value, expected_ty.as_ref(), resolver, diagnostics);
    if !destructure_pattern_is_irrefutable(pattern, diagnostics) {
        return;
    }
    if !value.resolution.is_resolved() {
        return;
    }
    bind_destructure_value(pattern, value, resolver, diagnostics);
}

/// The expected type of `value` on the right of a destructuring
/// assignment to `pattern`. A name that is an existing local gives
/// its type. A tuple pattern over a tuple literal of the same arity
/// gives a tuple of the same. Every other pair gives a gap
/// ([`ResolvedType::Unresolved`]), because the value has no target
/// type to read.
fn destructure_expected_type(
    pattern: &Pattern,
    value: &Expr,
    resolver: &Resolver<'_>,
) -> ResolvedType {
    match pattern {
        Pattern::Binding { name, .. } => {
            existing_local_type(name.as_str(), resolver).unwrap_or(ResolvedType::Unresolved)
        }
        Pattern::Tuple {
            elements: patterns, ..
        } => match tuple_literal_elements(value) {
            Some(elements) if elements.len() == patterns.len() => {
                ResolvedType::Anonymous(AnonymousKind::Tuple {
                    elements: patterns
                        .iter()
                        .zip(elements)
                        .map(|(pattern, element)| {
                            destructure_expected_type(pattern, element, resolver)
                        })
                        .collect(),
                })
            }
            _ => ResolvedType::Unresolved,
        },
        _ => ResolvedType::Unresolved,
    }
}

/// Bind an irrefutable destructure pattern to `value`, which has
/// already resolved. A name is one assignment `name = value`. A
/// tuple pattern over a tuple literal of the same arity pairs each
/// element pattern with its element expression. A tuple pattern
/// over any other value binds against the type of the value through
/// [`bind_destructure_pattern`].
fn bind_destructure_value(
    pattern: &mut Pattern,
    value: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match pattern {
        Pattern::Binding {
            local_id,
            name,
            span,
        } => {
            let ty = check_reassignment(name.as_str(), value, *span, resolver, diagnostics)
                .unwrap_or_else(|| value.resolution.clone());
            *local_id = rebind_or_declare_local(name.as_str(), ty, *span, resolver, diagnostics);
        }
        Pattern::Tuple {
            elements: patterns, ..
        } => {
            let arity = patterns.len();
            match tuple_literal_elements_mut(value).filter(|elements| elements.len() == arity) {
                Some(elements) => {
                    for (pattern, element) in patterns.iter_mut().zip(elements) {
                        bind_destructure_value(pattern, element, resolver, diagnostics);
                    }
                }
                None => {
                    let subject_ty = value.resolution.clone();
                    bind_destructure_pattern(pattern, &subject_ty, resolver, diagnostics);
                }
            }
        }
        Pattern::Wildcard { .. } => {}
        other => unreachable!(
            "destructure_pattern_is_irrefutable admitted a refutable {other:?} pattern"
        ),
    }
}

/// Stamp every binding in an irrefutable destructure pattern via
/// [`rebind_or_declare_local`], recursing into nested tuples. Each
/// element type must equal the type of its existing local.
fn bind_destructure_pattern(
    pattern: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match pattern {
        Pattern::Binding {
            local_id,
            name,
            span,
        } => {
            *local_id = rebind_or_declare_local(
                name.as_str(),
                subject_ty.clone(),
                *span,
                resolver,
                diagnostics,
            );
        }
        Pattern::Tuple { elements, span } => {
            let Some(element_types) =
                tuple_element_types(subject_ty, elements.len(), *span, resolver, diagnostics)
            else {
                return;
            };
            for (element, element_ty) in elements.iter_mut().zip(&element_types) {
                bind_destructure_pattern(element, element_ty, resolver, diagnostics);
            }
        }
        Pattern::Wildcard { .. } => {}
        other => unreachable!(
            "destructure_pattern_is_irrefutable admitted a refutable {other:?} pattern"
        ),
    }
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
    let name = target.segments[0].text.clone();

    let Some(head) = resolve_head_local(&name, target.span, resolver, diagnostics) else {
        if assigns_to_package_constant(&name, resolver) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot apply `{op_label}=` to `{name}` because package-level constants are \
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
                display_type(&leaf_ty, resolver.registry, DisplayStyle::Source),
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
    // `x += value` accepts what `x + value` accepts. That is a value
    // of the same type, or a number literal that takes the width of
    // `x`.
    if !types_equivalent(&value_ty, &leaf_ty, resolver.registry)
        && !coerce_literal_to(value, &leaf_ty, resolver.registry)
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "type mismatch on `{op_label}=` for `{}`. lhs is `{}`, rhs is `{}`",
                format_lvalue(target),
                display_type(&leaf_ty, resolver.registry, DisplayStyle::Source),
                display_type(&value_ty, resolver.registry, DisplayStyle::Source),
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
    let head_name = lvalue.segments[0].text.clone();
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
    let written = check_write(
        value,
        &leaf_ty,
        &format!("field `{}`", format_lvalue(lvalue)),
        || {
            format!(
                "type mismatch assigning to `{}`. Field has type `{}`, but the right-hand \
                 side has type `{}`",
                format_lvalue(lvalue),
                display_type(&leaf_ty, resolver.registry, DisplayStyle::Source),
                display_type(&value_ty, resolver.registry, DisplayStyle::Source),
            )
        },
        span,
        resolver,
        diagnostics,
    );
    if !written {
        return;
    }

    lvalue.head_resolved_type = Some(head.ty);
    lvalue.local_id = Some(head.local_id);
}

/// Bundle of the head-local resolution, the local id and its
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
                    "cannot project field `{segment}` on `{}` because field assignment requires \
                     a struct receiver",
                    display_type(&current_ty, resolver.registry, DisplayStyle::Source),
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
                    "cannot project field `{segment}` on `{}` ({}). Field assignment \
                     requires a struct receiver",
                    entry.identifier,
                    entry.kind.label(),
                ),
                lvalue.span,
            ));
            return None;
        };
        let Some((_, declared)) = definition.lookup_field(segment.as_str()) else {
            diagnostics.push(Diagnostic::error(
                format!("`{}` has no field `{segment}`", entry.identifier),
                lvalue.span,
            ));
            return None;
        };
        let subst = Substitution::from_args(struct_id, type_args);
        current_ty = substitute(&declared.ty, &subst, resolver.registry);
    }
    Some(current_ty)
}

fn format_lvalue(lvalue: &LValue) -> String {
    path_text(&lvalue.segments)
}

/// True when `name` is a package-level constant in the resolver's
/// package (same namespace as locals for single-segment assignment
/// targets).
fn assigns_to_package_constant(name: &str, resolver: &Resolver<'_>) -> bool {
    let identifier = Identifier::single(resolver.package, name);
    resolver
        .registry
        .lookup(&identifier)
        .is_some_and(|(_, entry)| matches!(entry.kind, GlobalKind::Constant(_)))
}
