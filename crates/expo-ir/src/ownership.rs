//! Ownership classification for local bindings and store sites.
//!
//! Tracks whether a binding owns its backing memory and is therefore
//! responsible for freeing it at scope exit. Used by `expo-codegen`'s
//! drop pass to distinguish heap-allocated values (interpolated
//! strings, mailbox-received binaries, list/map/set collections,
//! struct values with indirect fields) from borrowed values
//! (string-literal pointers, primitive copies).
//!
//! Lives in `expo-ir` so the lowering site for [`crate::values::IRInstruction::StoreLocal`]
//! can stamp the classification at IR-build time. The codegen
//! executor reads it back when registering the binding into
//! `Compiler.fn_state.variables`.

/// Whether a binding owns its backing memory and is responsible for
/// freeing it at scope exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    Unowned,
}
