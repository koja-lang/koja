//! Match-arm pattern resolution.
//!
//! Admits `Wildcard`, `Binding`, primitive `Literal`, `EnumUnit`,
//! `EnumTuple` / `EnumStruct` / `Struct` with arbitrary nested
//! pattern shapes in payload / field positions (literals, nested
//! enums, nested structs, or-alternatives), `Or` (alternatives
//! restricted to literal / EnumUnit, no bindings), and
//! `Constructor` shorthand (`Some(x)`, `None`, `Ok(x)`, ...).
//! `Constructor` rewrites in place to the corresponding `EnumTuple`
//! / `EnumUnit` after looking the variant up on the subject's enum,
//! so seal / generics-substitute / lowering never see the shape.
//! Every other shape diagnoses a feature gap. Returns
//! [`PatternCoverage`] so [`super::match_expr::resolve_match`] can
//! run the catch-all-or-exhaustiveness check without re-walking the
//! arm.
//!
//! # Module layout
//!
//! - [`constructor`]: `Some(x)` / `None` / `Ok(x)` shorthand,
//!   rewritten in place to its qualified form.
//! - [`enums`]: `EnumUnit` / `EnumTuple` / `EnumStruct` shapes,
//!   plus the shared enum-lookup / generic-substitution helpers.
//! - [`structs`]: plain-struct destructure and the field-pattern
//!   walker shared with struct-shaped enum variants.
//! - [`or_pattern`]: `A | B | C` alternatives with intra-or-pattern
//!   reachability warnings.
//! - [`literals`]: literal-vs-subject type checking and the
//!   canonical literal-string representation used by the cross-arm
//!   reachability machinery.

mod binary;
mod constructor;
mod enums;
mod literals;
mod or_pattern;
mod structs;

use koja_ast::ast::{Diagnostic, Pattern};
use koja_ast::identifier::{Resolution, ResolvedType};
use koja_ast::labels::pattern_span;

use super::ctx::Resolver;
use super::types::{display_resolution, is_primitive, names_struct, peel_alias, types_equivalent};
use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::registry::{EnumDefinition, GlobalKind, GlobalRegistry};

use literals::literal_repr;

/// One variant a pattern covers, with `full = true` when the inner
/// pattern is itself a catch-all (every inhabitant of the variant
/// matches). Narrowing inner patterns set `full = false`.
pub(super) struct VariantWitness {
    pub full: bool,
    pub tag: u32,
}

/// What a pattern admits at runtime. Drives the
/// catch-all-or-exhaustiveness rule in
/// [`super::match_expr::resolve_match`].
pub(super) enum PatternCoverage {
    /// Wildcard / binding: admits every value of the subject.
    CatchAll,
    /// `EnumUnit` / `EnumTuple` (or an `Or` of those): admits the
    /// listed variant tags. Exhaustiveness uses every witness,
    /// reachability uses only the `full` ones.
    Variants(Vec<VariantWitness>),
    /// `TypedBinding` matched against a union subject: admits
    /// values whose runtime tag corresponds to `member`. Drives
    /// union exhaustiveness in [`super::match_expr::resolve_match`].
    UnionMember(ResolvedType),
    /// Literal patterns and `Or`s of literals. The arm fires for a
    /// specific runtime value but does not contribute to enum
    /// exhaustiveness. Primitive subjects use the strict
    /// catch-all-required rule.
    Other,
}

