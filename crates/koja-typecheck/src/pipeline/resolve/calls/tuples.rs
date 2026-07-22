//! `recv.m(args)` against an anonymous-tuple receiver. Tuples are
//! structural (no registry entry, no impl blocks), so only the
//! universal protocol functions resolve: `format` / `print` /
//! `inspect` from `Debug` and `eq` from `Equality`. IR lowering
//! expands each of these inline per tuple shape, mirroring what
//! `derive_debug` / `derive_equality` synthesize for nominal types
//! (including the opaque-element fallbacks).

use koja_ast::ast::{Arg, Diagnostic, Expr};
use koja_ast::identifier::{AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr_with_expected;
use super::super::ops::is_primitive_equality_eligible;
use super::super::types::{display_resolution, peel_alias, types_equivalent};
use super::resolve_args;

pub(super) fn resolve_tuple_method_call(
    receiver: &Expr,
    method: &str,
    args: &mut [Arg],
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match method {
        "eq" => resolve_tuple_eq(receiver, args, call_span, resolver, diagnostics),
        "format" => zero_arg_return(
            "format",
            resolver.registry.primitive("String"),
            args,
            call_span,
            resolver,
            diagnostics,
        ),
        "inspect" => zero_arg_return(
            "inspect",
            receiver.resolution.clone(),
            args,
            call_span,
            resolver,
            diagnostics,
        ),
        "print" => zero_arg_return(
            "print",
            resolver.registry.primitive("Unit"),
            args,
            call_span,
            resolver,
            diagnostics,
        ),
        other => {
            resolve_args(args, None, resolver, diagnostics);
            diagnostics.push(Diagnostic::error(
                format!(
                    "no function `{other}` on tuple type `{}`. Tuples support only the \
                     universal protocol functions `format`, `print`, `inspect`, and `eq`",
                    display_resolution(&receiver.resolution, resolver.registry),
                ),
                call_span,
            ));
            ResolvedType::unresolved()
        }
    }
}

/// `lhs.eq(rhs)`: one argument, structurally the same tuple shape as
/// the receiver. Elements compare through their own `Equality`
/// conformance, with opaque elements (closures, unions) skipped the
/// same way derived struct `eq` skips opaque fields.
fn resolve_tuple_eq(
    receiver: &Expr,
    args: &mut [Arg],
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let bool_ty = resolver.registry.primitive("Bool");
    let [arg] = args else {
        resolve_args(args, None, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!("tuple `eq` takes exactly 1 argument, got {}", args.len()),
            call_span,
        ));
        return bool_ty;
    };
    resolve_expr_with_expected(
        &mut arg.value,
        Some(&receiver.resolution),
        resolver,
        diagnostics,
    );
    if !types_equivalent(
        &arg.value.resolution,
        &receiver.resolution,
        resolver.registry,
    ) {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot compare `{}` with `{}`. Tuple equality requires both sides to \
                 have the same tuple shape",
                display_resolution(&receiver.resolution, resolver.registry),
                display_resolution(&arg.value.resolution, resolver.registry),
            ),
            call_span,
        ));
        return bool_ty;
    }
    check_elements_support_equality(&receiver.resolution, call_span, resolver, diagnostics);
    bool_ty
}

/// Every comparable element must conform to `Equality`, or IR
/// lowering would have no `eq` function to call. Opaque elements
/// (closures, unions) are exempt because lowering skips them, and
/// type-param elements ride the universal-`Equality` bound.
fn check_elements_support_equality(
    tuple_ty: &ResolvedType,
    call_span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) =
        peel_alias(tuple_ty, resolver.registry)
    else {
        return;
    };
    for element in &elements {
        let structural_element = peel_alias(element, resolver.registry);
        match &structural_element {
            ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
                check_elements_support_equality(
                    &structural_element,
                    call_span,
                    resolver,
                    diagnostics,
                );
            }
            ResolvedType::Named {
                resolution: Resolution::Global(id),
                ..
            } if !element_has_eq(&structural_element, *id, resolver) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "cannot compare tuples containing `{}`. The element type does \
                         not conform to `Equality`",
                        display_resolution(element, resolver.registry),
                    ),
                    call_span,
                ));
            }
            _ => {}
        }
    }
}

fn element_has_eq(element: &ResolvedType, id: GlobalRegistryId, resolver: &Resolver<'_>) -> bool {
    if is_primitive_equality_eligible(element, resolver.registry) {
        return true;
    }
    let Some(entry) = resolver.registry.get(id) else {
        return false;
    };
    let mut eq_path = entry.identifier.path().to_vec();
    eq_path.push("eq".to_string());
    let eq_identifier = Identifier::new(entry.identifier.package(), eq_path);
    resolver.registry.lookup(&eq_identifier).is_some()
}

/// Shared zero-argument validation for the `Debug` family.
fn zero_arg_return(
    method: &str,
    return_ty: ResolvedType,
    args: &mut [Arg],
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if !args.is_empty() {
        resolve_args(args, None, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!("tuple `{method}` takes no arguments, got {}", args.len()),
            call_span,
        ));
    }
    return_ty
}
