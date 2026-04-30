//! Per-function semantic state. Companion to [`crate::TypeLayouts`]:
//! `TypeLayouts` is the type-scoped semantic store; `FnLowerState` is the
//! function-scoped semantic store. The LLVM-bound half lives in
//! `expo-codegen`'s `FnState`.
//!
//! TCO bookkeeping is now a single field, [`FnLowerState::current_fn`]:
//! tail-ness was promoted onto `IRInstruction::Call` /
//! `IRInstruction::MethodCall` in Slice 6 (Wave 25), so the ambient
//! `tail_position` flag (and its `mark_tail` / `clear_tail` /
//! `save_tail` / `restore_tail` accessors) was retired. The remaining
//! `current_fn` accessor lets the codegen executor recognize a
//! self-recursive call so it can rewrite a `tail = true` invocation
//! into the `tco_loop` back-edge.

use std::collections::HashMap;

use expo_ast::types::Type;

use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator};
use crate::values::{IRInstruction, IRValueId};

#[derive(Default)]
pub struct FnLowerState {
    pub block_counter: u32,
    pub closure_counter: usize,
    current_fn: Option<String>,
    /// Stack of enclosing-loop exit block ids. The lowering site for
    /// [`expo_ast::ast::Statement::Break`] reads `loop_exit.last()` to
    /// emit an [`crate::IRTerminator::Branch`] to the innermost loop's
    /// exit.
    pub loop_exit: Vec<IRBlockId>,
    pub process_msg_type: Option<Type>,
    pub return_type_hint: Option<Type>,
    pub self_type_name: Option<String>,
    pub type_subst: HashMap<String, Type>,
    pub value_counter: u32,
    /// Pending blocks under construction by the function-body lowerer.
    /// Populated by [`crate::Lowerer::open_block`] /
    /// [`crate::Lowerer::close_block`] / [`crate::Lowerer::append_instr`];
    /// drained by [`crate::Lowerer::lower_function_body`] into the
    /// resulting `Vec<IRBasicBlock>` returned to the caller.
    pub blocks: Vec<IRBasicBlock>,
    /// The block currently being written to. `None` when the previous
    /// block's terminator has been emitted but the next block has not
    /// been opened yet -- in that state instructions cannot be appended
    /// without first calling [`crate::Lowerer::open_block`].
    pub current_block: Option<IRBlockId>,
}

impl FnLowerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh function-scoped basic block identifier. Counter
    /// resets per function via `FnLowerState::new` / `Default`.
    pub fn next_block_id(&mut self) -> IRBlockId {
        let id = IRBlockId(self.block_counter);
        self.block_counter += 1;
        id
    }

    /// Mint a fresh function-scoped SSA value identifier. Counter
    /// resets per function via `FnLowerState::new` / `Default`.
    /// Mirrors [`Self::next_block_id`].
    pub fn next_value_id(&mut self) -> IRValueId {
        let id = IRValueId(self.value_counter);
        self.value_counter += 1;
        id
    }

    /// Set the current function name at method-body entry. Returns the
    /// previous value so the caller can restore it on exit.
    pub fn enter_fn(&mut self, name: String) -> Option<String> {
        self.current_fn.replace(name)
    }

    /// Check whether `callee` is a self-recursive call that the codegen
    /// executor should rewrite as a `tco_loop` back-edge. Combined with
    /// `IRInstruction::MethodCall::tail` at the call site (Slice 6
    /// Wave 25 -- explicit IR-level tail flag, no ambient state).
    pub fn is_self_call(&self, callee: &str) -> bool {
        self.current_fn.as_deref() == Some(callee)
    }

    /// Restore the previous function name when leaving a method body.
    pub fn leave_fn(&mut self, saved: Option<String>) {
        self.current_fn = saved;
    }

    /// Push an enclosing-loop exit block id. Called by each loop emit
    /// walker before walking the body so [`expo_ast::ast::Statement::Break`]
    /// lowering can resolve the innermost loop's exit.
    pub fn push_loop_exit(&mut self, exit: IRBlockId) {
        self.loop_exit.push(exit);
    }

    /// Pop the innermost enclosing-loop exit block id. Called after
    /// the loop body walks complete.
    pub fn pop_loop_exit(&mut self) -> Option<IRBlockId> {
        self.loop_exit.pop()
    }

    /// Innermost enclosing-loop exit block id, or `None` outside any
    /// loop. Read by [`expo_ast::ast::Statement::Break`] lowering to
    /// emit a [`crate::IRTerminator::Branch`] target.
    pub fn current_loop_exit(&self) -> Option<IRBlockId> {
        self.loop_exit.last().copied()
    }

    /// Append `instr` to the currently-open block. Panics if no
    /// block is open -- callers must follow each [`Self::close_block`]
    /// with [`Self::open_block`] before emitting more instructions.
    pub fn append_instr(&mut self, instr: IRInstruction) {
        let id = self
            .current_block
            .expect("FnLowerState::append_instr called with no open block");
        let block = self
            .blocks
            .iter_mut()
            .find(|b| b.id == id)
            .expect("current_block must be present in blocks");
        block.instructions.push(instr);
    }

    /// Mint a fresh [`IRBasicBlock`] with `label`, push it onto the
    /// pending-blocks vector, set it as the cursor, and return its
    /// id. The new block starts empty with a placeholder
    /// `Branch(self)` terminator that the next [`Self::close_block`]
    /// overwrites; using a self-branch keeps the IR well-formed if a
    /// builder bug ever leaves a block open.
    pub fn open_block(&mut self, label: impl Into<String>) -> IRBlockId {
        let id = self.next_block_id();
        self.blocks.push(IRBasicBlock {
            id,
            instructions: Vec::new(),
            label: label.into(),
            terminator: IRTerminator::Branch(id),
        });
        self.current_block = Some(id);
        id
    }

    /// Open a block with a pre-allocated id (minted earlier so a
    /// successor terminator could reference it). Otherwise identical
    /// to [`Self::open_block`].
    pub fn open_block_with_id(&mut self, id: IRBlockId, label: impl Into<String>) {
        self.blocks.push(IRBasicBlock {
            id,
            instructions: Vec::new(),
            label: label.into(),
            terminator: IRTerminator::Branch(id),
        });
        self.current_block = Some(id);
    }

    /// Write `terminator` onto the currently-open block and clear the
    /// cursor. The next instruction emission requires a fresh
    /// [`Self::open_block`].
    pub fn close_block(&mut self, terminator: IRTerminator) {
        let id = self
            .current_block
            .take()
            .expect("FnLowerState::close_block called with no open block");
        let block = self
            .blocks
            .iter_mut()
            .find(|b| b.id == id)
            .expect("current_block must be present in blocks");
        block.terminator = terminator;
    }

    /// `true` iff the previous block was closed without opening a
    /// successor. Lowering uses this to decide whether to synthesize
    /// an implicit-return terminator at the end of a function body.
    pub fn cursor_is_closed(&self) -> bool {
        self.current_block.is_none()
    }
}
