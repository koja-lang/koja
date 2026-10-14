//! Shared core for type-parameter inference at struct, enum, bare-call,
//! and method-call sites. Three helpers cover the work every site does:
//!
//! - [`unify_pairs`] walks `(template, actual)` pairs into a [`Substitution`],
//!   routing conflicts through a caller-supplied closure so each site can
//!   fold conflicts into its own diagnostic shape.
//! - [`fill_from_expected`] tries to unify a template against a hint
//!   without ever overriding existing bindings. The expected type is
//!   advisory, never authoritative.
//! - [`finalize_inference`] surfaces phantom-param diagnostics and
//!   bound-check diagnostics for every callee scope in one pass.
//!
//! The four `infer_*_type_args` sites compose these around their own
//! shape-specific pair extraction.

use koja_ast::ast::Diagnostic;
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::ctx::{BoundContext, Callee};
use super::types::{peel_alias, verify_bounds};
use crate::pipeline::unify::{Conflict, Substitution, unify_into};
use crate::registry::GlobalRegistry;

/// Where the inference is happening, used to shape the
/// "cannot infer type parameter" diagnostic per site. All four
/// variants share the same `cannot infer type parameter X of Y from
/// Z` skeleton so a user reading the message learns the same shape
/// regardless of whether they hit a free fn, struct, or enum.
pub(super) enum PhantomContext<'a> {
    Arguments,
    Fields,
    Payload(&'a str),
    UnitVariant(&'a str),
}

impl PhantomContext<'_> {
    fn diagnostic_for(&self, type_param_name: &str, callee_label: &str) -> String {
        let source = match self {
            Self::Arguments => "the supplied arguments".to_string(),
            Self::Fields => "the supplied fields".to_string(),
            Self::Payload(variant_name) => format!("the supplied `{variant_name}` payload"),
            Self::UnitVariant(variant_name) => format!("unit variant `{variant_name}`"),
        };
        format!(
            "typecheck cannot infer type parameter `{type_param_name}` of \
             `{callee_label}` from {source}",
        )
    }
}

/// Unify each `(template, actual, label)` triple into `subst`. Skips
/// pairs whose `actual` is unresolved (upstream diagnosed). Routes
/// [`Conflict`]s through `on_conflict` together with the per-pair
/// `label`, letting each call site thread per-pair diagnostic data
/// (typically a [`Span`]) through to its own conflict-message shape.
/// Sites without per-pair labels can pass `()`.
pub(super) fn unify_pairs<'a, T, I, F>(
    pairs: I,
    subst: &mut Substitution,
    registry: &GlobalRegistry,
    mut on_conflict: F,
) where
    I: IntoIterator<Item = (&'a ResolvedType, &'a ResolvedType, T)>,
    F: FnMut(Conflict, T),
{
    for (template, actual, label) in pairs {
        if !actual.is_resolved() {
            continue;
        }
        if let Err(conflict) = unify_into(template, actual, subst, registry) {
            on_conflict(conflict, label);
        }
    }
}

/// Try to fill empty slots of `subst` by unifying `template` against
/// `actual` (an expected type from the surrounding context). On any
/// conflict the attempt is discarded, already-bound slots stay
/// authoritative. Use for bidirectional return-type / element-type /
/// payload-type hints.
pub(super) fn fill_from_expected(
    template: &ResolvedType,
    actual: &ResolvedType,
    subst: &mut Substitution,
    registry: &GlobalRegistry,
) {
    if !actual.is_resolved() {
        return;
    }
    let mut scratch = subst.clone();
    if unify_into(template, actual, &mut scratch, registry).is_ok() {
        *subst = scratch;
    }
}

/// Complete `subst` from protocol bounds, then surface phantom-param
/// and bound-check diagnostics for every callee scope in `callees`.
/// Each callee whose owner is in `subst` walks its slots. Every
/// `None` slot emits the per-context "cannot infer" message, and
/// every filled slot is bound-checked against the callee's declared
/// bounds via [`verify_bounds`]. Out-of-scope callees skip silently,
/// since the substitution does not own them.
pub(super) fn finalize_inference(
    callees: &[Callee<'_>],
    subst: &mut Substitution,
    context: &PhantomContext<'_>,
    span: Span,
    ctx: BoundContext<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    fill_from_bounds(callees, subst, ctx);
    for callee in callees {
        if !subst.owns(callee.id) {
            continue;
        }
        for (index, slot) in subst.slots(callee.id).iter().enumerate() {
            if slot.is_some() {
                continue;
            }
            let type_param_name = callee
                .type_params
                .get(index)
                .map(String::as_str)
                .unwrap_or("?");
            diagnostics.push(Diagnostic::error(
                context.diagnostic_for(type_param_name, callee.label),
                span,
            ));
        }
        verify_bounds(*callee, subst, span, ctx, diagnostics);
    }
}

/// Fill empty slots from protocol bounds. A slot that appears only
/// as an argument of another slot's bound, `E` in `R: Reader<E>`,
/// has no argument to bind from, and the bounded type's conformance
/// carries the answer. Each pass reads the bounds of every filled
/// slot, looks up the protocol args the filled type conforms with,
/// and unifies them into the bound's args. A filled slot never
/// changes, and a conflict leaves the bound for [`verify_bounds`] to
/// report. Passes repeat while one fills something, since a fill can
/// unlock a bound on another slot.
fn fill_from_bounds(callees: &[Callee<'_>], subst: &mut Substitution, ctx: BoundContext<'_>) {
    let registry = ctx.registry;
    loop {
        let filled_before = filled_slots(callees, subst);
        for callee in callees {
            if !subst.owns(callee.id) {
                continue;
            }
            let Some(bounds) = registry.type_param_bounds(callee.id) else {
                continue;
            };
            for (index, slot_bounds) in bounds.iter().enumerate() {
                let Some(filled) = subst.slots(callee.id).get(index).cloned().flatten() else {
                    continue;
                };
                for bound in slot_bounds.iter().filter(|bound| !bound.args.is_empty()) {
                    let Some(protocol_args) =
                        conforming_protocol_args(&filled, bound.protocol_id, ctx)
                    else {
                        continue;
                    };
                    for (template, actual) in bound.args.iter().zip(&protocol_args) {
                        fill_from_expected(template, actual, subst, registry);
                    }
                }
            }
        }
        if filled_slots(callees, subst) == filled_before {
            return;
        }
    }
}

fn filled_slots(callees: &[Callee<'_>], subst: &Substitution) -> usize {
    callees
        .iter()
        .filter(|callee| subst.owns(callee.id))
        .map(|callee| subst.slots(callee.id).iter().flatten().count())
        .sum()
}

/// The protocol args `ty` conforms to `protocol_id` with, when one
/// answer exists. A named type reads its covering conformance
/// record. A type param reads the one granted bound with that
/// protocol. Structural shapes conform without protocol args.
fn conforming_protocol_args(
    ty: &ResolvedType,
    protocol_id: GlobalRegistryId,
    ctx: BoundContext<'_>,
) -> Option<Vec<ResolvedType>> {
    let registry = ctx.registry;
    match peel_alias(ty, registry) {
        ResolvedType::Named {
            resolution: Resolution::Global(target_id),
            type_args,
        } => registry.conformance_args(target_id, protocol_id, &type_args, ctx.overlay),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => {
            let granted = registry.granted_bounds(owner, index, ctx.overlay);
            let mut matching = granted
                .iter()
                .filter(|bound| bound.protocol_id == protocol_id);
            let bound = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            Some(bound.args.clone())
        }
        _ => None,
    }
}
