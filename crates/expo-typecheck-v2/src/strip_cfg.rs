//! Strip-cfg sub-pass: prune nodes excluded by `@cfg(...)` annotations
//! so no later pass ever sees them.
//!
//! Operates on the AST only; does not touch the registry. Today this is
//! a no-op — `@cfg` is not yet implemented in the language. The pass
//! exists in the pipeline so that when `@cfg` lands the surrounding
//! orchestration in [`crate::check_program`] does not change shape.

use crate::program::CheckedPackage;

pub(crate) fn strip_cfg(packages: Vec<CheckedPackage>) -> Vec<CheckedPackage> {
    packages
}
