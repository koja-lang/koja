//! The single public entry point for the alpha typecheck phase.
//!
//! [`check_program`] consumes a [`ParsedProgram`] and either:
//! - returns `Ok(CheckedProgram)` whose AST is **sealed** (every
//!   `Expr.resolution` fully resolved into the registry; every
//!   `Resolution` either `Global(GlobalRegistryId)` or `Unresolved`
//!   only on nodes the seal contract excludes), or
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
use crate::{collect, lift_signatures, resolve, seal};

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
/// `diagnostics` carries **only** the diagnostics alpha typecheck itself
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

/// Run every sub-pass in the alpha typecheck phase.
///
/// Halts immediately if `parsed` already carries error-severity parse
/// diagnostics (those belong to the parse stage; consumers read them
/// from `parsed.iter()`). Otherwise runs the sub-passes in order:
///
/// 0. **preload stdlib stubs** — seed the [`GlobalRegistry`] with
///    [`GlobalRegistry::with_stdlib_stubs`] so `Global.Int`, `.Bool`,
///    `.Unit`, `.Float`, `.String` are registered as structs before
///    any user decl. Temporary: once the real stdlib is compiled as a
///    package these entries land through `collect` like any other
///    decl.
/// 1. `collect` — register every top-level decl into the registry.
///    Identifiers only; function signatures land in the
///    `Function(None)` state, waiting for `lift_signatures` to stamp
///    them in.
/// 2. `lift_signatures` — walk each function's signature, resolve
///    its `TypeExpr` params + return into `ResolvedType`s, and
///    upgrade its registry entry to `Function(Some(signature))`.
///    This has to happen after `collect` (so cross-references
///    resolve) and before `resolve` (so call sites can look up
///    callee signatures).
/// 3. `resolve` — walk every body and populate `Resolution` +
///    `Expr.resolution` (a [`expo_ast::identifier::ResolvedType`]
///    pointing into the registry). Walks `File.items[Function]`
///    bodies and `File.body` (script-mode top-level statements)
///    uniformly.
/// 4. `seal` — assert sealed-AST invariants. Both project-mode files
///    (body `None`, `Function` items carry the work) and script-mode
///    files (body `Some(_)` of resolved statements, no synthesized
///    `fn main`) are accepted. Panics on violation.
///
/// Future sub-passes land in this orchestration when the work they do
/// becomes load-bearing — `strip_cfg` for `@cfg`-driven pruning,
/// `synthesize` between `collect` and `lift_signatures` for protocol
/// defaults, `check` between `resolve` and `seal` for compatibility
/// validation beyond what `resolve` enforces inline, and `annotate`
/// between `check` and `seal` for coercion emission. They're not in
/// the pipeline yet because the POC has nothing for them to do.
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

    for pkg in &packages {
        for file in &pkg.files {
            lift_signatures::lift_signatures(file, &pkg.package, &mut registry, &mut diagnostics);
        }
    }

    for pkg in &mut packages {
        for file in &mut pkg.files {
            resolve::resolve_file(file, &pkg.package, &registry, &mut diagnostics);
        }
    }

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
