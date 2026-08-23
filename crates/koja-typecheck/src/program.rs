//! Public entry point for the typecheck phase. [`check_program`]
//! returns a sealed [`CheckedProgram`] on success or a
//! [`crate::CheckFailure`] carrying diagnostics + the partial
//! `ParsedProgram` on failure. Seal is asserted as the last sub-pass
//! and panics on violation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use koja_ast::ast::{Diagnostic, File, Severity};
use koja_ast::span::FileId;
use koja_parser::{ParsedFile, ParsedProgram};

use crate::error::CheckFailure;
use crate::pipeline::{
    aliases, borrows, collect, definite_assignment, deprecation, desugar, lift_signatures, resolve,
    seal, synthesize, visibility,
};
use crate::registry::GlobalRegistry;

/// A package fragment of a [`CheckedProgram`].
#[derive(Debug, Clone)]
pub struct CheckedPackage {
    pub files: Vec<File>,
    pub package: String,
}

/// Sealed output of [`check_program`]'s success path. Every relevant
/// AST annotation is populated. Lowering crates can rely on this
/// without re-validating.
///
/// `diagnostics` carries non-error-severity diagnostics (today:
/// reachability / redundancy warnings on `match` arms). Errors
/// short-circuit to [`crate::CheckFailure`]. Only warnings ride the
/// success path. Downstream consumers (driver, LSP) surface them
/// alongside parse-phase warnings.
#[derive(Debug, Clone)]
pub struct CheckedProgram {
    pub diagnostics: Vec<Diagnostic>,
    pub packages: Vec<CheckedPackage>,
    /// Canonical source of truth for what was registered. Lowering
    /// crates build their own indices over `Identifier`.
    pub registry: GlobalRegistry,
    /// File table indexed by [`FileId`], copied from
    /// `ParsedProgram::order`. Resolves any span to its owning file.
    pub source_paths: Vec<PathBuf>,
}

impl CheckedProgram {
    /// Resolve a span's [`FileId`] to the owning file path.
    pub fn path_of(&self, file: FileId) -> Option<&Path> {
        self.source_paths.get(file.0 as usize).map(PathBuf::as_path)
    }
}

