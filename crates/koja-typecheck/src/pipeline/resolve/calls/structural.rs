//! `recv.m(args)` against a structural receiver: an anonymous tuple,
//! a function type, or a union. None of these has a registry entry
//! or impl blocks, so only the universal protocol functions resolve:
//! `equals?` from `Equality` on every shape, plus `format` / `print`
//! / `inspect` from `Debug` on tuples and unions. IR lowering expands
//! each of these inline per shape, mirroring what `derive_debug` /
//! `derive_equality` synthesize for nominal types. `equals?` also
//! requires every tuple element and union member (recursively) to
//! carry `Equality`, while `Debug` renders opaque parts as `"..."`.

use koja_ast::ast::{Arg, Diagnostic, Expr};
use koja_ast::identifier::{AnonymousKind, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr_with_expected;
use super::super::types::{
    display_resolution, peel_alias, type_supports_equality, types_equivalent,
};
use super::resolve_args;
use crate::registry::{GlobalRegistry, ResolvedProtocolBound};

/// Which structural shape a method receiver has. Decides the
/// admitted method set and the wording of rejections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StructuralShape {
    Function,
    Tuple,
    Union,
}

impl StructuralShape {
    fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Tuple => "tuple",
            Self::Union => "union",
        }
    }

    /// Function values are `Debug` only through a derived body (they
    /// render as `"..."`), so a direct `f.format()` stays rejected.
    /// Unions also hash (tag mixed into the member's hash) when every
    /// member does.
    fn admits(self, method: &str) -> bool {
        match self {
            Self::Function => method == "equals?",
            Self::Tuple => matches!(method, "equals?" | "format" | "inspect" | "print"),
            Self::Union => matches!(method, "equals?" | "format" | "hash" | "inspect" | "print"),
        }
    }

    fn supported_methods(self) -> &'static str {
        match self {
            Self::Function => "`equals?`",
            Self::Tuple => "`format`, `print`, `inspect`, and `equals?`",
            Self::Union => "`format`, `print`, `inspect`, `equals?`, and `hash`",
        }
    }
}

/// Inputs to [`resolve_structural_method_call`], bundled like
/// [`super::bounded::BoundedCall`] so the helper reads as one site.
pub(super) struct StructuralCall<'a> {
    pub(super) args: &'a mut [Arg],
    pub(super) call_span: Span,
    pub(super) method: &'a str,
    pub(super) receiver: &'a Expr,
    pub(super) shape: StructuralShape,
    pub(super) target: &'a mut Resolution,
}

pub(super) fn resolve_structural_method_call(
    site: StructuralCall<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let StructuralCall {
        args,
        call_span,
        method,
        receiver,
        shape,
        target,
    } = site;
    if !shape.admits(method) {
        resolve_args(args, None, resolver, diagnostics);
        let message = format!(
            "no function `{method}` on {} type `{}`. {}s support only the universal \
             protocol functions {}",
            shape.label(),
            display_resolution(&receiver.resolution, resolver.registry),
            capitalize(shape.label()),
            shape.supported_methods(),
        );
        diagnostics.push(match shape {
            StructuralShape::Union => Diagnostic::error_with_hint(
                message,
                "match the union first to bind a specific member".to_string(),
                call_span,
            ),
            StructuralShape::Function | StructuralShape::Tuple => {
                Diagnostic::error(message, call_span)
            }
        });
        return ResolvedType::unresolved();
    }
    // Structural receivers have no function to target: IR expands
    // the call inline and seal exempts them. Alias-backed shapes keep
    // the alias id as a dispatch marker so seal still sees a target.
    if let ResolvedType::Named {
        resolution: Resolution::Global(alias_id),
        ..
    } = receiver.resolution
    {
        *target = Resolution::Global(alias_id);
    }
    match method {
        "equals?" => resolve_structural_eq(shape, receiver, args, call_span, resolver, diagnostics),
        "format" => zero_arg_return(
            shape,
            "format",
            resolver.registry.primitive("String"),
            args,
            call_span,
            resolver,
            diagnostics,
        ),
        "hash" => resolve_union_hash(receiver, args, call_span, resolver, diagnostics),
        "inspect" => zero_arg_return(
            shape,
            "inspect",
            receiver.resolution.clone(),
            args,
            call_span,
            resolver,
            diagnostics,
        ),
        "print" => zero_arg_return(
            shape,
            "print",
            resolver.registry.primitive("Unit"),
            args,
            call_span,
            resolver,
            diagnostics,
        ),
        _ => unreachable!("`admits` filtered the method set"),
    }
}

