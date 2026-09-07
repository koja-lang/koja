//! Trial resolution with rollback. A site that resolves sibling
//! expressions with no outer hint can resolve one sibling on trial,
//! read the type it produced, and either keep that pass or roll back
//! and resolve again with a hint learned from the other sibling.
//!
//! Rollback restores the AST from a saved copy and the visible name
//! map from a [`LocalScope`] snapshot. `LocalId`s minted during the
//! trial stay in the id table by design (nothing reaches them once
//! the AST is restored), and the per-body `Resolver` counters are
//! balanced by each body walk, so nothing else needs saving.
//!
//! [`resolve_arms`] is the driver every value-producing branch form
//! (`match`, `if`, `cond`, `?:`) shares. Each form describes its arms
//! as an [`ArmSet`] and the driver decides whether a second pass is
//! worth running.
//!
//! [`LocalScope`]: crate::pipeline::local_scope::LocalScope

use koja_ast::ast::Diagnostic;
use koja_ast::identifier::ResolvedType;

use super::control_flow::is_never;
use super::ctx::Resolver;
use super::types::merge_partial;
use crate::pipeline::local_scope::LocalScopeSnapshot;
use crate::registry::GlobalRegistry;

/// An AST region that can be saved before a trial pass and put back
/// after it.
pub(super) trait Restorable {
    type Saved;
    fn save(&self) -> Self::Saved;
    fn restore(&mut self, saved: Self::Saved);
}

impl<T: Clone> Restorable for &mut T {
    type Saved = T;

    fn save(&self) -> T {
        (**self).clone()
    }

    fn restore(&mut self, saved: T) {
        **self = saved;
    }
}

pub(super) struct Speculation<S: Restorable> {
    diagnostics: Vec<Diagnostic>,
    saved: S::Saved,
    scope: LocalScopeSnapshot,
}

impl<S: Restorable> Speculation<S> {
    /// Capture `node` and the current scope before a trial pass.
    pub(super) fn begin(node: &S, resolver: &Resolver<'_>) -> Self {
        Self {
            diagnostics: Vec::new(),
            saved: node.save(),
            scope: resolver.scope.snapshot(),
        }
    }

    /// Scratch sink for the trial pass's diagnostics.
    pub(super) fn diagnostics(&mut self) -> &mut Vec<Diagnostic> {
        &mut self.diagnostics
    }

    /// Keep the trial pass. Its diagnostics move to the real sink.
    pub(super) fn commit(self, diagnostics: &mut Vec<Diagnostic>) {
        diagnostics.extend(self.diagnostics);
    }

    /// Discard the trial pass. Restores `node` and the scope so the
    /// caller can resolve again with a hint.
    pub(super) fn rollback(self, node: &mut S, resolver: &mut Resolver<'_>) {
        node.restore(self.saved);
        resolver.scope.restore(self.scope);
    }
}

/// One arm's label for join diagnostics paired with its tail type.
pub(super) type ArmTail = (String, ResolvedType);

/// The arms of one branch form. `resolve` walks every arm against
/// `hint` and reports the tails in arm order.
pub(super) trait ArmSet: Restorable {
    fn resolve(
        &mut self,
        hint: Option<&ResolvedType>,
        resolver: &mut Resolver<'_>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Vec<ArmTail>;
}

/// Resolve `arms` against `expected`, letting the arms hint each
/// other when there is no outer hint. `x = match ...` with
/// `Result.Ok(true)` in one arm and `Result.Err("nope")` in another
/// leaves each arm with a hole the other arm fills. A trial pass
/// collects the partial tails, and when merging them yields a
/// complete type the arms resolve again with that type as the hint.
/// The trial's diagnostics are dropped in that case, so only pass
/// two reports.
pub(super) fn resolve_arms(
    arms: &mut impl ArmSet,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ArmTail> {
    if expected.is_some() {
        return arms.resolve(expected, resolver, diagnostics);
    }
    let mut trial = Speculation::begin(arms, resolver);
    let tails = arms.resolve(None, resolver, trial.diagnostics());
    let Some(hint) = sibling_hint(&tails, resolver.registry) else {
        trial.commit(diagnostics);
        return tails;
    };
    trial.rollback(arms, resolver);
    arms.resolve(Some(&hint), resolver, diagnostics)
}

/// Merge the partial arm tails into one complete type. `None` when
/// every tail already resolved, when the shapes disagree, or when a
/// hole survives the merge, since a second pass could not improve
/// on the first in any of those cases.
fn sibling_hint(tails: &[ArmTail], registry: &GlobalRegistry) -> Option<ResolvedType> {
    let mut tails = tails
        .iter()
        .map(|(_, ty)| ty)
        .filter(|ty| !is_never(ty, registry));
    if tails.clone().all(ResolvedType::is_resolved) {
        return None;
    }
    let first = tails.next()?.clone();
    let merged = tails.try_fold(first, |merged, ty| merge_partial(&merged, ty))?;
    merged.is_resolved().then_some(merged)
}
