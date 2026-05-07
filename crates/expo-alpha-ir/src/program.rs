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

use std::collections::BTreeSet;

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::identifier::Identifier;

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::error::LowerError;
use crate::function::{FunctionKind, IRFunction, IRSymbol};
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
///
/// `link_libraries` is the deduped, sorted list of bare library names
/// (`m`, `crypto`) collected from every `@extern "C"` function's
/// [`crate::IRExternAttrs::link_lib`]. The driver feeds these to the
/// linker as `-l<name>`. Per-function `link_name` overrides stay on
/// the [`IRFunction`] — only the library set surfaces here.
#[derive(Debug, Clone)]
pub struct IRProgram {
    pub entry_point: IRSymbol,
    pub link_libraries: Vec<String>,
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

    /// Lookup a pooled constant value across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]; backends pass
    /// the `&IRSymbol` carried on [`crate::IRInstruction::LoadConst`]
    /// directly through the `IRSymbol: Borrow<str>` impl.
    pub fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.constants.get(mangled))
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

    if !output.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(output.diagnostics));
    }

    let initial = std::mem::take(&mut output.instantiations);
    generics::instantiate(
        initial,
        &checked.registry,
        &checked.packages,
        &mut packages,
        &mut output,
    );

    if !output.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(output.diagnostics));
    }

    let entry_symbol = IRSymbol::from_identifier(&entry);
    let mut program = merge::merge(packages, entry_symbol);
    program.link_libraries = collect_link_libraries(program.packages.iter());

    if program.function(program.entry_point.mangled()).is_none() {
        return Err(LowerError::EntryPointNotFound { identifier: entry });
    }

    seal::seal_program(&program);
    Ok(program)
}

/// Walk every `@extern "C"` function across `packages` and collect a
/// deduped, sorted list of `link_lib` names. Used at lower time so
/// backends and cache layers don't re-walk the IR. Functions without
/// a `link_lib` (bare `@extern "C"` with no `@link`) contribute
/// nothing; the C symbol is still resolved via the normal libc /
/// runtime search path at link time.
pub(crate) fn collect_link_libraries<'a, I>(packages: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a IRPackage>,
{
    let mut libs = BTreeSet::new();
    for pkg in packages {
        for function in pkg.functions.values() {
            if let FunctionKind::Extern(attrs) = &function.kind
                && let Some(lib) = &attrs.link_lib
            {
                libs.insert(lib.clone());
            }
        }
    }
    libs.into_iter().collect()
}