/// `lhs.equals?(rhs)`: one argument of the same structural type as
/// the receiver. Parts compare through their own `Equality`
/// conformance, so every tuple element and union member must carry
/// one.
fn resolve_structural_eq(
    shape: StructuralShape,
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
                "{} `equals?` takes exactly 1 argument, got {}",
                shape.label(),
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
                "cannot compare `{}` with `{}`. {} equality requires both sides to have \
                 the same type",
                display_resolution(&receiver.resolution, resolver.registry),
                display_resolution(&arg.value.resolution, resolver.registry),
                capitalize(shape.label()),
            ),
            call_span,
        ));
        return bool_ty;
    }
    if let Some(part) = first_part_without_equality(&receiver.resolution, resolver) {
        diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "cannot compare {}s containing `{}`",
                shape.label(),
                display_resolution(&part, resolver.registry),
            ),
            format!(
                "`{}` does not implement `Equality`",
                display_resolution(&part, resolver.registry),
            ),
            call_span,
        ));
    }
    bool_ty
}

/// `union.hash()`: zero arguments, `Int` result, every member `Hash`.
fn resolve_union_hash(
    receiver: &Expr,
    args: &mut [Arg],
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let int_ty = zero_arg_return(
        StructuralShape::Union,
        "hash",
        resolver.registry.primitive("Int"),
        args,
        call_span,
        resolver,
        diagnostics,
    );
    let hash_bound = protocol_bound("Hash", resolver.registry);
    let ResolvedType::Union(members) = peel_alias(&receiver.resolution, resolver.registry) else {
        return int_ty;
    };
    let unhashable = members.iter().find(|member| {
        hash_bound.as_ref().is_none_or(|bound| {
            !resolver
                .registry
                .bound_satisfied(member, bound, resolver.bound_context().overlay)
        })
    });
    if let Some(member) = unhashable {
        diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "cannot hash unions containing `{}`",
                display_resolution(member, resolver.registry),
            ),
            format!(
                "`{}` does not implement `Hash`",
                display_resolution(member, resolver.registry),
            ),
            call_span,
        ));
    }
    int_ty
}

fn protocol_bound(name: &str, registry: &GlobalRegistry) -> Option<ResolvedProtocolBound> {
    let identifier = Identifier::new("Global", vec![name.to_string()]);
    registry
        .lookup(&identifier)
        .map(|(protocol_id, _)| ResolvedProtocolBound {
            args: Vec::new(),
            protocol_id,
        })
}

/// The first tuple element or union member (at any depth) whose
/// nominal type lacks `Equality`, so the diagnostic can name it.
/// Functions and type params always compare, so they never surface.
fn first_part_without_equality(ty: &ResolvedType, resolver: &Resolver<'_>) -> Option<ResolvedType> {
    let structural = peel_alias(ty, resolver.registry);
    let parts = match &structural {
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => elements,
        ResolvedType::Union(members) => members,
        ResolvedType::Named {
            resolution: Resolution::Global(_),
            ..
        } if !type_supports_equality(&structural, resolver.bound_context()) => {
            return Some(ty.clone());
        }
        _ => return None,
    };
    parts
        .iter()
        .find_map(|part| first_part_without_equality(part, resolver))
}

/// Shared zero-argument validation for the `Debug` family.
fn zero_arg_return(
    shape: StructuralShape,
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
            format!(
                "{} `{method}` takes no arguments, got {}",
                shape.label(),
                args.len()
            ),
            call_span,
        ));
    }
    return_ty
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
