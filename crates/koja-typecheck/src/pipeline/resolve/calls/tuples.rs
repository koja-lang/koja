//! `recv.m(args)` against an anonymous-tuple receiver. Tuples are
//! structural (no registry entry, no impl blocks), so only the
//! universal protocol functions resolve: `format` / `print` /
//! `inspect` from `Debug` and `equals?` from `Equality`. IR lowering
//! expands each of these inline per tuple shape, mirroring what
//! `derive_debug` / `derive_equality` synthesize for nominal types.
//! `equals?` additionally requires every element (recursively) to carry
//! valid equality semantics, while `Debug` renders opaque elements
//! as `"..."` instead.

use koja_ast::ast::{Arg, Diagnostic, Expr};
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr_with_expected;
use super::super::types::{
    display_resolution, peel_alias, type_supports_equality, types_equivalent,
};
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
        "equals?" => resolve_tuple_eq(receiver, args, call_span, resolver, diagnostics),
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
                     universal protocol functions `format`, `print`, `inspect`, and `equals?`",
                    display_resolution(&receiver.resolution, resolver.registry),
                ),
                call_span,
            ));
            ResolvedType::unresolved()
        }
    }
}

/// `lhs.equals?(rhs)`: one argument, structurally the same tuple shape as
/// the receiver. Elements compare through their own `Equality`
/// conformance. Every element must have valid equality semantics, so
/// closure and union elements are rejected rather than skipped.
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
            format!(
                "tuple `equals?` takes exactly 1 argument, got {}",
                args.len()
            ),
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

/// Every element must have valid equality semantics, or IR lowering
/// would have no `equals?` to call for it. Closures and unions have no
/// defined equality, so tuples containing them (at any nesting
/// depth) reject instead of silently skipping the element.
/// Type-param elements ride the universal-`Equality` bound.
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
            ResolvedType::Anonymous(AnonymousKind::Function { .. }) => emit_no_equality(
                element,
                "closures cannot be compared for equality. Compare the other elements \
                 individually, or keep the closure out of the compared value"
                    .to_string(),
                call_span,
                resolver,
                diagnostics,
            ),
            ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
                check_elements_support_equality(
                    &structural_element,
                    call_span,
                    resolver,
                    diagnostics,
                );
            }
            ResolvedType::Named {
                resolution: Resolution::Global(_),
                ..
            } if !type_supports_equality(&structural_element, resolver.bound_context()) => {
                emit_no_equality(
                    element,
                    format!(
                        "`{}` does not implement `Equality`",
                        display_resolution(element, resolver.registry),
                    ),
                    call_span,
                    resolver,
                    diagnostics,
                );
            }
            ResolvedType::Union(_) => emit_no_equality(
                element,
                "union values cannot be compared for equality. Match on the member type \
                 first, then compare the members"
                    .to_string(),
                call_span,
                resolver,
                diagnostics,
            ),
            _ => {}
        }
    }
}

fn emit_no_equality(
    element: &ResolvedType,
    hint: String,
    call_span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::error_with_hint(
        format!(
            "cannot compare tuples containing `{}`",
            display_resolution(element, resolver.registry),
        ),
        hint,
        call_span,
    ));
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
