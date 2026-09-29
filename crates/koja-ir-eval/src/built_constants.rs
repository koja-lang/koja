//! Per-run store for [`koja_ir::IRConstantValue::Built`] constants.
//!
//! PID 1 runs every init in `built_constant_order` before user code
//! starts and stores each value here, so a run sees one shared value
//! per constant and an init side effect happens once at startup, the
//! same as the LLVM backend's globals. Every `LoadConst` on a `Built`
//! entry is then a plain read. Each `run_*` entry installs a fresh
//! store through [`install`] and the guard clears it on drop, so
//! tests that run many programs on one thread start clean. Eval is
//! single-threaded, so the store needs no lock.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::value::Value;

thread_local! {
    /// The store for the in-flight run, keyed by the constant's
    /// mangled symbol. `None` outside a run.
    static BUILT: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
}

/// Clears the installed store on drop.
pub(crate) struct BuiltGuard;

impl Drop for BuiltGuard {
    fn drop(&mut self) {
        BUILT.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Installs an empty store for the current run. The guard removes it.
pub(crate) fn install() -> BuiltGuard {
    BUILT.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
    BuiltGuard
}

/// Records the built value of `symbol`. Every `run_*` entry installs
/// a store first, so a missing store is an interpreter bug.
pub(crate) fn store(symbol: &str, value: Value) {
    BUILT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("interpreter: built constant stored outside a run")
            .insert(symbol.to_string(), value);
    });
}

/// The built value of `symbol`. Returns a clone so the caller holds
/// no borrow on the store. Every `Built` constant is stored before
/// user code runs, so a miss means the stored `built_constant_order`
/// let a read run before its init, which the seal rules out.
pub(crate) fn value(symbol: &str) -> Value {
    BUILT.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|store| store.get(symbol).cloned())
            .unwrap_or_else(|| {
                panic!(
                    "interpreter: built constant `{symbol}` read before its init ran (seal \
                     invariant violation)"
                )
            })
    })
}
