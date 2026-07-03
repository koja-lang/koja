//! Externs declared in `lib/global/src/system.koja`.
//!
//! Wrappers over `koja_runtime::system` for env-var, cwd, and
//! hostname queries. Each handler routes through the same C ABI
//! the LLVM backend uses, so eval observes the same OS-state-driven
//! values the native binary would.
//!
//! The pointer-returning calls (`koja_cwd`, `koja_get_env`,
//! `koja_hostname`) hand back a runtime-allocated payload that
//! `CPtr<UInt8>.to_cstring` walks. Eval just round-trips the raw
//! pointer through [`crate::value::Value::CPtr`]. The runtime side
//! owns the storage layout.
//!
//! `koja_get_env` returns a null pointer when the requested variable
//! is unset, matching the runtime's contract: `System.get_env`'s
//! `ptr.null?()` check then yields `Option.None` without any
//! eval-side branching.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    cwd => fn koja_cwd() -> CPtr;
    get_env => fn koja_get_env(key: CPtr) -> CPtr;
    hostname => fn koja_hostname() -> CPtr;
    set_env => fn koja_set_env(key: CPtr, val: CPtr) -> ();
}
