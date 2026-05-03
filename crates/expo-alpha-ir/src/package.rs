//! Per-package IR fragment. Output of the `lower-package` sub-pass and
//! input to `merge`. A `IRPackage` is the cacheable unit per the
//! northstar incremental story (one package, one cache entry).

use std::collections::BTreeMap;

use expo_ast::identifier::Identifier;

use crate::function::IRFunction;

#[derive(Debug, Clone)]
pub struct IRPackage {
    /// Functions owned by this package, keyed by their fully-qualified
    /// [`Identifier`]. Each `Identifier`'s package field equals
    /// [`Self::package`] by construction.
    pub functions: BTreeMap<Identifier, IRFunction>,
    /// The package label (e.g. `"TestApp"`, `"Global"`). Matches
    /// `CheckedPackage::package` from `expo-alpha-typecheck`.
    pub package: String,
}
