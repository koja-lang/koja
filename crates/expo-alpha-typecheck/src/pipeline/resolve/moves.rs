//! Per-function move-state ledger for use-after-move enforcement.
//!
//! [`MoveLedger`] stamps each local with `Moved { at }` when it's
//! consumed (assignment RHS for non-`Copy` types, `move` parameter
//! arg, `move self` receiver). Subsequent reads consult the ledger
//! through [`super::ctx::Resolver`] and diagnose accordingly.
//! Branches use [`MoveLedger::snapshot`] / [`restore`] /
//! [`merge_branches`] so a move in one arm is visible at the join
//! (pessimistic union: `MaybeMoved` when only some branches moved
//! the slot, `Moved` when every branch moved it).
//!
//! Fresh writes through [`resolve_assignment`] route through
//! [`MoveLedger::clear`] so a reassignment to a previously-moved
//! name restores the slot to live.
//!
//! [`resolve_assignment`]: super::statements::resolve_assignment

use std::collections::BTreeMap;

use expo_ast::ast::{Expr, ExprKind};
use expo_ast::identifier::LocalId;
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::types::is_copy_type;

/// Move status of a single local slot.
#[derive(Clone, Copy, Debug)]
pub(super) enum MoveState {
    /// Unconditional move at `at`.
    Moved { at: Span },
    /// Moved in some but not all preceding branches.
    MaybeMoved { at: Span },
}

impl MoveState {
    pub(super) fn span(self) -> Span {
        match self {
            MoveState::Moved { at } | MoveState::MaybeMoved { at } => at,
        }
    }
}

/// Per-function move-state map. Absence from the map means
/// `Available` (live).
#[derive(Debug, Default)]
pub(super) struct MoveLedger {
    states: BTreeMap<LocalId, MoveState>,
}

impl MoveLedger {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn state(&self, local: LocalId) -> Option<MoveState> {
        self.states.get(&local).copied()
    }

    /// Stamp `local` as unconditionally moved at `span`. Overwrites
    /// any prior state — a re-move replaces the prior span so the
    /// most recent move is what diagnostics point at.
    pub(super) fn mark_moved(&mut self, local: LocalId, span: Span) {
        self.states.insert(local, MoveState::Moved { at: span });
    }

    /// Reset `local` to live (fresh write through an assignment).
    pub(super) fn clear(&mut self, local: LocalId) {
        self.states.remove(&local);
    }

    pub(super) fn snapshot(&self) -> MoveLedgerSnapshot {
        MoveLedgerSnapshot {
            states: self.states.clone(),
        }
    }

    pub(super) fn restore(&mut self, snapshot: MoveLedgerSnapshot) {
        self.states = snapshot.states;
    }

    /// Pessimistic merge across `branches`: a local lands as
    /// `Moved` when every branch ended with strict `Moved`,
    /// `MaybeMoved` when at least one branch moved it but the
    /// remaining branches either also moved (mixed `Moved` /
    /// `MaybeMoved`) or didn't touch the slot, absent (live) when
    /// no branch touched it. Diagnostics point at the first move
    /// span seen across branches.
    pub(super) fn merge_branches(&mut self, branches: Vec<MoveLedgerSnapshot>) {
        if branches.is_empty() {
            return;
        }
        let mut tally: BTreeMap<LocalId, BranchTally> = BTreeMap::new();
        for branch in &branches {
            for (local, state) in &branch.states {
                let entry = tally.entry(*local).or_default();
                if matches!(state, MoveState::Moved { .. }) {
                    entry.strict += 1;
                }
                if entry.first_span.is_none() {
                    entry.first_span = Some(state.span());
                }
            }
        }
        let total = branches.len();
        let mut merged = BTreeMap::new();
        for (local, t) in tally {
            let span = t.first_span.expect("merge: branch states carry spans");
            let state = if t.strict == total {
                MoveState::Moved { at: span }
            } else {
                MoveState::MaybeMoved { at: span }
            };
            merged.insert(local, state);
        }
        self.states = merged;
    }
}

#[derive(Default)]
struct BranchTally {
    /// Branches where the local is strictly `Moved` (no maybe).
    strict: usize,
    /// First move span seen across branches; reported by the
    /// post-join diagnostic.
    first_span: Option<Span>,
}

#[derive(Clone, Debug)]
pub(super) struct MoveLedgerSnapshot {
    states: BTreeMap<LocalId, MoveState>,
}

/// If `expr` is a bare-identifier read of a non-`Copy` local,
/// return the source [`LocalId`] so the caller can stamp it
/// `Moved`. Returns `None` for fresh rvalues (calls, literals,
/// field projections, etc.), `Copy`-typed locals (numerics, Bool,
/// function pointers), and globals.
pub(super) fn move_source_local(expr: &Expr, resolver: &Resolver<'_>) -> Option<LocalId> {
    let ExprKind::Ident { name, .. } = &expr.kind else {
        return None;
    };
    let (local_id, ty) = resolver.scope.lookup(name)?;
    if is_copy_type(ty, resolver.registry) {
        return None;
    }
    Some(local_id)
}
