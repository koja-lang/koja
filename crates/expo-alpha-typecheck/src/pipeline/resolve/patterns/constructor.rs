//! Constructor-shorthand pattern (`Some(x)` / `None` / `Ok(x)`).
//!
//! Resolves the variant on the subject's enum and rewrites the
//! `Pattern::Constructor` AST node in place to its qualified form
//! (`Pattern::EnumTuple` for tuple variants, `Pattern::EnumUnit` for
//! unit variants), then re-enters [`super::resolve_pattern`] so the
//! rewritten shape flows through the same arity / element-binding /
//! variant-tag machinery enum patterns already use. Seal,
//! generics-substitute, and IR lowering therefore never see a
//! `Pattern::Constructor`.

use expo_ast::ast::{Diagnostic, Pattern};
use expo_ast::identifier::{Resolution, ResolvedType};

use super::super::ctx::Resolver;
use super::super::types::display_resolution;
use super::{PatternCoverage, resolve_pattern};
use crate::registry::{GlobalKind, ResolvedVariantData};

pub(super) fn resolve_constructor_pattern(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    match constructor_metadata(pat, subject_ty, resolver, diagnostics) {
        Ok(metadata) => {
            let Pattern::Constructor {
                name,
                elements,
                span,
                ..
            } = pat
            else {
                unreachable!("resolve_constructor_pattern dispatched on non-Constructor");
            };
            let span = *span;
            let name_owned = std::mem::take(name);
            let elements_owned = std::mem::take(elements);
            *pat = match metadata.kind {
                ConstructorRewrite::Unit => Pattern::EnumUnit {
                    type_path: metadata.type_path,
                    variant: name_owned,
                    span,
                    resolved_type: None,
                },
                ConstructorRewrite::Tuple => Pattern::EnumTuple {
                    type_path: metadata.type_path,
                    variant: name_owned,
                    elements: elements_owned,
                    span,
                    resolved_type: None,
                },
            };
            resolve_pattern(pat, subject_ty, resolver, diagnostics)
        }
        Err(()) => {
            let Pattern::Constructor { elements, .. } = pat else {
                unreachable!("resolve_constructor_pattern dispatched on non-Constructor");
            };
            for element in elements.iter_mut() {
                resolve_pattern(element, &ResolvedType::unresolved(), resolver, diagnostics);
            }
            PatternCoverage::Other
        }
    }
}

enum ConstructorRewrite {
    Tuple,
    Unit,
}

struct ConstructorMetadata {
    kind: ConstructorRewrite,
    type_path: Vec<String>,
}

/// Resolve the subject's enum, look the variant up by `name`, and
/// pick the rewrite shape. Splits out so the immutable registry
/// borrow ends before [`resolve_constructor_pattern`] re-borrows
/// `pat` mutably to perform the swap.
fn constructor_metadata(
    pat: &Pattern,
    subject_ty: &ResolvedType,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ConstructorMetadata, ()> {
    let Pattern::Constructor {
        name,
        elements,
        span,
        ..
    } = pat
    else {
        unreachable!("constructor_metadata called on non-Constructor pattern");
    };
    let span = *span;
    if !subject_ty.is_resolved() {
        // An upstream error already fired (subject didn't resolve);
        // stay silent here so the user only sees the original cause.
        return Err(());
    }
    let ResolvedType::Named {
        resolution: Resolution::Global(enum_id),
        ..
    } = subject_ty
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "constructor pattern `{name}(...)` requires an enum subject (got `{}`)",
                display_resolution(subject_ty, resolver.registry),
            ),
            span,
        ));
        return Err(());
    };
    let enum_id = *enum_id;
    let Some(entry) = resolver.registry.get(enum_id) else {
        diagnostics.push(Diagnostic::error(
            "internal: subject enum id is not registered".to_string(),
            span,
        ));
        return Err(());
    };
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "constructor pattern `{name}(...)` requires an enum subject (got `{}`, a {})",
                entry.identifier,
                entry.kind.label(),
            ),
            span,
        ));
        return Err(());
    };
    let label = entry.identifier.to_string();
    let Some((_index, variant)) = definition.lookup_variant(name) else {
        let known: Vec<String> = definition.variants.iter().map(|v| v.name.clone()).collect();
        diagnostics.push(Diagnostic::error(
            format!(
                "enum `{label}` has no variant `{name}` (declared variants: `{}`)",
                known.join("`, `"),
            ),
            span,
        ));
        return Err(());
    };
    let kind = match &variant.data {
        ResolvedVariantData::Unit => {
            if !elements.is_empty() {
                let n = elements.len();
                diagnostics.push(Diagnostic::error(
                    format!(
                        "variant `{label}.{name}` is a unit variant and takes no payload \
                         (got {n} positional element{})",
                        if n == 1 { "" } else { "s" },
                    ),
                    span,
                ));
                return Err(());
            }
            ConstructorRewrite::Unit
        }
        ResolvedVariantData::Tuple(_) => ConstructorRewrite::Tuple,
        ResolvedVariantData::Struct(_) => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "variant `{label}.{name}` is a struct variant; use `{label}.{name}{{...}}` \
                     syntax instead of `{name}(...)`"
                ),
                span,
            ));
            return Err(());
        }
    };
    let type_path = vec![entry.identifier.last().to_string()];
    Ok(ConstructorMetadata { kind, type_path })
}
