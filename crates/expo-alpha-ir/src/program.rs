//! The single public entry point for the alpha lowering phase.
//!
//! [`lower_program`] consumes a sealed
//! [`expo_alpha_typecheck::CheckedProgram`] and either:
//! - returns `Ok(IRProgram)` whose shape is **sealed** (every block
//!   ends in a terminator, every value reference points at a
//!   previously-defined value in the same function, the entry point
//!   resolves to a registered function), or
//! - returns `Err(LowerError)` carrying one of two user-actionable
//!   failure modes: feature-gap diagnostics accumulated while walking
//!   the sealed AST, or an entry-point lookup miss when the caller
//!   asked for a function that no package registered.
//!
//! `seal_program` runs as the last sub-pass of `lower_program`; seal
//! violations panic per northstar (compiler bugs, not user errors).

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::Diagnostic;
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
///
/// `Diagnostics` and `EntryPointNotFound` are disjoint: the lowering
/// pass short-circuits before the entry-point check when diagnostics
/// are present, so callers can match on one variant at a time.
#[derive(Debug, Clone)]
pub enum LowerError {
    /// One or more feature-gap diagnostics surfaced while lowering
    /// the sealed AST (unsupported expression / literal / statement
    /// kinds, extern-body functions, unsupported binary operators,
    /// etc.). Each [`Diagnostic`] carries a source span + message.
    /// Lowering is per-function fail-fast: a failed function
    /// contributes one diagnostic and is omitted from the resulting
    /// partial IR.
    Diagnostics(Vec<Diagnostic>),
    /// The caller asked for an entry point that no package in the
    /// lowered program registers.
    EntryPointNotFound { identifier: Identifier },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::Diagnostics(diagnostics) => {
                for (index, diag) in diagnostics.iter().enumerate() {
                    if index > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{}", diag.message)?;
                }
                Ok(())
            }
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
///    `IRPackage` fragment. Pushes feature-gap diagnostics into the
///    shared buffer and omits functions that failed to lower.
/// 2. If any diagnostics were recorded, return
///    `Err(LowerError::Diagnostics)` immediately. Seal never runs on
///    a partial IR — its invariants assume a complete program, and
///    violating them panics (northstar: seal failures are compiler
///    bugs, not user errors).
/// 3. `merge` — stitch the per-package fragments into a single
///    working `IRProgram`.
/// 4. Entry-point existence check — surfaces `EntryPointNotFound`.
/// 5. `seal` — assert sealed-IRProgram invariants. Panics on violation.
///
/// Future sub-passes land between `merge` and `seal` when the work
/// they do becomes load-bearing (e.g. a `closure` pass for generic
/// instantiation discovery, or an `elaborate` pass for coercion
/// emission). They're not in the pipeline yet because the POC has
/// nothing for them to do — adding no-op pass-throughs would be
/// dead architecture.
pub fn lower_program(checked: &CheckedProgram, entry: Identifier) -> Result<IRProgram, LowerError> {
    let mut diagnostics = Vec::new();
    let mut packages = Vec::with_capacity(checked.packages.len());
    for pkg in &checked.packages {
        packages.push(lower_package::lower_package(
            pkg,
            &checked.registry,
            &mut diagnostics,
        ));
    }

    if !diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(diagnostics));
    }

    let program = merge::merge(packages, entry.clone());

    if program.function(&entry).is_none() {
        return Err(LowerError::EntryPointNotFound { identifier: entry });
    }

    seal::seal_program(&program);
    Ok(program)
}
