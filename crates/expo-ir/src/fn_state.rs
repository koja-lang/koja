//! Per-function semantic state. Companion to [`crate::TypeLayouts`]:
//! `TypeLayouts` is the type-scoped semantic store; `FnLowerState` is the
//! function-scoped semantic store. The LLVM-bound half lives in
//! `expo-codegen`'s `FnState`.
//!
//! Phase 4g Slice 3: the cursor API (ambient `current_block` /
//! `blocks` / `scope_stack` / `open_block` / `close_block` /
//! `append_instr` / etc.) introduced in commit `6c39591` was deleted
//! in favor of explicit [`crate::CFGBuilder`] threading through every
//! recursive `lower_*` call. Block / value identifiers stay
//! function-unique here so terminator references resolve regardless
//! of which builder produced the referenced block.
//!
//! TCO bookkeeping is a single field, [`FnLowerState::current_fn`]:
//! tail-ness was promoted onto `IRInstruction::Call` /
//! `IRInstruction::MethodCall` in Slice 6 (Wave 25). The remaining
//! `current_fn` accessor lets the codegen executor recognize a
//! self-recursive call so it can rewrite a `tail = true` invocation
//! into the `tco_loop` back-edge.

use std::collections::HashMap;

use expo_ast::types::Type;

use crate::blocks::IRBlockId;
use crate::lower::ctx::LocalBindings;
use crate::values::IRValueId;

#[derive(Default)]
pub struct FnLowerState {
    pub block_counter: u32,
    pub closure_counter: usize,
    current_fn: Option<String>,
    /// Name -> resolved Expo type for locals visible to lowering.
    /// Seeded from `meta.params` (incl. `self` for methods) at
    /// function entry; updated by `lower_assignment_stmt::store_local`
    /// (fresh decls) and the codegen-side bind sites
    /// (`bind_self_param`, `bind_regular_params`, `bind_for_pattern`,
    /// the `IRInstruction::StoreLocal` executor's `is_decl` arm,
    /// and the legacy `compile_assignment` fork) as the lowerer
    /// walks. Decoupled from `expo-codegen`'s `FnState::variables`
    /// (LLVM-alloca-bound, only meaningful at execute time): the
    /// lowerer reads its own typed view here so whole-function
    /// lowering can resolve `Ident` references to typed `LoadLocal`
    /// instructions before any LLVM state exists. Flat scope --
    /// `insert` overwrites for shadowing; block-scope escape rules
    /// are enforced by typecheck.
    pub local_types: HashMap<String, Type>,
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
}

impl LocalBindings for FnLowerState {
    fn type_of(&self, name: &str) -> Option<Type> {
        self.local_types.get(name).cloned()
    }
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

    /// Push an enclosing-loop exit block id. Called by each loop
    /// lowering before lowering the body so
    /// [`expo_ast::ast::Statement::Break`] resolves to the innermost
    /// loop's exit.
    pub fn push_loop_exit(&mut self, exit: IRBlockId) {
        self.loop_exit.push(exit);
    }

    /// Pop the innermost enclosing-loop exit block id. Called after
    /// the loop body lowering completes.
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
