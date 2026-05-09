//! Public entry point for the alpha typecheck phase. [`check_program`]
//! returns a sealed [`CheckedProgram`] on success or a
//! [`crate::CheckFailure`] carrying diagnostics + the partial
//! `ParsedProgram` on failure. Seal is asserted as the last sub-pass
//! and panics on violation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use expo_ast::ast::{Diagnostic, File, Severity};
use expo_parser::{ParsedFile, ParsedProgram};

use crate::error::CheckFailure;
use crate::pipeline::{collect, lift_signatures, resolve, seal, synthesize};
use crate::registry::GlobalRegistry;

/// A package fragment of a [`CheckedProgram`].
#[derive(Debug, Clone)]
pub struct CheckedPackage {
    pub files: Vec<File>,
    pub package: String,
}

/// Sealed output of [`check_program`]'s success path. Every relevant
/// AST annotation is populated; lowering crates can rely on this
/// without re-validating.
///
/// `diagnostics` carries non-error-severity diagnostics (today:
/// reachability / redundancy warnings on `match` arms). Errors
/// short-circuit to [`crate::CheckFailure`]; only warnings ride the
/// success path. Downstream consumers (driver, LSP) surface them
/// alongside parse-phase warnings.
#[derive(Debug, Clone)]
pub struct CheckedProgram {
    pub diagnostics: Vec<Diagnostic>,
    pub packages: Vec<CheckedPackage>,
    /// Canonical source of truth for what was registered. Lowering
    /// crates build their own indices over `Identifier`.
    pub registry: GlobalRegistry,
}

/// Run every sub-pass in the alpha typecheck phase.
///
/// Short-circuits if `parsed` already carries error-severity parse
/// diagnostics. Otherwise runs the sub-passes in order:
///
/// 0. **preload stdlib stubs** — seed the [`GlobalRegistry`] with
///    [`GlobalRegistry::with_stdlib_stubs`] so `Global.Int`/`.Bool`/
///    `.Unit`/`.Float`/`.String` are registered as structs before any
///    user decl. Temporary; once the real stdlib compiles as a
///    package these entries land through `collect`.
/// 1. `collect` — register every top-level decl. Function signatures
///    land in the `Function(None)` state.
/// 2. `lift_signatures` — resolve each function's `TypeExpr` params +
///    return into `ResolvedType`s and upgrade the registry entry to
///    `Function(Some(signature))`.
/// 3. `synthesize` — surface-shape AST rewrites (today: `for` desugar).
/// 4. `resolve` — walk every body and populate `Resolution` +
///    `Expr.resolution`.
/// 5. `seal` — assert sealed-AST invariants. Panics on violation.
///
/// Future sub-passes (`strip_cfg`, `check`, `annotate`) land between
/// these when the work they do becomes load-bearing.
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure> {
    if parsed.has_errors() {
        return Err(CheckFailure {
            diagnostics: Vec::new(),
            partial: parsed,
        });
    }

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut registry = GlobalRegistry::with_stdlib_stubs();

    let mut packages = into_packages(parsed);

    for pkg in &packages {
        for file in &pkg.files {
            collect::collect_file(file, &pkg.package, &mut registry, &mut diagnostics);
        }
    }

    lift_signatures::lift_signatures(&mut packages, &mut registry, &mut diagnostics);

    synthesize::synthesize_program(&mut packages);

    for pkg in &mut packages {
        for file in &mut pkg.files {
            resolve::resolve_file(file, &pkg.package, &registry, &mut diagnostics);
        }
    }

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(CheckFailure {
            diagnostics,
            partial: rebuild_parsed(&packages),
        });
    }

    let checked = CheckedProgram {
        diagnostics,
        packages,
        registry,
    };
    seal::seal_ast(&checked);
    Ok(checked)
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
