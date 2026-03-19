//! Compilation pipeline: type checking, context merging, code generation, and linking.
//!
//! This module contains the shared infrastructure that powers `build`, `run`,
//! and `check`. No CLI command functions live here -- those are in [`crate::commands`].

use std::collections::HashMap;
use std::path::Path;
use std::{env, fs, process};

use expo_ast::ast::{Module, Severity};

use crate::diagnostics::render_diagnostics;
use crate::resolve;

/// Parsed and type-collected standard library, ready to be merged into
/// user module contexts.
pub struct Stdlib {
    pub ctx: expo_typecheck::context::TypeContext,
    pub module: Module,
}

/// Parses the kernel source embedded in `expo_typecheck` and collects its
/// type information. Called once per compilation.
pub fn parse_stdlib() -> Stdlib {
    let parse_result = expo_parser::parse(expo_typecheck::KERNEL_SOURCE);
    let ctx = expo_typecheck::collect_module(&parse_result.module);
    Stdlib {
        ctx,
        module: parse_result.module,
    }
}

/// Runs the type-checking pipeline for every module in the resolved graph.
///
/// For each module (in topological order): collects types, merges the stdlib,
/// re-resolves generics, resolves cross-module imports, and runs the checker.
/// Returns the per-module type contexts and whether any errors were found.
pub fn typecheck_modules(
    graph: &resolve::ModuleGraph,
    stdlib: &Stdlib,
    color: bool,
) -> (HashMap<String, expo_typecheck::context::TypeContext>, bool) {
    let mut module_contexts: HashMap<String, expo_typecheck::context::TypeContext> = HashMap::new();
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

    for name in &graph.order {
        let rm = &graph.modules[name];
        let mut ctx = expo_typecheck::collect_module(&rm.module);
        expo_typecheck::merge_stdlib(&stdlib.ctx, &mut ctx);
        expo_typecheck::re_resolve_generics(&mut ctx);
        expo_typecheck::mark_recursive_fields(&mut ctx);
        expo_typecheck::resolve_imports(&rm.module, &mut ctx, &module_contexts);
        expo_typecheck::check_module(&rm.module, &mut ctx);
        module_contexts.insert(name.clone(), ctx);
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

/// Full build pipeline: resolve modules, type-check, merge contexts, codegen,
/// and link into an executable.
///
/// When `quiet` is true, the "compiled: <output>" message is suppressed
/// (used by `expo run` to avoid noise).
pub fn build(args: &[String], quiet: bool, color: bool) {
    if args.is_empty() {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    }

    let (source_file, output_name) = parse_build_args(args);

    let path = source_file.unwrap_or_else(|| {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    });

    let output = output_name.unwrap_or_else(|| {
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

    let graph = match resolve::resolve_modules(&entry_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let stdlib = parse_stdlib();
    let (module_contexts, has_errors) = typecheck_modules(&graph, &stdlib, color);
    if has_errors {
        process::exit(1);
    }

    let mut merged_ctx = expo_typecheck::context::TypeContext::new();
    expo_typecheck::merge_stdlib(&stdlib.ctx, &mut merged_ctx);
    for ctx in module_contexts.values() {
        merged_ctx.merge(ctx);
    }

    let mut modules_ast: Vec<&Module> = vec![&stdlib.module];
    modules_ast.extend(graph.order.iter().map(|name| &graph.modules[name].module));

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

    link(&obj_path, &output, quiet);
}

/// Extracts `-o <output>` and the source file path from build arguments.
fn parse_build_args(args: &[String]) -> (Option<String>, Option<String>) {
    let mut source_file = None;
    let mut output_name = None;
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
        } else {
            source_file = Some(args[i].clone());
            i += 1;
        }
    }
    (source_file, output_name)
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
