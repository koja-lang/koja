//! Whole-program type-checking entry point.
//!
//! [`check_program`] is the sealed handoff between parsing and codegen:
//! it consumes a [`ParsedProgram`] from `expo-parser`, walks every
//! type-checking sub-pass in order, and returns a flat
//! [`CheckedProgram`] whose `ast: Vec<File>` is the post-typecheck
//! shape downstream stages (codegen, eventually IR) actually see. The
//! `ParsedProgram` does not leak forward -- once typecheck has consumed
//! it, the parser-phase bundle is gone.
//!
//! Diagnostics are emitted through a [`DiagnosticSink`] at the same
//! three lifecycle moments today's driver-inline rendering uses:
//! parse-error pass, `scan_globals` pass, and the post-check pass.
//! Drivers that want terminal rendering implement the trait themselves
//! (see `expo-driver`'s `TerminalSink`); LSP-style consumers can keep
//! diagnostics in memory by returning them from their own sink.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use expo_ast::ast::{Diagnostic, File, Severity};
use expo_parser::{ParsedFile, ParsedProgram};

use crate::context::TypeContext;
use crate::registry::GlobalRegistry;
use crate::types::Package;
use crate::{
    GlobalNames, check_file, collect_all_names, collect_file, mark_recursive_fields,
    resolve_file_aliases, resolve_packages, scan_globals, synthesize_protocol_defaults,
    validate_resolved_types,
};

/// Sink for diagnostics produced during [`check_program`]. Implementors
/// receive batches scoped to a single file (path + source for span
/// rendering); the sink is invoked at the same lifecycle moments as
/// today's driver-inline rendering: once per file with parse errors,
/// once per file with `scan_globals` errors, once per file with
/// check-phase errors.
pub trait DiagnosticSink {
    fn emit(&mut self, path: &Path, source: &str, diagnostics: &[Diagnostic]);
}

/// Sealed output of [`check_program`]: the file ASTs (mutated in place
/// by typecheck; each [`File`] carries its own `package` and `path`),
/// per-file contexts keyed by path, the codegen-ready merged context,
/// and a single `has_errors` flag the driver checks before proceeding
/// to codegen.
///
/// Deliberately flat: no `ParsedProgram`, no `ParsedFile`, no source
/// text. Once typecheck has consumed the parsed bundle, the
/// parser-phase shape is gone and downstream stages see only the
/// validated ASTs they actually need.
pub struct CheckedProgram {
    pub ast: Vec<File>,
    pub file_contexts: BTreeMap<PathBuf, TypeContext>,
    pub has_errors: bool,
    pub merged_ctx: TypeContext,
}

/// Runs every type-checking phase against `parsed`, emitting
/// diagnostics through `sink` at today's lifecycle moments. Each
/// phase is its own helper below so this body reads as a script.
pub fn check_program(parsed: ParsedProgram, sink: &mut dyn DiagnosticSink) -> CheckedProgram {
    let mut parsed = parsed;

    if render_parse_diagnostics(&parsed, sink) {
        return seal_empty(parsed, true);
    }
    if scan_globals_pass(&parsed, sink) {
        return seal_empty(parsed, true);
    }

    let global_names = build_global_names(&parsed);
    let mut file_contexts: BTreeMap<PathBuf, TypeContext> = BTreeMap::new();
    let stdlib_ctx = gather_stdlib(&mut parsed, &global_names, &mut file_contexts);
    gather_project(&mut parsed, &global_names, &stdlib_ctx, &mut file_contexts);
    let unified = unify_project_contexts(&stdlib_ctx, &file_contexts, &parsed);
    let mut has_errors = false;
    check_pass(
        &mut parsed,
        &mut file_contexts,
        &unified,
        &mut has_errors,
        sink,
    );

    let merged_ctx = build_merged_ctx(&parsed, &file_contexts);
    let ast = into_ast(parsed);
    CheckedProgram {
        ast,
        file_contexts,
        has_errors,
        merged_ctx,
    }
}

// =============================================================================
// Phase helpers
// =============================================================================

