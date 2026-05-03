//! Check sub-pass: validates type compatibility on the resolved AST.
//!
//! Today this is a no-op. The POC's resolve pass already enforces
//! `Int + Int -> Int` inline; everything else is unreachable in the POC
//! scope. Real checks (call argument arity, return-type compatibility,
//! struct field consistency, exhaustive pattern matching) land here as
//! features grow and as `lift_signatures` starts publishing resolved
//! signature data the check pass can consume.

use expo_ast::ast::{Diagnostic, File};

use crate::registry::GlobalRegistry;

pub(crate) fn check_file(
    _file: &File,
    _registry: &GlobalRegistry,
    _diagnostics: &mut Vec<Diagnostic>,
) {
}
