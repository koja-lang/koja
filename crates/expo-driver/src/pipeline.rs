//! Compilation pipeline: type checking, context merging, code generation, and linking.
//!
//! This module contains the shared infrastructure that powers `build`, `run`,
//! and `check`. No CLI command functions live here -- those are in [`crate::commands`].

use std::collections::BTreeMap;
use std::path::Path;
use std::{env, fs, process};

use expo_ast::ast::{AnnotationValue, Item, Module, Severity};

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
pub fn build(args: BuildArgs, quiet: bool, color: bool) {
    let path = args.source_file.unwrap_or_else(|| {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    });

    let output = args.output_name.unwrap_or_else(|| {
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
    build_from_graph(&graph, &output, quiet, color, args.emit_llvm);
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

/// A discovered `@test` function: its fully qualified module name, function name,
/// and human-readable description (from `@test "..."` or the function name itself).
struct TestCase {
    fn_name: String,
    description: String,
}

/// Discovers `@test` functions, generates a test harness, compiles and runs it.
pub fn test_project(config: &ProjectConfig, project_root: &Path, color: bool) {
    let mut graph = match resolve::resolve_test_project_modules(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let tests = discover_tests(&graph, &config.name);

    if tests.is_empty() {
        println!("no tests found");
        return;
    }

    let harness_source = generate_harness(&tests, &graph);
    let harness_name = format!("{}.__test_harness__", config.name);

    let parse_result = expo_parser::parse(&harness_source);
    if !parse_result.errors.is_empty() {
        eprintln!("internal error: generated test harness failed to parse");
        for d in &parse_result.errors {
            eprintln!("  {}", d.message);
        }
        process::exit(1);
    }

    graph.order.push(harness_name.clone());
    graph.modules.insert(
        harness_name.clone(),
        resolve::ResolvedModule {
            name: harness_name.clone(),
            path: std::path::PathBuf::from("<test_harness>"),
            source: harness_source,
            module: parse_result.module,
            errors: parse_result.errors,
        },
    );
    graph.entry = harness_name;

    let tmp_dir = env::temp_dir();
    let binary = tmp_dir.join(format!("expo_test_{}", config.name));
    let output = binary.to_str().unwrap().to_string();

    build_from_graph(&graph, &output, true, color, false);

    let status = process::Command::new(&binary).status();
    let _ = fs::remove_file(&binary);

    match status {
        Ok(s) => process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("failed to run test binary: {e}");
            process::exit(1);
        }
    }
}

/// Walks the module graph and collects all functions annotated with `@test`.
fn discover_tests(graph: &ModuleGraph, _project_name: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();

    for name in &graph.order {
        if name.starts_with("std.") {
            continue;
        }

        let rm = &graph.modules[name];
        for item in &rm.module.items {
            if let Item::Function(func) = item
                && let Some(ref ann) = func.annotation
                && ann.name == "test"
            {
                let description = match &ann.value {
                    Some(AnnotationValue::String(s)) => s.clone(),
                    _ => func.name.clone(),
                };
                tests.push(TestCase {
                    fn_name: func.name.clone(),
                    description,
                });
            }
        }
    }

    tests
}

/// Generates the Expo source for a test harness module.
///
/// The harness imports every module that contains tests, then calls each test
/// function sequentially. It prints the test description before calling, and
/// "ok" after it returns. If a test panics, the process aborts and the last
/// printed description identifies the failing test.
fn generate_harness(tests: &[TestCase], graph: &ModuleGraph) -> String {
    let mut imports: Vec<String> = Vec::new();
    let mut seen_modules = std::collections::HashSet::new();

    for rm in graph.modules.values() {
        if rm.name.starts_with("std.") {
            continue;
        }
        if seen_modules.insert(rm.name.clone()) {
            imports.push(format!("import {}", rm.name));
        }
    }

    imports.sort();

    let total = tests.len();
    let mut body = String::new();
    body.push_str(&format!("  print(\"running {} tests\")\n", total));

    for test in tests {
        let escaped_desc = test.description.replace('\\', "\\\\").replace('"', "\\\"");
        body.push_str(&format!("  print(\"test {} ...\")\n", escaped_desc));
        body.push_str(&format!("  {}()\n", test.fn_name));
        body.push_str("  print(\"  ok\")\n");
    }

    body.push_str("  print(\"\")\n");
    body.push_str(&format!("  print(\"{} passed, 0 failed\")\n", total));

    let mut source = String::new();
    for imp in &imports {
        source.push_str(imp);
        source.push('\n');
    }
    source.push('\n');
    source.push_str("fn main\n");
    source.push_str(&body);
    source.push_str("end\n");

    source
}

/// Parsed build arguments.
pub struct BuildArgs {
    pub source_file: Option<String>,
    pub output_name: Option<String>,
    pub emit_llvm: bool,
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
