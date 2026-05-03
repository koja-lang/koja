//! IR lowering entry point and per-package scaffold.
//!
//! [`lower_program`] is the sealed handoff from `expo-typecheck` to
//! `expo-ir`: it takes a [`CheckedProgram`] and returns an
//! [`IRProgram`], mirroring `expo_typecheck::check_program`'s
//! `ParsedProgram → CheckedProgram` shape. Fresh-compile callers (the
//! driver today) need only this one function; the per-package
//! decomposition lives behind it as an internal scaffold.
//!
//! expo-ir lowers at package granularity so a future incremental flow
//! can cache per-package fragments. That granularity shows up here as
//! the private [`lower_package`] helper and the [`IRPackage`] return
//! type. Both are no-ops today; the next slice fills the body. When
//! partial-recompile work begins, `lower_package` is promoted to a
//! public building block alongside a `relink_program` (or similar)
//! that takes `&[IRPackage]` instead of `&CheckedProgram`.

use expo_typecheck::context::TypeContext;
use expo_typecheck::{CheckedPackage, CheckedProgram};

use crate::program::IRProgram;

/// Per-package source-lowering fragment. Empty today; populated with
/// the per-package decl tables a future slice will produce. The unit
/// of incremental cache.
#[derive(Default)]
pub struct IRPackage {}

/// Lower a sealed [`CheckedProgram`] into an [`IRProgram`].
///
/// Takes the typecheck output as its proper input type and returns
/// the IR's proper output type. No-op today; the per-package and
/// merge bodies land in the next slice. The call chain through
/// [`lower_package`] is wired so future work fills bodies without
/// restructuring call sites.
pub fn lower_program(checked: &CheckedProgram) -> IRProgram {
    let program = IRProgram::default();
    for package in &checked.packages {
        let _fragment = lower_package(package, &checked.merged_ctx);
    }
    program
}

/// Lower one package's sealed AST into an [`IRPackage`] fragment.
/// Internal scaffold today; promoted to a public building block when
/// the partial-recompile flow lands (with its own `deps: &[&IRPackage]`
/// argument and a sibling `relink_program` for re-merging cached
/// fragments).
fn lower_package(package: &CheckedPackage, type_ctx: &TypeContext) -> IRPackage {
    let _ = (package, type_ctx);
    IRPackage::default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn empty_checked() -> CheckedProgram {
        CheckedProgram {
            file_contexts: BTreeMap::new(),
            has_errors: false,
            merged_ctx: TypeContext::new(),
            packages: Vec::new(),
        }
    }

    #[test]
    fn lower_program_returns_empty_program() {
        let program = lower_program(&empty_checked());
        assert!(program.constants.is_empty());
        assert!(program.enums.is_empty());
        assert!(program.functions.is_empty());
        assert!(program.structs.is_empty());
    }

    #[test]
    fn lower_program_visits_every_package() {
        let mut checked = empty_checked();
        checked.packages.push(CheckedPackage {
            ast: Vec::new(),
            package: "alpha".to_string(),
        });
        checked.packages.push(CheckedPackage {
            ast: Vec::new(),
            package: "beta".to_string(),
        });
        let program = lower_program(&checked);
        assert!(program.functions.is_empty());
    }
}