/// Run every sub-pass in the typecheck phase.
///
/// Short-circuits if `parsed` already carries error-severity parse
/// diagnostics. Otherwise runs the sub-passes in order:
///
/// 0. **preload stdlib stubs**: seed the [`GlobalRegistry`] with
///    [`GlobalRegistry::with_stdlib_stubs`] so `Global.Int`/`.Bool`/
///    `.Unit`/`.Float`/`.String` are registered as structs before any
///    user decl. Temporary. Once the real stdlib compiles as a
///    package these entries land through `collect`.
/// 1. Hoist lexically nested type declarations to qualified
///    top-level items.
/// 2. Derive Debug and Equality impls before binding.
/// 3. Collect declarations, then impl blocks, across every file.
/// 4. Validate nested declarations and file aliases.
/// 5. Lift signatures and declaration definitions into the registry.
/// 6. Reject private types leaked through public signatures.
/// 7. Rewrite typed surface shapes such as `for`.
/// 8. Resolve and type-check every body.
/// 9. Reject escaping `CPtr.borrow` results.
/// 10. Reject reads of locals not definitely assigned on every path.
/// 11. Warn on uses of `@deprecated` declarations.
/// 12. Return [`CheckFailure`] if any errors were collected.
/// 13. Seal successful AST and registry invariants.
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure> {
    if parsed.has_errors() {
        return Err(CheckFailure {
            diagnostics: Vec::new(),
            source_paths: parsed.order.clone(),
            partial: parsed,
        });
    }

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut registry = GlobalRegistry::with_stdlib_stubs();

    let source_paths = parsed.order.clone();
    let mut packages = into_packages(parsed);

    // Must run first so derive synthesis and collect see the flat
    // qualified shape.
    desugar::desugar_packages(&mut packages);

    // Pre-collect synthesis: append `impl Debug / Equality for T`
    // blocks so they're present when collect / lift register items.
    // Has to run before collect because the synthesizer introduces
    // new top-level items.
    //
    // The "existing impls" set is collected per-package across all
    // files first so a hand-written `impl Debug for List<T>` in
    // `debug_containers.koja` suppresses synthesis in
    // `list.koja` (and vice versa). Without the cross-file scan
    // we'd get duplicate impls.
    for pkg in &mut packages {
        synthesize::derive_debug::derive_debug_package(pkg);
        synthesize::derive_equality::derive_equality_package(pkg);
    }

    // Collect is a cross-file two-pass: register every declared
    // type first across every file in every package, then register
    // impl blocks. The split lets an `impl Debug for List<T>` in
    // `debug_containers.koja` find the `List` declared in
    // `list.koja` regardless of file order. The alternative is
    // dependency-ordered file walks at the driver layer, which the
    // typechecker shouldn't care about.
    for_each_file(&packages, &mut diagnostics, |file, package, diags| {
        collect::collect_file_decls(file, package, &mut registry, diags);
    });
    for_each_file(&packages, &mut diagnostics, |file, package, diags| {
        collect::collect_file_impls(file, package, &mut registry, diags);
    });

    collect::validate_nested_types(&packages, &registry, &mut diagnostics);

    aliases::validate_aliases(&packages, &registry, &mut diagnostics);

    lift_signatures::lift_signatures(&mut packages, &mut registry, &mut diagnostics);

    visibility::check_signature_leaks(&registry, &mut diagnostics);

    for pkg in &mut packages {
        for file in &mut pkg.files {
            resolve::resolve_file(file, &pkg.package, &registry, &mut diagnostics);
        }
    }

    // Position check on `CPtr.borrow` results. Runs after resolve so
    // static receivers carry their `Resolution::Global` stamp.
    for_each_file(&packages, &mut diagnostics, |file, _package, diags| {
        borrows::check_file(file, &registry, diags);
    });

    // Definite-assignment analysis reads the `LocalId` stamps resolve
    // left on reads and assignment targets.
    for_each_file(&packages, &mut diagnostics, |file, _package, diags| {
        definite_assignment::check_file(file, &registry, diags);
    });

    // Deprecation warnings also read post-resolve stamps.
    for_each_file(&packages, &mut diagnostics, |file, package, diags| {
        deprecation::check_file(file, package, &registry, diags);
    });

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(CheckFailure {
            diagnostics,
            partial: rebuild_parsed(&packages),
            source_paths,
        });
    }

    let checked = CheckedProgram {
        diagnostics,
        packages,
        registry,
        source_paths,
    };
    seal::seal_ast(&checked);
    Ok(checked)
}

/// Run `pass` on every file across every package.
fn for_each_file(
    packages: &[CheckedPackage],
    diagnostics: &mut Vec<Diagnostic>,
    mut pass: impl FnMut(&File, &str, &mut Vec<Diagnostic>),
) {
    for pkg in packages {
        for file in &pkg.files {
            pass(file, &pkg.package, diagnostics);
        }
    }
}

/// Group the parsed files by package, preserving each package's
/// file order from `ParsedProgram::order`.
fn into_packages(parsed: ParsedProgram) -> Vec<CheckedPackage> {
    let ParsedProgram { mut files, order } = parsed;
    let mut by_package: BTreeMap<String, Vec<File>> = BTreeMap::new();
    let mut seen_order: Vec<String> = Vec::new();
    for path in &order {
        if let Some(file) = files.remove(path) {
            let pkg = file.package.clone();
            if !seen_order.contains(&pkg) {
                seen_order.push(pkg.clone());
            }
            by_package.entry(pkg).or_default().push(file.ast);
        }
    }
    seen_order
        .into_iter()
        .map(|package| CheckedPackage {
            files: by_package.remove(&package).unwrap_or_default(),
            package,
        })
        .collect()
}

/// Best-effort reconstruction of a `ParsedProgram` from the in-flight
/// per-package files. Used only on the failure path so LSPs can
/// inspect what made it through before diagnostics halted processing.
fn rebuild_parsed(packages: &[CheckedPackage]) -> ParsedProgram {
    let mut files = BTreeMap::new();
    let mut order = Vec::new();
    for pkg in packages {
        for file in &pkg.files {
            let path = file
                .path
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("<{}>", pkg.package)));
            order.push(path.clone());
            files.insert(
                path.clone(),
                ParsedFile {
                    ast: file.clone(),
                    diagnostics: Vec::new(),
                    package: pkg.package.clone(),
                    path,
                    source: String::new(),
                },
            );
        }
    }
    ParsedProgram { files, order }
}
