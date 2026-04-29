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

use crate::blocks::IRBlockId;
use crate::values::IRValueId;

#[derive(Default)]
pub struct FnLowerState {
    pub block_counter: u32,
    pub closure_counter: usize,
    current_fn: Option<String>,
    /// Stack of enclosing-loop exit block ids. The lowering site for
    /// [`expo_ast::ast::Statement::Break`] reads `loop_exit.last()` to
    /// emit an [`crate::IRTerminator::Branch`] to the innermost loop's
    /// exit. Pushed by each loop emit walker (`emit_loop_unified` /
    /// `emit_while_unified` / `emit_for_unified`) before walking the
    /// body and popped after, mirroring the LLVM-bound exit-block
    /// stack on `expo-codegen`'s `FnState`.
    pub loop_exit: Vec<IRBlockId>,
    pub process_msg_type: Option<Type>,
    pub return_type_hint: Option<Type>,
    pub self_type_name: Option<String>,
    pub type_subst: HashMap<String, Type>,
    pub value_counter: u32,
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
}
