//! [`CFGBuilder`]: an explicit, stack-allocated accumulator for a
//! function's control-flow graph during lowering.
//!
//! Lowering passes a `&mut CFGBuilder` plus the currently-open
//! [`IRBlockId`] through every recursive call. Each `lower_*` method
//! mutates the builder (adds blocks, appends instructions, sets
//! terminators) and returns the new "open" block (or [`None`] if all
//! paths terminated). The builder owns no state between calls beyond
//! its block list, so lowering is referentially transparent: given
//! the same `(builder snapshot, open, expr)` it produces the same
//! result.
//!
//! The builder replaces the cursor-on-`FnLowerState` approach
//! (ambient `current_block` + `blocks` + `scope_stack`) the foundation
//! commit `6c39591` set up. Stub-deferred contexts (e.g.
//! [`crate::values::IRInstruction::Stub`]'s executor walking AST that
//! contains an `if` / `match`) get their own fresh `CFGBuilder` per
//! call -- no scope nesting needed because no ambient cursor exists.
//!
//! Block / value identifiers stay function-unique (minted by
//! [`crate::FnLowerState::next_block_id`] /
//! [`crate::FnLowerState::next_value_id`]) so terminator references
//! resolve regardless of which builder produced the block.

use std::collections::HashSet;

use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator, LoopExitOp};
use crate::values::IRInstruction;

/// Accumulator for a single CFG fragment. Owns the in-progress
/// [`IRBasicBlock`] list and tracks which blocks have had a real
/// terminator set (vs. the placeholder self-branch added at
/// [`Self::add_block`] time).
#[derive(Debug, Default)]
pub struct CFGBuilder {
    blocks: Vec<IRBasicBlock>,
    /// Block ids whose terminator has been explicitly set via
    /// [`Self::set_terminator`]. Walks consult this to distinguish
    /// intentional self-branches (e.g. an infinite loop's body
    /// terminator targeting itself) from the placeholder
    /// `Branch(self)` left when a block is opened but never closed
    /// by lowering (typical of construct merge blocks where the
    /// surrounding caller continues writing).
    closed: HashSet<IRBlockId>,
}

impl CFGBuilder {
    /// Empty builder. Block counter / value counter live on
    /// [`crate::FnLowerState`] -- the builder only owns the block
    /// list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a fresh empty block with the given id and human-readable
    /// label. Caller must have minted `id` via
    /// [`crate::FnLowerState::next_block_id`]. The new block starts
    /// open: its terminator slot holds a placeholder `Branch(self)`
    /// that callers must overwrite via [`Self::set_terminator`]
    /// before the block is walked. Walks treat unclosed blocks
    /// (those without a recorded [`Self::set_terminator`] call) as
    /// "leave the LLVM builder positioned here" so the surrounding
    /// caller can continue writing into the merge block.
    pub fn add_block(&mut self, id: IRBlockId, label: impl Into<String>) {
        self.blocks.push(IRBasicBlock {
            id,
            instructions: Vec::new(),
            label: label.into(),
            terminator: IRTerminator::Branch(id),
            loop_exit_op: LoopExitOp::None,
        });
    }

    /// Tag `body_id` with [`LoopExitOp::Push`] of `exit_id` and
    /// `exit_id` with [`LoopExitOp::Pop`] so `walk_function_blocks`
    /// re-establishes the enclosing loop scope at codegen-execute
    /// time. See [`crate::LoopExitOp`].
    pub fn mark_loop(&mut self, body_id: IRBlockId, exit_id: IRBlockId) {
        self.block_mut(body_id).loop_exit_op = LoopExitOp::Push(exit_id);
        self.block_mut(exit_id).loop_exit_op = LoopExitOp::Pop;
    }

    /// Append `instr` to the block identified by `id`. Panics if the
    /// block is not present in the builder -- callers should add the
    /// block via [`Self::add_block`] first.
    pub fn append(&mut self, id: IRBlockId, instr: IRInstruction) {
        self.block_mut(id).instructions.push(instr);
    }

    /// Overwrite the terminator of the block identified by `id` and
    /// mark the block as closed. Panics if the block is not present.
    pub fn set_terminator(&mut self, id: IRBlockId, terminator: IRTerminator) {
        self.block_mut(id).terminator = terminator;
        self.closed.insert(id);
    }

    /// `true` iff [`Self::set_terminator`] has been called on `id`.
    /// Walks read this to decide whether to honor the block's
    /// terminator or leave the LLVM builder positioned at the
    /// block's end.
    pub fn is_closed(&self, id: IRBlockId) -> bool {
        self.closed.contains(&id)
    }

    /// Borrow of the accumulated blocks (in insertion order). Used by
    /// the codegen walker [`walk_function_blocks`] to allocate LLVM
    /// blocks and emit instructions / terminators.
    pub fn blocks(&self) -> &[IRBasicBlock] {
        &self.blocks
    }

    /// Mutable access to a block's contents (used by sub-passes that
    /// patch a block in place, e.g. elaboration replacing a stub).
    /// Panics if `id` is absent.
    pub fn block_mut(&mut self, id: IRBlockId) -> &mut IRBasicBlock {
        self.blocks
            .iter_mut()
            .find(|b| b.id == id)
            .unwrap_or_else(|| panic!("CFGBuilder: block {id:?} not present"))
    }

    /// Consume the builder and return the accumulated block list
    /// plus the set of closed block ids (those whose terminator was
    /// explicitly set via [`Self::set_terminator`]). Walks consult
    /// the closed-set to distinguish intentional terminators from
    /// the placeholder self-branch left on unclosed blocks.
    pub fn into_blocks_with_closed(self) -> (Vec<IRBasicBlock>, HashSet<IRBlockId>) {
        (self.blocks, self.closed)
    }

    /// Consume the builder and return only the block list (drops
    /// the closed-set). Use when the caller walks via the legacy
    /// [`IRFunctionKind`] storage path that doesn't carry the
    /// closed-set; the placeholder self-branch will then leak as a
    /// real terminator. Prefer [`Self::into_blocks_with_closed`] for
    /// new sites.
    pub fn into_blocks(self) -> Vec<IRBasicBlock> {
        self.blocks
    }
}
