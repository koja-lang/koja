//! The single public entry point for the alpha lowering phase.
//!
//! [`lower_program`] consumes a sealed
//! [`expo_alpha_typecheck::CheckedProgram`] and either:
//! - returns `Ok(IRProgram)` whose shape is **sealed** (every block
//!   ends in a terminator, every value reference points at a
//!   previously-defined value in the same function, the entry point
//!   resolves to a registered function), or
//! - returns `Err(LowerError)` for the one user-actionable failure
//!   today: the caller-supplied entry point is not present in any
//!   lowered package.
//!
//! `seal_program` runs as the last sub-pass of `lower_program`; seal
//! violations panic per northstar (compiler bugs, not user errors).

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::identifier::Identifier;

use crate::function::IRFunction;
use crate::package::IRPackage;
use crate::{lower_package, merge, seal};

/// Sealed output of [`lower_program`]'s success path. Backends consume
/// this directly; they build their own indices over the sealed
/// vocabulary and never need to revisit the `CheckedProgram` it came
/// from.
#[derive(Debug, Clone)]
pub struct IRProgram {
    pub entry_point: Identifier,
    pub packages: Vec<IRPackage>,
}

impl IRProgram {
    /// Lookup a function across every package by its fully-qualified
    /// identifier. `O(packages * log functions_per_package)`; for the
    /// 1–3 packages an alpha program ships today this is overwhelmingly
    /// cheap. A flat index lands when codegen needs hot-path lookups.
    pub fn function(&self, id: &Identifier) -> Option<&IRFunction> {
        self.packages.iter().find_map(|pkg| pkg.functions.get(id))
    }

    /// The function the entry point resolves to. Panics if missing —
    /// the entry-point existence check is a precondition that
    /// `lower_program` enforces, and `seal_program` re-asserts on the
    /// final IRProgram.
    pub fn entry_function(&self) -> &IRFunction {
        self.function(&self.entry_point)
            .expect("entry point not registered in IRProgram (seal violation upstream)")
    }
}

/// User-actionable failure modes from [`lower_program`]. Anything that
/// could only originate from a compiler bug panics through `seal`
/// instead of surfacing here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    /// The caller asked for an entry point that no package in the
    /// lowered program registers.
    EntryPointNotFound { identifier: Identifier },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::EntryPointNotFound { identifier } => {
                write!(f, "entry point `{identifier}` is not defined")
            }
        }
    }
}

impl std::error::Error for LowerError {}

/// Run every sub-pass in the alpha lowering phase.
///
/// Sub-pass order (forced by data dependencies):
///
/// 1. `lower_package` — translate each `CheckedPackage` into an
///    `IRPackage` fragment. Pure with respect to its input.
/// 2. `merge` — stitch the per-package fragments into a single
///    working `IRProgram`.
/// 3. `seal` — assert sealed-IRProgram invariants. Panics on violation.
///
/// The entry-point existence check happens between `merge` and `seal`
/// so the caller sees a clean [`LowerError`] before the seal pass
/// runs.
///
/// Future sub-passes land between `merge` and `seal` when the work
/// they do becomes load-bearing (e.g. a `closure` pass for generic
/// instantiation discovery, or an `elaborate` pass for coercion
/// emission). They're not in the pipeline yet because the POC has
/// nothing for them to do — adding no-op pass-throughs would be
/// dead architecture.
pub fn lower_program(checked: &CheckedProgram, entry: Identifier) -> Result<IRProgram, LowerError> {
    let mut packages = Vec::with_capacity(checked.packages.len());
    for pkg in &checked.packages {
        packages.push(lower_package::lower_package(pkg));
    }

    let program = merge::merge(packages, entry.clone());

    if program.function(&entry).is_none() {
        return Err(LowerError::EntryPointNotFound { identifier: entry });
    }

    seal::seal_program(&program);
    Ok(program)
}
