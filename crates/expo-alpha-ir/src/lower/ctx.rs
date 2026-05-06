//! Per-function lowering context: counters, [`CFGBuilder`], and the
//! `value -> IRType` index every recursive helper threads through.
//!
//! No language-aware logic lives here — this is the bookkeeping
//! layer the rest of the [`crate::lower`] modules sit on top of.
//!
//! Two types live here together because they're co-evolving and
//! never used independently:
//!
//! - [`FnLowerCtx`] owns mutable lowering state.
//! - [`FlowResult`] is the return shape every `lower_*` helper
//!   produces, distinguishing "flow continues at this block with
//!   this value" from "flow terminated already (e.g. via early
//!   `return`)".

use std::collections::{BTreeMap, HashSet};

use crate::cfg::CFGBuilder;
use crate::function::{IRBasicBlock, IRBlockId};
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

/// The shape every `lower_*` helper returns. `Open` carries the
/// trailing value (when the construct produces one) and the block
/// where flow continues; `Closed` signals that an inner statement
/// already terminated the function (the only path today is
/// `Statement::Return`). Closed branches don't fall through to a
/// surrounding merge block — the caller's wiring sees
/// `FlowResult::Closed` directly and skips the fall-through wiring
/// it would otherwise emit.
#[derive(Debug, Clone)]
pub(crate) enum FlowResult {
    Open {
        value: Option<ValueId>,
        block: IRBlockId,
    },
    Closed,
}

/// Per-function lowering context. Owns the [`CFGBuilder`] plus the
/// `ValueId` / `IRBlockId` counters and a `value -> IRType` index
/// callers consult to derive operator result types and the function's
/// return type without re-querying the typecheck registry.
///
/// `entry_block` is set as soon as the function's first block is
/// created (before parameter promotion), so any later body lowering
/// step can append [`crate::function::IRInstruction::LocalDecl`]s
/// into the entry regardless of the currently-open block.
/// `declared_locals` is the per-function set of [`IRLocalId`]s that
/// have already been declared, so reassignments emit a single `LocalDecl`
/// followed by repeated `LocalWrite`s rather than a fresh decl per
/// write.
///
/// One context per `IRFunction` (or per script body). Discarded after
/// the function's blocks are extracted via [`Self::into_blocks`];
/// downstream consumers (seal, backends) build their own indices.
pub(crate) struct FnLowerCtx {
    pub(crate) cfg: CFGBuilder,
    next_value: u32,
    next_block: u32,
    value_types: BTreeMap<ValueId, IRType>,
    entry_block: Option<IRBlockId>,
    declared_locals: HashSet<IRLocalId>,
}

impl FnLowerCtx {
    pub(crate) fn new() -> Self {
        Self {
            cfg: CFGBuilder::new(),
            next_value: 0,
            next_block: 0,
            value_types: BTreeMap::new(),
            entry_block: None,
            declared_locals: HashSet::new(),
        }
    }

    /// Mint a fresh `ValueId` and record its `IRType`.
    pub(crate) fn fresh_value(&mut self, ty: IRType) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        self.value_types.insert(id, ty);
        id
    }

    /// Mint a fresh `IRBlockId` and add the corresponding empty
    /// block to the [`CFGBuilder`]. The first block created against
    /// this context is recorded as [`Self::entry_block`] so later
    /// body lowering can append `LocalDecl`s back into the entry
    /// regardless of the currently-open block.
    pub(crate) fn fresh_block(&mut self, label: impl Into<String>) -> IRBlockId {
        let id = IRBlockId(self.next_block);
        self.next_block += 1;
        self.cfg.add_block(id, label);
        if self.entry_block.is_none() {
            self.entry_block = Some(id);
        }
        id
    }

    /// The entry block of the function being lowered. Panics if
    /// called before [`Self::fresh_block`] — every consumer (param
    /// promotion, body-assignment lowering) sequences after entry
    /// creation.
    pub(crate) fn entry_block(&self) -> IRBlockId {
        self.entry_block.expect(
            "alpha IR lower: entry_block consulted before any block was opened — \
             lower_function ordering bug",
        )
    }

    /// Has `local` been declared yet in this function? `LocalWrite`s
    /// without a prior `LocalDecl` need to seed one in the entry
    /// block; subsequent writes skip the seed.
    pub(crate) fn local_is_declared(&self, local: IRLocalId) -> bool {
        self.declared_locals.contains(&local)
    }

    /// Record that `local` has been declared. Returns `true` if this
    /// is the first declaration (so the caller should emit the
    /// `LocalDecl`); `false` if `local` was already in the set.
    pub(crate) fn mark_local_declared(&mut self, local: IRLocalId) -> bool {
        self.declared_locals.insert(local)
    }

    /// Lookup the recorded `IRType` for `id`. Panics on a miss —
    /// every emitted `ValueId` registers its type at allocation time,
    /// so a miss is a lowering bug.
    pub(crate) fn type_of(&self, id: ValueId) -> IRType {
        self.value_types
            .get(&id)
            .cloned()
            .unwrap_or_else(|| panic!("alpha IR lower: missing type for {id} — lowering bug"))
    }

    /// Consume the context and return the accumulated block list.
    /// Asserts via `CFGBuilder`'s closed-set that every block has had
    /// a real terminator stamped — an unclosed block reaching the
    /// caller is a lowering bug.
    pub(crate) fn into_blocks(self) -> Vec<IRBasicBlock> {
        let (blocks, closed) = self.cfg.into_blocks_with_closed();
        for block in &blocks {
            if !closed.contains_key(&block.id) {
                panic!(
                    "alpha IR lower: block {} ({}) was opened but never had its terminator set — \
                     lowering bug",
                    block.id, block.label,
                );
            }
        }
        blocks
    }
}
