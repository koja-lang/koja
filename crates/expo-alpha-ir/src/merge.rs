//! Merge sub-pass: stitch the per-package [`IRPackage`] fragments
//! produced by [`crate::lower_package`] into a single working
//! [`IRProgram`].
//!
//! Today this is mechanical (preserve input order, attach the
//! caller-supplied entry-point identifier). It exists as its own pass
//! so the future deduplication / specialized-decl planning has a clear
//! seam.

use expo_ast::identifier::Identifier;

use crate::IRProgram;
use crate::package::IRPackage;

pub(crate) fn merge(packages: Vec<IRPackage>, entry_point: Identifier) -> IRProgram {
    IRProgram {
        entry_point,
        packages,
    }
}
