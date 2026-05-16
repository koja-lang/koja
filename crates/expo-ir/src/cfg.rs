//! [`CFGBuilder`]: an explicit accumulator for a function's
//! control-flow graph during lowering.
//!
//! Lowering passes a `&mut CFGBuilder` plus the currently-open
//! [`IRBlockId`] through every recursive call. Each `lower_*` helper
//! mutates the builder (adds blocks, appends instructions, sets
//! terminators) and returns the new "open" block (or signals a
//! closed-flow with no fall-through). The builder owns no state
//! between calls beyond its block list, so lowering is referentially
//! transparent: given the same `(builder snapshot, open, expr)` it
//! produces the same result.
//!
//! Block ids stay function-unique (minted by [`crate::FnLowerCtx`])
//! so terminator references resolve regardless of which builder
//! produced the block. The builder owns no counters.
//!
//! Adapted from the v1 `CFGBuilder` (`expo-ir/src/cfg.rs`). Trimmed
//! for the feature slice: no loop scoping (`mark_loop`, `LoopExitOp`)
//! since loops aren't lowered yet; no legacy `into_blocks` escape
//! hatch since the pipeline has no walker that drops the closed-set.

use std::collections::HashMap;

use crate::function::{BlockParam, IRBasicBlock, IRBlockId, IRInstruction, IRTerminator};
use crate::types::{IRType, ValueId};

/// Accumulator for a single CFG fragment. Owns the in-progress
/// [`IRBasicBlock`] list and tracks which blocks have had a real
/// terminator set (vs. the placeholder self-branch added at
/// [`Self::add_block`] time).
///
/// A `HashMap<IRBlockId, usize>` shadows the block list so
/// [`Self::block_mut`] / [`Self::append`] / [`Self::set_terminator`]
/// resolve in O(1).
#[derive(Debug, Default)]
pub(crate) struct CFGBuilder {
    blocks: Vec<IRBasicBlock>,
    /// `id -> index into self.blocks`. Maintained alongside `blocks`
    /// to avoid the linear scan v1 paid in `block_mut`.
    by_id: HashMap<IRBlockId, usize>,
    /// Block ids whose terminator has been explicitly set via
    /// [`Self::set_terminator`]. Walks consult this to distinguish
    /// intentional self-branches from the placeholder
    /// `Branch(self)` left when a block is opened but never closed.
    closed: HashMap<IRBlockId, ()>,
}

impl CFGBuilder {
    /// Empty builder. Block / value counters live on
    /// [`crate::FnLowerCtx`] — the builder only owns the block list.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a fresh empty block with the given id and human-readable
    /// label. Caller must have minted `id` via
    /// [`crate::FnLowerCtx::fresh_block`]. The new block starts open:
    /// its terminator slot holds a placeholder `Branch(id)` that
    /// callers must overwrite via [`Self::set_terminator`] before
    /// the block is sealed.
    pub(crate) fn add_block(&mut self, id: IRBlockId, label: impl Into<String>) {
        if self.by_id.contains_key(&id) {
            panic!("CFGBuilder: duplicate block id {id}");
        }
        let index = self.blocks.len();
        self.blocks.push(IRBasicBlock {
            id,
            label: label.into(),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: IRTerminator::branch(id),
        });
        self.by_id.insert(id, index);
    }

    /// Push a typed [`BlockParam`] onto block `id`'s entry signature.
    /// The caller has already minted `dest` via the surrounding
    /// `FnLowerCtx::fresh_value` (so the value-types index is in
    /// sync); this helper just records the param on the block. Panics
    /// if the block is not present.
    pub(crate) fn declare_block_param(&mut self, id: IRBlockId, dest: ValueId, ty: IRType) {
        self.block_mut(id).params.push(BlockParam { dest, ty });
    }

    /// Append `instr` to the block identified by `id`. Panics if the
    /// block is not present in the builder — callers should add the
    /// block via [`Self::add_block`] first.
    pub(crate) fn append(&mut self, id: IRBlockId, instr: IRInstruction) {
        self.block_mut(id).instructions.push(instr);
    }

    /// Overwrite the terminator of the block identified by `id` and
    /// mark the block as closed. Panics if the block is not present.
    pub(crate) fn set_terminator(&mut self, id: IRBlockId, terminator: IRTerminator) {
        self.block_mut(id).terminator = terminator;
        self.closed.insert(id, ());
    }

    /// Mutable access to a block's contents. Panics if `id` is
    /// absent.
    fn block_mut(&mut self, id: IRBlockId) -> &mut IRBasicBlock {
        let index = *self
            .by_id
            .get(&id)
            .unwrap_or_else(|| panic!("CFGBuilder: block {id} not present"));
        &mut self.blocks[index]
    }

    /// Consume the builder and return the accumulated block list
    /// plus the set of closed block ids. The seal pass uses the
    /// closed-set to assert every block carries an explicitly-set
    /// terminator before it reaches downstream consumers.
    pub(crate) fn into_blocks_with_closed(self) -> (Vec<IRBasicBlock>, HashMap<IRBlockId, ()>) {
        (self.blocks, self.closed)
    }
}
