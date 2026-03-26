//! Compilation pipeline: type checking, context merging, code generation, and linking.
//!
//! This module contains the shared infrastructure that powers `build`, `run`,
//! and `check`. No CLI command functions live here -- those are in [`crate::commands`].

use std::collections::BTreeMap;
use std::path::Path;
use std::{env, fs, process};

use expo_ast::ast::{Module, Severity};

use crate::diagnostics::render_diagnostics;
use crate::project::ProjectConfig;
use crate::resolve::{self, ModuleGraph};

/// Runs the type-checking pipeline for every module in a graph that includes
/// stdlib modules. Stdlib context is accumulated as stdlib modules are processed,
/// then merged into every subsequent module.
///
/// Returns the per-module type contexts and whether any errors were found.
pub fn typecheck_graph(
    graph: &ModuleGraph,
    color: bool,
) -> (BTreeMap<String, expo_typecheck::context::TypeContext>, bool) {
    let mut module_contexts: BTreeMap<String, expo_typecheck::context::TypeContext> =
        BTreeMap::new();
    let mut has_errors = false;

    for name in &graph.order {
        let rm = &graph.modules[name];
        if !rm.errors.is_empty() {
            render_diagnostics(
                rm.path.to_str().unwrap_or(&rm.name),
                &rm.source,
                &rm.errors,
                color,
            );
            has_errors = true;
        }
    }
    if has_errors {
        return (module_contexts, true);
    }

    // Phase 1: collect all struct/enum names across every module so that
    // cross-module type references resolve on the first pass.
    let all_modules: Vec<&Module> = graph
        .order
        .iter()
        .map(|n| &graph.modules[n].module)
        .collect();
    let global_names = expo_typecheck::collect_all_names(&all_modules);

    let mut stdlib_ctx = expo_typecheck::context::TypeContext::new();

    // Phase 2: per-module collection and type checking.
    for name in &graph.order {
        let rm = &graph.modules[name];

        if name.starts_with("std.") {
            let mut ctx = expo_typecheck::collect_module(&rm.module, &global_names);
            ctx.merge(&stdlib_ctx);
            stdlib_ctx.merge(&ctx);
            module_contexts.insert(name.clone(), ctx);
        } else {
            let mut ctx = expo_typecheck::collect_module(&rm.module, &global_names);
            ctx.merge(&stdlib_ctx);
            expo_typecheck::synthesize_protocol_defaults(&rm.module, &mut ctx);
            expo_typecheck::mark_recursive_fields(&mut ctx);
            expo_typecheck::resolve_imports(&rm.module, &mut ctx, &module_contexts);
            expo_typecheck::check_module(&rm.module, &mut ctx);
            module_contexts.insert(name.clone(), ctx);
        }
    }

    for name in &graph.order {
        let rm = &graph.modules[name];
        let ctx = &module_contexts[name];
        if !ctx.diagnostics.is_empty() {
            render_diagnostics(
                rm.path.to_str().unwrap_or(&rm.name),
                &rm.source,
                &ctx.diagnostics,
                color,
            );
            if ctx
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)
            {
                has_errors = true;
            }
        }
    }

    (module_contexts, has_errors)
}

/// Compiles a fully resolved module graph into an executable.
///
/// Type-checks all modules, merges contexts, emits LLVM IR, and links.
/// The graph must include stdlib modules (via [`resolve::insert_stdlib`]).
/// When `emit_llvm` is true, prints LLVM IR to stdout instead of linking.
pub fn build_from_graph(
    graph: &ModuleGraph,
    output: &str,
    quiet: bool,
    color: bool,
    emit_llvm: bool,
) {
    let (module_contexts, has_errors) = typecheck_graph(graph, color);
    if has_errors {
        process::exit(1);
    }

    let mut merged_ctx = expo_typecheck::context::TypeContext::new();
    for name in &graph.order {
        merged_ctx.merge(&module_contexts[name]);
    }

    let modules_ast: Vec<&Module> = graph
        .order
        .iter()
        .map(|name| &graph.modules[name].module)
        .collect();

    if emit_llvm {
        match expo_codegen::emit_llvm_ir(&modules_ast, &merged_ctx) {
            Ok(ir) => print!("{ir}"),
            Err(diagnostics) => {
                let entry_rm = &graph.modules[&graph.entry];
                render_diagnostics(
                    entry_rm.path.to_str().unwrap_or(&entry_rm.name),
                    &entry_rm.source,
                    &diagnostics,
                    color,
                );
                process::exit(1);
            }
        }
        return;
    }

    let obj_path = format!("{output}.o");
    if let Err(diagnostics) =
        expo_codegen::compile_modules(&modules_ast, &merged_ctx, Path::new(&obj_path))
    {
        let entry_rm = &graph.modules[&graph.entry];
        render_diagnostics(
            entry_rm.path.to_str().unwrap_or(&entry_rm.name),
            &entry_rm.source,
            &diagnostics,
            color,
        );
        process::exit(1);
    }

    link(&obj_path, output, quiet);
}

