//! Per-package IR fragment. Output of the `lower-package` sub-pass and
//! input to `merge`. A `IRPackage` is the cacheable unit per the
//! northstar incremental story (one package, one cache entry).

use std::collections::BTreeMap;

use crate::function::{IRFunction, IRSymbol};

#[derive(Debug, Clone)]
pub struct IRPackage {
    /// Functions owned by this package, keyed by their stable
    /// [`IRSymbol`]. Each function's `symbol` field equals its key
    /// here by construction. Backends look up by `&str` through the
    /// `IRSymbol: Borrow<str>` impl, e.g.
    /// `pkg.functions.get(callee.mangled())`.
    pub functions: BTreeMap<IRSymbol, IRFunction>,
    /// The package label (e.g. `"TestApp"`, `"Global"`). Matches
    /// `CheckedPackage::package` from `expo-alpha-typecheck`.
    pub package: String,
}