/// Emit every file's parse-error diagnostics. Returns true if any file
/// produced an error-severity diagnostic, signalling the orchestrator
/// to short-circuit.
fn render_parse_diagnostics(parsed: &ParsedProgram, sink: &mut dyn DiagnosticSink) -> bool {
    let mut has_errors = false;
    for file in parsed.iter() {
        if file.diagnostics.is_empty() {
            continue;
        }
        sink.emit(&file.path, &file.source, &file.diagnostics);
        if file.has_errors() {
            has_errors = true;
        }
    }
    has_errors
}

/// Build the program-wide identifier registry so cross-file
/// duplicate-decl collisions surface as errors before the
/// gather-unify-check loop ever runs. Each file's diagnostics are
/// scoped to the second-defined site, rendering against the file
/// that introduced the collision. Returns true on any error-severity
/// diagnostic so the orchestrator can short-circuit.
fn scan_globals_pass(parsed: &ParsedProgram, sink: &mut dyn DiagnosticSink) -> bool {
    let mut has_errors = false;
    let mut shared_registry = GlobalRegistry::new();
    for file in parsed.iter() {
        let scan_diags = scan_globals(&file.ast, &file.package, &mut shared_registry);
        if scan_diags.is_empty() {
            continue;
        }
        sink.emit(&file.path, &file.source, &scan_diags);
        if scan_diags.iter().any(|d| d.severity == Severity::Error) {
            has_errors = true;
        }
    }
    has_errors
}

/// Collect program-wide [`GlobalNames`] for collection-phase lookups.
/// Stdlib files collapse to a single [`Package::Std`]; every other
/// file's package becomes a [`Package::Named`] so the type resolver
/// can validate qualified `pkg.Type` paths during signature collection.
fn build_global_names(parsed: &ParsedProgram) -> GlobalNames {
    let all_files: Vec<&File> = parsed.iter().map(|f| &f.ast).collect();
    let mut packages: BTreeSet<Package> = BTreeSet::new();
    for file in parsed.iter() {
        if file.package == "std" {
            packages.insert(Package::Std);
        } else {
            packages.insert(Package::Named(file.package.clone()));
        }
    }
    collect_all_names(&all_files, packages)
}

/// Collect every stdlib file into a shared `stdlib_ctx`. Each file's
/// per-file context is also stashed in `file_contexts` so the
/// downstream check pass and post-check diagnostic emission see every
/// file (stdlib included). `collect_file` runs the synthesize sub-pass
/// internally (auto-derives `impl Debug for T`), so stdlib ASTs gain
/// synthesized items as a side effect.
fn gather_stdlib(
    parsed: &mut ParsedProgram,
    global_names: &GlobalNames,
    file_contexts: &mut BTreeMap<PathBuf, TypeContext>,
) -> TypeContext {
    let mut stdlib_ctx = TypeContext::new();
    let stdlib_paths: Vec<PathBuf> = parsed
        .order
        .iter()
        .filter(|p| parsed.files[*p].package == "std")
        .cloned()
        .collect();
    for path in &stdlib_paths {
        let file = parsed.files.get_mut(path).expect("file present");
        let mut ctx = collect_file(&mut file.ast, global_names, "std");
        ctx.merge(&stdlib_ctx);
        stdlib_ctx.merge(&ctx);
        file_contexts.insert(path.clone(), ctx);
    }
    stdlib_ctx
}

/// Gather signatures from every project file. Each file's context is
/// pre-merged with `stdlib_ctx` so signature lookups during collection
/// see stdlib types; protocol defaults are synthesized in place; and
/// recursive struct/enum fields are marked for indirection. Each
/// gathered context is appended to `file_contexts` alongside the
/// stdlib entries so the check pass below has a single map to work
/// against.
fn gather_project(
    parsed: &mut ParsedProgram,
    global_names: &GlobalNames,
    stdlib_ctx: &TypeContext,
    file_contexts: &mut BTreeMap<PathBuf, TypeContext>,
) {
    let project_paths: Vec<PathBuf> = parsed
        .order
        .iter()
        .filter(|p| parsed.files[*p].package != "std")
        .cloned()
        .collect();
    for path in &project_paths {
        let file = parsed.files.get_mut(path).expect("file present");
        let pkg = file.package.clone();
        let mut ctx = collect_file(&mut file.ast, global_names, &pkg);
        ctx.merge(stdlib_ctx);
        synthesize_protocol_defaults(&file.ast, &mut ctx, &pkg);
        mark_recursive_fields(&mut ctx);
        file_contexts.insert(path.clone(), ctx);
    }
}

