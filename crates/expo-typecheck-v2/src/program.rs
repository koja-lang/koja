//! The single public entry point for the v2 typecheck phase.
//!
//! [`check_program`] consumes a [`ParsedProgram`] and either:
//! - returns `Ok(CheckedProgram)` whose AST is **sealed** (every
//!   `Expr.resolved_type` populated; every `Resolution` either
//!   `Global(Identifier)` or `Unresolved` only on nodes the seal
//!   contract excludes), or
//! - returns `Err(CheckFailure)` carrying diagnostics + the partial
//!   `ParsedProgram` for LSP best-effort consumption.
//!
//! The seal is asserted as the last step inside `check_program`; seal
//! violations panic per northstar (compiler bugs, not user errors).

use std::collections::BTreeMap;
use std::path::PathBuf;

use expo_ast::ast::{Diagnostic, File};
use expo_parser::{ParsedFile, ParsedProgram};

use crate::registry::GlobalRegistry;
use crate::{annotate, check, collect, lift_signatures, resolve, seal, strip_cfg, synthesize};

/// A package fragment of a [`CheckedProgram`]: the package name plus
/// the set of sealed AST files that belong to it.
#[derive(Debug, Clone)]
pub struct CheckedPackage {
    pub files: Vec<File>,
    pub package: String,
}

/// Sealed output of [`check_program`]'s success path. Every relevant
/// AST annotation is populated. Lowering crates can rely on this
/// without re-validating.
#[derive(Debug, Clone)]
pub struct CheckedProgram {
    pub packages: Vec<CheckedPackage>,
    /// Whole-program registry of resolved top-level decls. Lowering
    /// crates build their own indices over `Identifier`; this registry
    /// is the canonical source of truth for what was registered.
    pub registry: GlobalRegistry,
}

/// Failure result of [`check_program`].
///
/// `diagnostics` carries **only** the diagnostics typecheck-v2 itself
/// emitted. Parse diagnostics live on `partial.iter().flat_map(|f|
/// &f.diagnostics)` — the parse stage owns those. When the parser had
/// already produced error-severity diagnostics, typecheck halts early
/// without contributing anything; in that case `diagnostics` is empty
/// and the caller sources errors entirely from `partial`.
///
/// The partial AST is **not** sealed.
#[derive(Debug)]
pub struct CheckFailure {
    pub diagnostics: Vec<Diagnostic>,
    pub partial: ParsedProgram,
}

impl std::fmt::Display for CheckFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for diag in &self.diagnostics {
            writeln!(f, "{}", diag.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for CheckFailure {}

/// Run every sub-pass in the v2 typecheck phase.
///
/// Halts immediately if `parsed` already carries error-severity parse
/// diagnostics (those belong to the parse stage; consumers read them
/// from `parsed.iter()`). Otherwise runs the sub-passes in order:
///
/// 1. `strip_cfg` — prune nodes excluded by `@cfg(...)` (no-op today).
/// 2. `collect` — register every surviving top-level decl into the
///    registry. Identifiers only; signatures stay at placeholders.
/// 3. `synthesize` — generate default protocol impl ASTs (no-op today).
/// 4. `lift_signatures` — resolve and annotate every registered decl's
///    signature on the AST using the now-populated registry. No-op
///    today; lands when the first real cross-decl reference does.
/// 5. `resolve` — walk every body and populate `Resolution` +
///    `Expr.resolved_type`.
/// 6. `check` — validate type compatibility.
/// 7. `annotate` — emit coercion annotations (no-op today).
/// 8. `seal` — assert sealed-AST invariants. Panics on violation.
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure> {
    if parsed.has_errors() {
        return Err(CheckFailure {
            diagnostics: Vec::new(),
            partial: parsed,
        });
    }

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut registry = GlobalRegistry::default();

    let packages = strip_cfg::strip_cfg(into_packages(parsed));

    for pkg in &packages {
        for file in &pkg.files {
            collect::collect_file(file, &pkg.package, &mut registry, &mut diagnostics);
        }
    }

    let mut packages = synthesize::synthesize(packages, &mut registry, &mut diagnostics);

    for pkg in &packages {
        for file in &pkg.files {
            lift_signatures::lift_signatures_in_file(
                file,
                &pkg.package,
                &mut registry,
                &mut diagnostics,
            );
        }
    }

    for pkg in &mut packages {
        for file in &mut pkg.files {
            resolve::resolve_file(file, &registry, &mut diagnostics);
        }
    }

    for pkg in &packages {
        for file in &pkg.files {
            check::check_file(file, &registry, &mut diagnostics);
        }
    }

    let packages = annotate::annotate(packages, &registry, &mut diagnostics);

    if !diagnostics.is_empty() {
        return Err(CheckFailure {
            diagnostics,
            partial: rebuild_parsed(&packages),
        });
    }

    let checked = CheckedProgram { packages, registry };
    seal::seal_ast(&checked);
    Ok(checked)
}

/// Group the parsed files by package name, preserving each package's
/// internal file order from `ParsedProgram::order`.
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
/// per-package files. Used only on the failure path so the LSP can
/// inspect what made it through the pipeline before diagnostics
/// stopped further processing.
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