/// Builds a project from its config: resolves modules, type-checks, compiles, links.
pub fn build_project(
    config: &ProjectConfig,
    project_root: &Path,
    output: Option<&str>,
    quiet: bool,
    color: bool,
    emit_llvm: bool,
) {
    let graph = match resolve::resolve_project_modules(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let output = output.unwrap_or(&config.name);
    build_from_graph(&graph, output, quiet, color, emit_llvm);
}

/// Full single-file build pipeline: resolve modules from an entry file,
/// type-check, merge contexts, codegen, and link into an executable.
///
/// When `quiet` is true, the "compiled: <output>" message is suppressed
/// (used by `expo run` to avoid noise).
pub fn build(args: &[String], quiet: bool, color: bool, emit_llvm: bool) {
    if args.is_empty() {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    }

    let build_args = parse_build_args(args);
    let emit_llvm = emit_llvm || build_args.emit_llvm;

    let path = build_args.source_file.unwrap_or_else(|| {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    });

    let output = build_args.output_name.unwrap_or_else(|| {
        Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string()
    });

    let entry_path = Path::new(&path).canonicalize().unwrap_or_else(|_| {
        eprintln!("error: file not found: {path}");
        process::exit(1);
    });

    let mut graph = match resolve::resolve_modules(&entry_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    prepend_stdlib(&mut graph);
    build_from_graph(&graph, &output, quiet, color, emit_llvm);
}

/// Type-checks a single-file module graph (without compiling).
pub fn check_single_file(entry_path: &Path, color: bool) -> bool {
    let mut graph = match resolve::resolve_modules(entry_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    prepend_stdlib(&mut graph);
    let (_, has_errors) = typecheck_graph(&graph, color);
    has_errors
}

/// Type-checks a project module graph (without compiling).
pub fn check_project(config: &ProjectConfig, project_root: &Path, color: bool) -> bool {
    let graph = match resolve::resolve_project_modules(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    let (_, has_errors) = typecheck_graph(&graph, color);
    has_errors
}

/// Inserts stdlib modules at the front of an existing graph's order.
/// Used for single-file mode where the graph is built without stdlib.
fn prepend_stdlib(graph: &mut ModuleGraph) {
    let mut stdlib_order = Vec::new();
    for &(name, source) in expo_stdlib::SOURCES {
        let parse_result = expo_parser::parse(source);
        stdlib_order.push(name.to_string());
        graph.modules.insert(
            name.to_string(),
            resolve::ResolvedModule {
                name: name.to_string(),
                path: std::path::PathBuf::from(format!("<{name}>")),
                source: source.to_string(),
                module: parse_result.module,
                errors: parse_result.errors,
            },
        );
    }
    stdlib_order.append(&mut graph.order);
    graph.order = stdlib_order;
}

/// Parsed build arguments.
pub struct BuildArgs {
    pub source_file: Option<String>,
    pub output_name: Option<String>,
    pub emit_llvm: bool,
}

/// Extracts `-o <output>`, `--emit-llvm`, and the source file path from build arguments.
pub fn parse_build_args(args: &[String]) -> BuildArgs {
    let mut source_file = None;
    let mut output_name = None;
    let mut emit_llvm = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" {
            if i + 1 < args.len() {
                output_name = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("-o requires an argument");
                process::exit(1);
            }
        } else if args[i] == "--emit-llvm" {
            emit_llvm = true;
            i += 1;
        } else {
            source_file = Some(args[i].clone());
            i += 1;
        }
    }
    BuildArgs {
        source_file,
        output_name,
        emit_llvm,
    }
}

/// Links an object file with the embedded runtime library to produce an executable.
fn link(obj_path: &str, output: &str, quiet: bool) {
    let runtime_lib_bytes: &[u8] = include_bytes!(env!("EXPO_RUNTIME_LIB_PATH"));
    let tmp_dir = env::temp_dir();
    let tmp_lib = tmp_dir.join("libexpo_runtime.a");
    fs::write(&tmp_lib, runtime_lib_bytes).expect("failed to write embedded runtime library");
    let tmp_dir_str = tmp_dir.to_string_lossy();

    let status = process::Command::new("cc")
        .args([obj_path, "-lexpo_runtime", "-L", &tmp_dir_str, "-o", output])
        .status();

    let _ = fs::remove_file(&tmp_lib);

    match status {
        Ok(s) if s.success() => {
            let _ = fs::remove_file(obj_path);
            if !quiet {
                println!("compiled: {output}");
            }
        }
        Ok(s) => {
            eprintln!("linker failed with exit code: {}", s.code().unwrap_or(-1));
            let _ = fs::remove_file(obj_path);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to run linker: {e}");
            let _ = fs::remove_file(obj_path);
            process::exit(1);
        }
    }
}