pub(super) fn resolve_pattern(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    // Mirror of the value-side rewrite: `A.B { … }` becomes a `Struct`
    // pattern when the path names a struct.
    rewrite_dotted_struct_pattern(pat, resolver);
    match pat {
        Pattern::Binding { local_id, name, .. } => {
            let id = resolver.scope.declare(name, subject_ty.clone());
            *local_id = Some(id);
            PatternCoverage::CatchAll
        }
        Pattern::Constructor { .. } => {
            constructor::resolve_constructor_pattern(pat, subject_ty, resolver, diagnostics)
        }
        Pattern::EnumStruct {
            fields,
            span,
            type_path,
            variant,
            ..
        } => enums::resolve_enum_struct_pattern(
            type_path,
            variant,
            fields,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumTuple {
            elements,
            span,
            type_path,
            variant,
            ..
        } => enums::resolve_enum_tuple_pattern(
            type_path,
            variant,
            elements,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumUnit {
            span,
            type_path,
            variant,
            ..
        } => enums::resolve_enum_unit_pattern(
            type_path,
            variant,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Literal {
            literal_coercion,
            span,
            value,
        } => {
            literals::check_literal_matches_subject(
                value,
                literal_coercion,
                subject_ty,
                *span,
                resolver,
                diagnostics,
            );
            PatternCoverage::Other
        }
        Pattern::Or { patterns, span } => {
            or_pattern::resolve_or_pattern(patterns, subject_ty, *span, resolver, diagnostics)
        }
        Pattern::Struct {
            fields,
            span,
            type_path,
            ..
        } => structs::resolve_struct_pattern(
            type_path,
            fields,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Wildcard { .. } => PatternCoverage::CatchAll,
        Pattern::Binary { segments, span } => {
            binary::resolve_binary_pattern(segments, subject_ty, *span, resolver, diagnostics)
        }
        Pattern::List { .. } => {
            diagnostics.push(Diagnostic::error(
                "typecheck does not yet support list patterns (blocked on IR \
                 list ops + a stable `List<T>` layout)",
                pattern_span(pat),
            ));
            PatternCoverage::Other
        }
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            type_expr,
            span,
        } => {
            let resolved = resolve_type_expr(
                type_expr,
                TypeParamScope::new(resolver.type_param_owners),
                resolver.resolution_scope(),
                diagnostics,
            );
            if !resolved.is_resolved() {
                return PatternCoverage::Other;
            }
            let peeled_subject = peel_alias(subject_ty, resolver.registry);
            match &peeled_subject {
                ResolvedType::Union(members) => {
                    if !members
                        .iter()
                        .any(|m| types_equivalent(m, &resolved, resolver.registry))
                    {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "type `{}` is not a member of union `{}`",
                                display_resolution(&resolved, resolver.registry),
                                display_resolution(subject_ty, resolver.registry),
                            ),
                            *span,
                        ));
                        return PatternCoverage::Other;
                    }
                }
                _ if subject_ty.is_resolved()
                    && !types_equivalent(subject_ty, &resolved, resolver.registry) =>
                {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "typed-binding pattern requires a union subject; \
                             got `{}`",
                            display_resolution(subject_ty, resolver.registry),
                        ),
                        *span,
                    ));
                    return PatternCoverage::Other;
                }
                _ => {}
            }
            let id = resolver.scope.declare(name, resolved.clone());
            *local_id = Some(id);
            *resolved_type = Some(resolved.clone());
            PatternCoverage::UnionMember(resolved)
        }
    }
}

/// Rewrite an `EnumStruct` pattern whose path names a struct into a
/// `Struct` pattern. A no-op for real enum struct-variant patterns.
fn rewrite_dotted_struct_pattern(pat: &mut Pattern, resolver: &Resolver<'_>) {
    let Pattern::EnumStruct {
        type_path,
        variant,
        span,
        ..
    } = pat
    else {
        return;
    };
    let mut full = type_path.clone();
    full.push(variant.clone());
    if !names_struct(&full, resolver.resolution_scope()) {
        return;
    }
    let span = *span;
    let Pattern::EnumStruct {
        mut type_path,
        variant,
        fields,
        ..
    } = std::mem::replace(pat, Pattern::Wildcard { span })
    else {
        unreachable!("guarded by the match above");
    };
    type_path.push(variant);
    *pat = Pattern::Struct {
        type_path,
        fields,
        span,
    };
}

/// True when `subject_ty` resolves to a primitive admitted as a
/// literal-comparable subject (`Bool` / `Int` / `Float` / `String`).
/// Patterns made entirely of catch-alls bypass this check at the
/// `resolve_match` level. Any subject type is fine when the only
/// patterns are wildcards / bindings.
pub(super) fn is_match_subject_primitive(
    subject_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    const PRIMITIVES: &[&str] = &[
        "Bool", "Float", "Float32", "Float64", "Int", "Int16", "Int32", "Int64", "Int8", "String",
        "UInt16", "UInt32", "UInt64", "UInt8",
    ];
    PRIMITIVES
        .iter()
        .any(|name| is_primitive(subject_ty, registry, name))
}

/// Lookup the [`EnumDefinition`] whose `Global(id)` head matches
/// `subject_ty`. Returns `None` for non-enum / unresolved subjects.
/// Used by [`super::match_expr::resolve_match`] to drive the
/// structural exhaustiveness check.
pub(super) fn match_subject_enum<'a>(
    subject_ty: &ResolvedType,
    registry: &'a GlobalRegistry,
) -> Option<&'a EnumDefinition> {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = subject_ty
    else {
        return None;
    };
    let entry = registry.get(*id)?;
    let GlobalKind::Enum(definition) = &entry.kind else {
        return None;
    };
    definition.as_ref()
}

/// Walk `pattern` and append a canonical string representation of
/// every `Literal` / `Or`-of-literal alternative it contains. Used
/// by [`super::match_expr::resolve_match`] to detect cross-arm
/// literal duplication (`1 -> _, 1 -> _`) without re-walking the
/// pattern's enum / struct / binding shapes.
pub(super) fn collect_literal_reprs(pattern: &Pattern, out: &mut Vec<String>) {
    match pattern {
        Pattern::Literal { value, .. } => out.push(literal_repr(value)),
        Pattern::Or { patterns, .. } => {
            for alt in patterns {
                collect_literal_reprs(alt, out);
            }
        }
        _ => {}
    }
}
