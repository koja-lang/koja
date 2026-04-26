//! Compilation pipeline: type checking, context merging, code generation, and linking.
//!
//! This module contains the shared infrastructure that powers `build`, `run`,
//! and `check`. No CLI command functions live here -- those are in [`crate::commands`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::{env, fs, mem, process};

use expo_ast::ast::{Annotation, AnnotationValue, ImplMember, Item, Module, Severity};

use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Package, fqn_to_package};

use crate::diagnostics::render_diagnostics;
use crate::project::ProjectConfig;
use crate::resolve::{self, ModuleGraph};

/// Builds the set of [`Package`]s visible to the resolver from a module
/// graph's FQNs. `std.*` modules collapse to a single [`Package::Std`];
/// every other module's leading segment becomes a [`Package::Named`] so the
/// type resolver can validate qualified `pkg.Type` paths during signature
/// collection without waiting for the full type tables to be merged.
fn packages_from_module_names<I, S>(names: I) -> BTreeSet<Package>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut packages = BTreeSet::new();
    for name in names {
        let pkg = fqn_to_package(name.as_ref());
        if pkg == "std" {
            packages.insert(Package::Std);
        } else {
            packages.insert(Package::Named(pkg.to_string()));
        }
    }
    packages
}

/// Runs the type-checking pipeline for every module in a graph.
///
/// Stdlib modules are processed sequentially (they have a defined dependency
/// order). Project modules use a gather-unify-check pipeline:
///   1. **Gather** – collect type signatures from every project module.
///   2. **Unify** – merge all project contexts into a single shared context
///      so every module sees every other module's types without imports.
///   3. **Check** – type-check each project module against the unified context.
///
/// Returns the per-module type contexts and whether any errors were found.
pub fn typecheck_graph(
    graph: &mut ModuleGraph,
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

    let all_modules: Vec<&Module> = graph
        .order
        .iter()
        .map(|n| &graph.modules[n].module)
        .collect();
    let known_packages = packages_from_module_names(&graph.order);
    let global_names = expo_typecheck::collect_all_names(&all_modules, known_packages);

    let mut stdlib_ctx = expo_typecheck::context::TypeContext::new();

    let is_stdlib = |n: &str| n.starts_with("std.");

    let (stdlib_names, project_names): (Vec<&String>, Vec<&String>) =
        graph.order.iter().partition(|n| is_stdlib(n));

    // Auto-imported std modules: merge into stdlib_ctx directly.
    for name in &stdlib_names {
        let rm = &graph.modules[*name];
        let mut ctx = expo_typecheck::collect_module(&rm.module, &global_names, "std");
        ctx.merge(&stdlib_ctx);

        stdlib_ctx.merge(&ctx);
        module_contexts.insert((*name).clone(), ctx);
    }

    // Gather: collect signatures from every project module.
    for name in &project_names {
        let rm = &graph.modules[*name];
        let pkg = fqn_to_package(name);
        let mut ctx = expo_typecheck::collect_module(&rm.module, &global_names, pkg);
        ctx.merge(&stdlib_ctx);
        expo_typecheck::auto_derive_debug(&mut ctx);
        expo_typecheck::synthesize_protocol_defaults(&rm.module, &mut ctx, pkg);
        expo_typecheck::mark_recursive_fields(&mut ctx);
        module_contexts.insert((*name).clone(), ctx);
    }

    // Unify: build a shared context containing all project definitions.
    let mut unified_project_ctx = stdlib_ctx.clone();
    for name in &project_names {
        unified_project_ctx.merge(&module_contexts[*name]);
    }

    // Check: every module needs alias resolution, package resolution, and
    // body typechecking. Embedded stdlib modules (synthetic `<std.x>` paths)
    // are checked alongside workspace sources so every expression carries a
    // populated `resolved_type` -- downstream codegen / IR lowering relies on
    // this invariant rather than re-deriving types from emission output.
    for name in graph.order.clone() {
        let Some(mut ctx) = module_contexts.remove(&name) else {
            continue;
        };
        ctx.merge(&unified_project_ctx);
        let rm = graph.modules.get_mut(&name).unwrap();
        let pkg = fqn_to_package(&name);
        expo_typecheck::resolve_module_aliases(&rm.module, &mut ctx);
        expo_typecheck::resolve_packages(&mut ctx);
        expo_typecheck::check_module(&mut rm.module, &mut ctx, pkg);
        expo_typecheck::validate_resolved_types(&rm.module, &mut ctx);
        module_contexts.insert(name, ctx);
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
    graph: &mut ModuleGraph,
    output: &str,
    quiet: bool,
    color: bool,
    emit_llvm: bool,
    release: bool,
) {
    let (module_contexts, has_errors) = typecheck_graph(graph, color);
    if has_errors {
        process::exit(1);
    }

    let mut merged_ctx = TypeContext::new();
    for name in &graph.order {
        merged_ctx.merge(&module_contexts[name]);
    }
    expo_typecheck::resolve_packages(&mut merged_ctx);

    let modules_ast: Vec<&Module> = graph
        .order
        .iter()
        .map(|name| &graph.modules[name].module)
        .collect();
    let module_packages: Vec<String> = graph
        .order
        .iter()
        .map(|name| fqn_to_package(name).to_string())
        .collect();
    let module_packages_refs: Vec<&str> = module_packages.iter().map(String::as_str).collect();

    let app_name = graph
        .entry
        .split('.')
        .next()
        .unwrap_or(&graph.entry)
        .to_string();

    let entry_type = graph.entry_type.as_deref();

    if emit_llvm {
        match expo_codegen::emit_llvm_ir(
            &modules_ast,
            &module_packages_refs,
            &merged_ctx,
            &app_name,
            entry_type,
        ) {
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
    if let Err(diagnostics) = expo_codegen::compile_modules(
        &modules_ast,
        &module_packages_refs,
        &merged_ctx,
        Path::new(&obj_path),
        release,
        &app_name,
        entry_type,
    ) {
        let entry_rm = &graph.modules[&graph.entry];
        render_diagnostics(
            entry_rm.path.to_str().unwrap_or(&entry_rm.name),
            &entry_rm.source,
            &diagnostics,
            color,
        );
        process::exit(1);
    }

    let link_libs = collect_link_libraries(&modules_ast);
    link(&obj_path, output, quiet, release, &link_libs);
}

/// Walks all modules and collects unique `@link` library names from
/// function annotations across structs, enums, impl blocks, and top-level items.
fn collect_link_libraries(modules: &[&Module]) -> Vec<String> {
    fn collect_from(annotations: &[Annotation], libs: &mut BTreeSet<String>) {
        for ann in annotations {
            if ann.name == "link"
                && let Some(AnnotationValue::String(s)) = &ann.value
            {
                let lib = s.split_once(':').map_or(s.as_str(), |(l, _)| l);
                libs.insert(lib.to_string());
            }
        }
    }

    let mut libs = BTreeSet::new();
    for module in modules {
        for item in &module.items {
            match item {
                Item::Function(f) => collect_from(&f.annotations, &mut libs),
                Item::Struct(s) => {
                    for f in &s.functions {
                        collect_from(&f.annotations, &mut libs);
                    }
                }
                Item::Enum(e) => {
                    for f in &e.functions {
                        collect_from(&f.annotations, &mut libs);
                    }
                }
                Item::Impl(imp) => {
                    for member in &imp.members {
                        if let ImplMember::Function(f) = member {
                            collect_from(&f.annotations, &mut libs);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    libs.into_iter().collect()
}

/// Builds a project from its config: resolves modules, type-checks, compiles, links.
pub fn build_project(
    config: &ProjectConfig,
    project_root: &Path,
    output: Option<&str>,
    quiet: bool,
    color: bool,
    emit_llvm: bool,
    release: bool,
) {
    let mut graph = match resolve::resolve_project_modules(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let default_output;
    let output = match output {
        Some(o) => o,
        None => {
            let target_dir = if release {
                project_root.join("target").join("release")
            } else {
                project_root.join("target").join("debug")
            };
            fs::create_dir_all(&target_dir).unwrap_or_else(|e| {
                eprintln!("error: cannot create target directory: {e}");
                process::exit(1);
            });
            default_output = target_dir.join(&config.name);
            default_output.to_str().unwrap()
        }
    };
    build_from_graph(&mut graph, output, quiet, color, emit_llvm, release);
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
    build_from_graph(
        &mut graph,
        &output,
        quiet,
        color,
        args.emit_llvm,
        args.release,
    );
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
    let (_, has_errors) = typecheck_graph(&mut graph, color);
    has_errors
}

/// Type-checks a project module graph (without compiling).
pub fn check_project(config: &ProjectConfig, project_root: &Path, color: bool) -> bool {
    let mut graph = match resolve::resolve_project_modules(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    let (_, has_errors) = typecheck_graph(&mut graph, color);
    has_errors
}

/// Inserts stdlib modules at the front of an existing graph's order.
/// Used for single-file mode where the graph is built without stdlib.
fn prepend_stdlib(graph: &mut ModuleGraph) {
    let user_order = mem::take(&mut graph.order);
    resolve::insert_stdlib(graph);
    graph.order.extend(user_order);
}

/// A discovered `@test` function inside a struct, called as `StructName.fn_name()`.
struct TestCase {
    struct_name: String,
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
            path: PathBuf::from("<test_harness>"),
            source: harness_source,
            module: parse_result.module,
            errors: parse_result.errors,
        },
    );
    graph.entry = harness_name;

    let target_dir = project_root.join("target").join("debug");
    fs::create_dir_all(&target_dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create target directory: {e}");
        process::exit(1);
    });
    let binary = target_dir.join(format!("{}_test", config.name));
    let output = binary.to_str().unwrap().to_string();

    build_from_graph(&mut graph, &output, true, color, false, false);

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

/// Walks the module graph and collects `@test`-annotated functions inside structs.
/// Only scans modules belonging to the current project (by name prefix).
fn discover_tests(graph: &ModuleGraph, project_name: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();
    let prefix = format!("{project_name}.");

    for name in &graph.order {
        if name != project_name && !name.starts_with(&prefix) {
            continue;
        }

        let rm = &graph.modules[name];
        for item in &rm.module.items {
            if let Item::Struct(s) = item {
                for func in &s.functions {
                    if let Some(ann) = func.annotations.iter().find(|a| a.name == "test") {
                        let description = match &ann.value {
                            Some(AnnotationValue::String(s)) => s.clone(),
                            _ => func.name.clone(),
                        };
                        tests.push(TestCase {
                            struct_name: s.name.clone(),
                            fn_name: func.name.clone(),
                            description,
                        });
                    }
                }
            }
        }
    }

    tests
}

/// Generates the Expo source for a test harness module.
///
/// Each `@test` function must return `Result<Bool, String>`. The harness
/// calls each test as `StructName.fn_name()`, matches on the result to
/// track pass/fail counts, and continues running all tests even when some
/// fail. A final non-zero exit (via panic) is triggered when any test failed.
///
/// No imports are needed -- the gather-then-check pipeline makes all project
/// types visible to every module automatically.
fn generate_harness(tests: &[TestCase], _graph: &ModuleGraph) -> String {
    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let reset = "\x1b[0m";

    let mut body = String::new();
    body.push_str("  failures: List<String> = []\n");
    body.push_str("  passed = 0\n");
    body.push_str("  failed = 0\n");

    for test in tests {
        let escaped_desc = test.description.replace('\\', "\\\\").replace('"', "\\\"");
        body.push_str(&format!(
            "  match {}.{}()\n",
            test.struct_name, test.fn_name
        ));
        body.push_str("    Result.Ok(_) ->\n");
        body.push_str("      passed = passed + 1\n");
        body.push_str(&format!("      IO.write(\"{green}.{reset}\")\n"));
        body.push_str("    Result.Err(msg) ->\n");
        body.push_str("      failed = failed + 1\n");
        body.push_str(&format!("      IO.write(\"{red}X{reset}\")\n"));
        body.push_str(&format!(
            "      failures = failures.append(\"  #{{failed}}) {} ({})\\n     \" <> msg)\n",
            escaped_desc, test.struct_name
        ));
        body.push_str("  end\n");
    }

    body.push_str("  print(\"\")\n");
    body.push_str("  if failed > 0\n");
    body.push_str("    print(\"\")\n");
    body.push_str("    print(\"Failures:\")\n");
    body.push_str("    print(\"\")\n");
    body.push_str("    for f in failures\n");
    body.push_str("      print(f)\n");
    body.push_str("      print(\"\")\n");
    body.push_str("    end\n");
    body.push_str(&format!(
        "    print(\"{red}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
    ));
    body.push_str("    Kernel.exit(1)\n");
    body.push_str("  else\n");
    body.push_str(&format!(
        "    print(\"{green}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
    ));
    body.push_str("  end\n");

    let mut source = String::new();
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
    pub release: bool,
}

/// Embedded static libraries written to the temp link directory.
/// The runtime is always linked; others are available for `@link` resolution.
const EMBEDDED_RUNTIME: &[u8] = include_bytes!(env!("EXPO_RUNTIME_LIB_PATH"));
const EMBEDDED_CRYPTO: &[u8] = include_bytes!(env!("EXPO_CRYPTO_LIB_PATH"));
const EMBEDDED_SSL: &[u8] = include_bytes!(env!("EXPO_SSL_LIB_PATH"));

/// Returns the macOS product version (e.g. "26.4") for use as MACOSX_DEPLOYMENT_TARGET.
/// Cached so `sw_vers` is invoked at most once per process.
#[cfg(target_os = "macos")]
fn macos_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| {
                let s = s.trim();
                let parts: Vec<&str> = s.splitn(3, '.').collect();
                if parts.len() >= 2 {
                    format!("{}.{}", parts[0], parts[1])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| "11.0".to_string())
    })
}

/// Links an object file with the embedded runtime library to produce an executable.
/// `link_libraries` contains library names from `@link` annotations (passed as `-l` flags).
fn link(obj_path: &str, output: &str, quiet: bool, release: bool, link_libraries: &[String]) {
    #[cfg(not(target_os = "macos"))]
    let _ = release;

    let tmp_dir = env::temp_dir().join(format!("expo-link-{}", process::id()));
    fs::create_dir_all(&tmp_dir).expect("failed to create temp dir for linking");

    fs::write(tmp_dir.join("libexpo_runtime.a"), EMBEDDED_RUNTIME)
        .expect("failed to write embedded runtime library");
    fs::write(tmp_dir.join("libcrypto.a"), EMBEDDED_CRYPTO)
        .expect("failed to write embedded crypto library");
    fs::write(tmp_dir.join("libssl.a"), EMBEDDED_SSL)
        .expect("failed to write embedded ssl library");

    let tmp_dir_str = tmp_dir.to_string_lossy();

    let mut args = vec![
        obj_path.to_string(),
        "-lexpo_runtime".to_string(),
        "-L".to_string(),
        tmp_dir_str.to_string(),
        "-o".to_string(),
        output.to_string(),
    ];
    for lib in link_libraries {
        args.push(format!("-l{lib}"));
    }

    let mut cmd = process::Command::new("cc");
    cmd.args(&args);
    cmd.stderr(process::Stdio::piped());
    #[cfg(target_os = "macos")]
    {
        cmd.env("MACOSX_DEPLOYMENT_TARGET", macos_version());
    }
    let result = cmd.output();

    let cleanup = |tmp: &Path, obj: &str| {
        let _ = fs::remove_dir_all(tmp);
        let _ = fs::remove_file(obj);
    };

    match result {
        Ok(output_result) => {
            let stderr = String::from_utf8_lossy(&output_result.stderr);
            for line in stderr.lines() {
                if !line.contains("reexported library") {
                    eprintln!("{line}");
                }
            }

            if output_result.status.success() {
                #[cfg(target_os = "macos")]
                if !release {
                    let _ = process::Command::new("dsymutil")
                        .arg(output)
                        .stderr(process::Stdio::null())
                        .status();
                }
                cleanup(&tmp_dir, obj_path);
                if !quiet {
                    println!("compiled: {output}");
                }
            } else {
                eprintln!(
                    "linker failed with exit code: {}",
                    output_result.status.code().unwrap_or(-1)
                );
                cleanup(&tmp_dir, obj_path);
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("failed to run linker: {e}");
            cleanup(&tmp_dir, obj_path);
            process::exit(1);
        }
    }
}
