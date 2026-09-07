//! Trial resolution with rollback. A site that resolves sibling
//! expressions with no outer hint can resolve one sibling on trial,
//! read the type it produced, and either keep that pass or roll back
//! and resolve again with a hint learned from the other sibling.
//!
//! Rollback restores the AST from a clone and the visible name map
//! from a [`LocalScope`] snapshot. `LocalId`s minted during the trial
//! stay in the id table by design (nothing reaches them once the AST
//! is restored), and the per-body `Resolver` counters are balanced
//! by each body walk, so nothing else needs saving.
//!
//! [`LocalScope`]: crate::pipeline::local_scope::LocalScope

use koja_ast::ast::Diagnostic;

use super::ctx::Resolver;
use crate::pipeline::local_scope::LocalScopeSnapshot;

pub(super) struct Speculation<T: Clone> {
    diagnostics: Vec<Diagnostic>,
    pristine: T,
    scope: LocalScopeSnapshot,
}

impl<T: Clone> Speculation<T> {
    /// Capture `node` and the current scope before a trial pass.
    pub(super) fn begin(node: &T, resolver: &Resolver<'_>) -> Self {
        Self {
            diagnostics: Vec::new(),
            pristine: node.clone(),
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
    pub(super) fn rollback(self, node: &mut T, resolver: &mut Resolver<'_>) {
        *node = self.pristine;
        resolver.scope.restore(self.scope);
    }
}
