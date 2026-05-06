//! Sealed IR for project-mode sources (`expo build`, `expo run` on
//! a manifest-rooted package) plus the [`lower_program`] entry point
//! that produces them.
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
use expo_ast::identifier::Identifier;

use crate::enum_decl::IREnumDecl;
use crate::error::LowerError;
use crate::function::{IRFunction, IRSymbol};
use crate::generics;
use crate::lower::LowerOutput;
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::{lower, merge, seal};

/// Sealed output of [`lower_program`]'s success path. Backends consume
/// this directly; they build their own indices over the sealed
/// vocabulary and never need to revisit the `CheckedProgram` it came
/// from.
///
/// `entry_point` is the stable [`IRSymbol`] of the user-declared entry
/// function. Stamped from the [`Identifier`] the caller passed into
/// [`lower_program`] — backends consume the symbol and never need to
/// reach back into `expo-ast`.
#[derive(Debug, Clone)]
pub struct IRProgram {
    pub entry_point: IRSymbol,
    pub packages: Vec<IRPackage>,
}

impl IRProgram {
    /// Lookup a function across every package by its mangled symbol.
    /// `O(packages * log functions_per_package)`; for the 1–3 packages
    /// an alpha program ships today this is overwhelmingly cheap. A
    /// flat index lands when codegen needs hot-path lookups.
    ///
    /// Accepts any `&str`-borrowable input, so backends can pass a
    /// `&IRSymbol` directly or a raw mangled string they pulled off
    /// an `IRInstruction::Call`.
    pub fn function(&self, mangled: &str) -> Option<&IRFunction> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.functions.get(mangled))
    }

    /// Lookup a struct declaration across every package by its
    /// mangled symbol. Mirrors [`Self::function`]; backends pass a
    /// `&IRSymbol` from `IRType::Struct` / `IRInstruction::StructInit`
    /// / `IRInstruction::FieldGet` directly through the
    /// `IRSymbol: Borrow<str>` impl.
    pub fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.structs.get(mangled))
    }

    /// Lookup an enum declaration across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]; backends pass
    /// a `&IRSymbol` from `IRType::Enum` /
    /// `IRInstruction::EnumConstruct` directly through the
    /// `IRSymbol: Borrow<str>` impl.
    pub fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        self.packages.iter().find_map(|pkg| pkg.enums.get(mangled))
    }

    /// The function the entry point resolves to. Panics if missing —
    /// the entry-point existence check is a precondition that
    /// `lower_program` enforces, and `seal_program` re-asserts on the
    /// final IRProgram.
    pub fn entry_function(&self) -> &IRFunction {
        self.function(self.entry_point.mangled())
            .expect("entry point not registered in IRProgram (seal violation upstream)")
    }

    /// Whether `function` is this program's entry point. Lets backends
    /// distinguish the entry function (which gets exported under the
    /// host-runtime symbol, e.g. `main` on Unix) from every other
    /// function in the program — symbol-keyed, with no AST types in
    /// scope.
    pub fn is_entry(&self, function: &IRFunction) -> bool {
        function.symbol == self.entry_point
    }
}

/// Run every sub-pass in the alpha lowering phase.
///
/// Sub-pass order (forced by data dependencies):
///
/// 1. `lower_package` — translate each `CheckedPackage` into an
///    `IRPackage` fragment. Generic decls are skipped (they live in
///    the typecheck registry); concrete instantiations encountered
///    along the way accumulate into a flat list keyed at the
///    template's [`expo_ast::identifier::GlobalRegistryId`]. Feature-
///    gap diagnostics push into the shared buffer and the offending
///    decl is dropped.
/// 2. If any diagnostics were recorded, return
///    `Err(LowerError::Diagnostics)` immediately. Seal never runs on
///    a partial IR — its invariants assume a complete program, and
///    violating them panics (northstar: seal failures are compiler
///    bugs, not user errors).
/// 3. `generics::instantiate` — dedupe the instantiation list and
///    monomorphize each one off the typecheck registry into the
///    [`IRPackage`] that owns the template. The instantiation set is
///    dropped here and never reaches merge / seal / backends.
/// 4. `merge` — stitch the per-package fragments into a single
///    working `IRProgram`.
/// 5. Entry-point existence check — surfaces `EntryPointNotFound`.
/// 6. `seal` — assert sealed-IRProgram invariants. Panics on violation.
pub fn lower_program(checked: &CheckedProgram, entry: Identifier) -> Result<IRProgram, LowerError> {
    let mut output = LowerOutput::default();
    let mut packages = Vec::with_capacity(checked.packages.len());
    for pkg in &checked.packages {
        packages.push(lower::lower_package(pkg, &checked.registry, &mut output));
    }
    let LowerOutput {
        diagnostics,
        instantiations,
    } = output;

    if !diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(diagnostics));
    }

    generics::instantiate(instantiations, &checked.registry, &mut packages);

    let entry_symbol = IRSymbol::from_identifier(&entry);
    let program = merge::merge(packages, entry_symbol);

    if program.function(program.entry_point.mangled()).is_none() {
        return Err(LowerError::EntryPointNotFound { identifier: entry });
    }

    seal::seal_program(&program);
    Ok(program)
}