/// Build the unified context every check-phase pass merges against:
/// `stdlib_ctx` plus every project file's gathered context. Same
/// iteration order (`parsed.order`) the check pass uses below; stdlib
/// paths are skipped because their contexts are already in `stdlib_ctx`.
fn unify_project_contexts(
    stdlib_ctx: &TypeContext,
    file_contexts: &BTreeMap<PathBuf, TypeContext>,
    parsed: &ParsedProgram,
) -> TypeContext {
    let mut unified = stdlib_ctx.clone();
    for path in &parsed.order {
        let file = &parsed.files[path];
        if file.package == "std" {
            continue;
        }
        if let Some(ctx) = file_contexts.get(path) {
            unified.merge(ctx);
        }
    }
    unified
}

/// Per-file alias resolution + package resolution + body checking +
/// resolved-type validation, then emit each file's accumulated
/// diagnostics. Embedded stdlib files are checked alongside workspace
/// sources so every expression carries a populated `resolved_type` --
/// downstream codegen / IR lowering relies on this invariant.
fn check_pass(
    parsed: &mut ParsedProgram,
    file_contexts: &mut BTreeMap<PathBuf, TypeContext>,
    unified: &TypeContext,
    has_errors: &mut bool,
    sink: &mut dyn DiagnosticSink,
) {
    let order_clone: Vec<PathBuf> = parsed.order.clone();
    for path in &order_clone {
        let Some(mut ctx) = file_contexts.remove(path) else {
            continue;
        };
        ctx.merge(unified);
        let file = parsed.files.get_mut(path).expect("file present");
        let pkg = file.package.clone();
        resolve_file_aliases(&file.ast, &mut ctx);
        resolve_packages(&mut ctx);
        check_file(&mut file.ast, &mut ctx, &pkg);
        validate_resolved_types(&file.ast, &mut ctx);
        file_contexts.insert(path.clone(), ctx);
    }

    for file in parsed.iter() {
        let ctx = &file_contexts[&file.path];
        if ctx.diagnostics.is_empty() {
            continue;
        }
        sink.emit(&file.path, &file.source, &ctx.diagnostics);
        if ctx
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            *has_errors = true;
        }
    }
}

/// Build the codegen-ready merged context: concatenate every per-file
/// context in `parsed.order`, then run the package-resolution pass so
/// codegen sees only fully-qualified `Package` identifiers (no
/// lingering `Package::Unresolved`). Mirrors today's
/// driver-inline merge that codegen consumed directly.
fn build_merged_ctx(
    parsed: &ParsedProgram,
    file_contexts: &BTreeMap<PathBuf, TypeContext>,
) -> TypeContext {
    let mut merged = TypeContext::new();
    for path in &parsed.order {
        if let Some(ctx) = file_contexts.get(path) {
            merged.merge(ctx);
        }
    }
    resolve_packages(&mut merged);
    merged
}

/// Drain `parsed.files` in `parsed.order` into a flat `Vec<File>`,
/// dropping per-file `source` and `diagnostics` (already emitted
/// through the sink). Each `File` carries its own `package` and
/// `path` for downstream identification.
fn into_ast(mut parsed: ParsedProgram) -> Vec<File> {
    let mut ast = Vec::with_capacity(parsed.order.len());
    for path in &parsed.order {
        if let Some(ParsedFile { ast: file_ast, .. }) = parsed.files.remove(path) {
            ast.push(file_ast);
        }
    }
    ast
}

/// Construct the empty-but-flagged sealed program for the parse-error
/// and scan-error short-circuits. Carries the ASTs the parser produced
/// (so callers that want a partial dump still have something to walk)
/// but no contexts and no merged context.
fn seal_empty(parsed: ParsedProgram, has_errors: bool) -> CheckedProgram {
    let ast = into_ast(parsed);
    CheckedProgram {
        ast,
        file_contexts: BTreeMap::new(),
        has_errors,
        merged_ctx: TypeContext::new(),
    }
}
