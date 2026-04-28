//! Per-function semantic state. Companion to [`crate::TypeLayouts`]:
//! `TypeLayouts` is the type-scoped semantic store; `FnLowerState` is the
//! function-scoped semantic store. The LLVM-bound half lives in
//! `expo-codegen`'s `FnState`.
//!
//! The TCO traversal state (`current_fn`, `tail_position`) is an ambient
//! flag set during the AST walk. It exists today only because lowering and
//! emission are merged in one pass; once `expo-ir` carries explicit
//! instructions, tail-ness becomes a property on `IRInstruction::Call`
//! (and the loop header / parameter allocas become derivable from the IR's
//! function header), so this whole sub-area dissolves. Inlined directly
//! here rather than wrapped in a sub-struct because the cohesion is
//! transitional.

use std::collections::HashMap;

use expo_ast::types::Type;

use crate::blocks::IRBlockId;
use crate::values::IRValueId;

#[derive(Default)]
pub struct FnLowerState {
    pub block_counter: u32,
    pub closure_counter: usize,
    current_fn: Option<String>,
    pub process_msg_type: Option<Type>,
    pub return_type_hint: Option<Type>,
    pub self_type_name: Option<String>,
    tail_position: bool,
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

    /// Clear the tail-position flag.
    pub fn clear_tail(&mut self) {
        self.tail_position = false;
    }

    /// Set the current function name at method-body entry. Returns the
    /// previous value so the caller can restore it on exit.
    pub fn enter_fn(&mut self, name: String) -> Option<String> {
        self.current_fn.replace(name)
    }

    /// Check whether `callee` is a self-recursive call that should be
    /// rewritten as a loop jump. `was_tail` should come from `save_tail`.
    pub fn is_self_tail_call(&self, callee: &str, was_tail: bool) -> bool {
        was_tail && self.current_fn.as_deref() == Some(callee)
    }

    /// Restore the previous function name when leaving a method body.
    pub fn leave_fn(&mut self, saved: Option<String>) {
        self.current_fn = saved;
    }

    /// Mark the current compile position as tail position.
    pub fn mark_tail(&mut self) {
        self.tail_position = true;
    }

    /// Restore the tail-position flag after subexpression compilation.
    /// This ensures sibling code paths (other match arms, if/else branches)
    /// still see the flag.
    pub fn restore_tail(&mut self, was_tail: bool) {
        if was_tail {
            self.tail_position = true;
        }
    }

    /// Save and clear the tail-position flag. The flag is cleared so that
    /// subexpressions (receiver, arguments) don't inherit it. The returned
    /// value must be passed to `restore_tail` and `is_self_tail_call`.
    pub fn save_tail(&mut self) -> bool {
        std::mem::replace(&mut self.tail_position, false)
    }

    /// Read the current tail-position flag without clearing it. Used by
    /// the IR call-lift helpers to defer to Stub when a self-tail-recursive
    /// rewrite needs to happen on the legacy emission path.
    pub fn tail_position(&self) -> bool {
        self.tail_position
    }
}
