//! Lift-signatures sub-pass: resolve every registered top-level decl's
//! parameter and return types and annotate them on the AST.
//!
//! Splitting registration ([`crate::collect`]) from signature
//! resolution (this pass) means a function's parameter or return type
//! can reference any other top-level type — including ones declared
//! later in the same file or in a sibling package — without ordering
//! constraints on traversal.
//!
//! Today this is a no-op. The POC's only program shape (`fn main; 2 +
//! 2; end`) has no `Ident` references, so the consumer of lifted
//! signatures (resolve's identifier handler) does not exist yet. Lands
//! when the first real cross-decl reference does.

use expo_ast::ast::{Diagnostic, File};

use crate::registry::GlobalRegistry;

pub(crate) fn lift_signatures_in_file(
    _file: &File,
    _package: &str,
    _registry: &mut GlobalRegistry,
    _diagnostics: &mut Vec<Diagnostic>,
) {
}
