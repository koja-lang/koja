//! Externs declared in `lib/global/src/system.koja`. `koja_get_env`
//! returns null when the variable is unset.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    cwd => fn koja_cwd() -> CPtr;
    get_env => fn koja_get_env(key: CPtr) -> CPtr;
    hostname => fn koja_hostname() -> CPtr;
    set_env => fn koja_set_env(key: CPtr, val: CPtr) -> ();
    toolchain_version => fn koja_toolchain_version() -> CPtr;
}
