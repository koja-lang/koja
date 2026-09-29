//! Per-run cache for [`koja_ir::IRConstantValue::Built`] constants.
//!
//! The interpreter builds a constant the first time a `LoadConst`
//! reads it and serves every later read from this cache, so a run
//! sees one shared value per constant, the same as the LLVM backend's
//! global. Each `run_*` entry installs a fresh cache through
//! [`install`] and the guard clears it on drop, so tests that run
//! many programs on one thread start clean. Eval is single-threaded,
//! so the lazy build needs no lock. A read inside an init body
//! recurses through the same path, which gives dependency order for
//! free, and koja-ir has already rejected startup cycles.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::value::Value;

thread_local! {
    /// The cache for the in-flight run, keyed by the constant's
    /// mangled symbol. `None` outside a run.
    static BUILT: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
}

/// Clears the installed cache on drop.
pub(crate) struct BuiltGuard;

impl Drop for BuiltGuard {
    fn drop(&mut self) {
        BUILT.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Installs an empty cache for the current run. The guard removes it.
pub(crate) fn install() -> BuiltGuard {
    BUILT.with(|slot| *slot.borrow_mut() = Some(HashMap::new()));
    BuiltGuard
}

/// The built value of `symbol`, when this run has built it. Returns
/// a clone so the caller holds no borrow on the cache across an
/// await.
pub(crate) fn cached(symbol: &str) -> Option<Value> {
    BUILT.with(|slot| slot.borrow().as_ref()?.get(symbol).cloned())
}

/// Records the built value of `symbol`. A no-op when no cache is
/// installed, in which case every read rebuilds the constant.
pub(crate) fn store(symbol: &str, value: Value) {
    BUILT.with(|slot| {
        if let Some(cache) = slot.borrow_mut().as_mut() {
            cache.insert(symbol.to_string(), value);
        }
    });
}
