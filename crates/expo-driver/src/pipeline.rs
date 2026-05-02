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
use crate::resolve::{self, SourceSet};

/// Builds the set of [`Package`]s visible to the resolver from a source
/// set's FQNs. `std.*` files collapse to a single [`Package::Std`]; every
/// other file's leading segment becomes a [`Package::Named`] so the type
/// resolver can validate qualified `pkg.Type` paths during signature
/// collection without waiting for the full type tables to be merged.
fn packages_from_file_names<I, S>(names: I) -> BTreeSet<Package>
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

/// Runs the type-checking pipeline for every file in a source set.
///
/// Stdlib files are processed sequentially (they have a defined dependency
/// order). Project files use a gather-unify-check pipeline:
///   1. **Gather** – collect type signatures from every project file.
///   2. **Unify** – merge all project contexts into a single shared context
///      so every file sees every other file's types without imports.
///   3. **Check** – type-check each project file against the unified context.
///
/// Returns the per-file type contexts and whether any errors were found.
pub fn typecheck_sources(
    sources: &mut SourceSet,
    color: bool,
) -> (BTreeMap<String, expo_typecheck::context::TypeContext>, bool) {
    let mut file_contexts: BTreeMap<String, expo_typecheck::context::TypeContext> = BTreeMap::new();
    let mut has_errors = false;

    for name in &sources.order {
        let rm = &sources.files[name];
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
        return (file_contexts, true);
    }

    let all_files: Vec<&Module> = sources
        .order
        .iter()
        .map(|n| &sources.files[n].ast)
        .collect();
    let known_packages = packages_from_file_names(&sources.order);
    let global_names = expo_typecheck::collect_all_names(&all_files, known_packages);

    let mut stdlib_ctx = expo_typecheck::context::TypeContext::new();

    let is_stdlib = |n: &str| n.starts_with("std.");

    let (stdlib_names, project_names): (Vec<&String>, Vec<&String>) =
        sources.order.iter().partition(|n| is_stdlib(n));

    // Auto-imported std files: merge into stdlib_ctx directly.
    // `collect_file` now runs the synthesize sub-pass internally
    // (auto-derives `impl Debug for T`), so the AST gains synthesized
    // items as a side effect.
    for name in &stdlib_names {
        let rm = sources.files.get_mut(*name).expect("file present");
        let mut ctx = expo_typecheck::collect_file(&mut rm.ast, &global_names, "std");
        ctx.merge(&stdlib_ctx);

        stdlib_ctx.merge(&ctx);
        file_contexts.insert((*name).clone(), ctx);
    }

    // Gather: collect signatures from every project file.
    for name in &project_names {
        let rm = sources.files.get_mut(*name).expect("file present");
        let pkg = fqn_to_package(name);
        let mut ctx = expo_typecheck::collect_file(&mut rm.ast, &global_names, pkg);
        ctx.merge(&stdlib_ctx);
        // Other stdlib protocols (today: `Process` with `run` /
        // `handle_signal`) still rely on default-method synthesis for
        // user impls. `Debug` is auto-derived by the synthesize
        // sub-pass inside `collect_file` and never touches this
        // codepath.
        expo_typecheck::synthesize_protocol_defaults(&rm.ast, &mut ctx, pkg);
        expo_typecheck::mark_recursive_fields(&mut ctx);
        file_contexts.insert((*name).clone(), ctx);
    }

    // Unify: build a shared context containing all project definitions.
    let mut unified_project_ctx = stdlib_ctx.clone();
    for name in &project_names {
        unified_project_ctx.merge(&file_contexts[*name]);
    }

    // Check: every file needs alias resolution, package resolution, and
    // body typechecking. Embedded stdlib files (synthetic `<std.x>` paths)
    // are checked alongside workspace sources so every expression carries a
    // populated `resolved_type` -- downstream codegen / IR lowering relies on
    // this invariant rather than re-deriving types from emission output.
    for name in sources.order.clone() {
        let Some(mut ctx) = file_contexts.remove(&name) else {
            continue;
        };
        ctx.merge(&unified_project_ctx);
        let rm = sources.files.get_mut(&name).unwrap();
        let pkg = fqn_to_package(&name);
        expo_typecheck::resolve_file_aliases(&rm.ast, &mut ctx);
        expo_typecheck::resolve_packages(&mut ctx);
        expo_typecheck::check_file(&mut rm.ast, &mut ctx, pkg);
        expo_typecheck::validate_resolved_types(&rm.ast, &mut ctx);
        file_contexts.insert(name, ctx);
    }

    for name in &sources.order {
        let rm = &sources.files[name];
        let ctx = &file_contexts[name];
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

    (file_contexts, has_errors)
}

/// Compiles a fully resolved source set into an executable.
///
/// Type-checks all files, merges contexts, emits LLVM IR, and links.
/// The source set must include stdlib files (via [`resolve::insert_stdlib`]).
/// When `emit_llvm` is true, prints LLVM IR to stdout instead of linking.
pub fn build_from_sources(
    sources: &mut SourceSet,
    output: &str,
    quiet: bool,
    color: bool,
    emit_llvm: bool,
    release: bool,
) {
    let (file_contexts, has_errors) = typecheck_sources(sources, color);
    if has_errors {
        process::exit(1);
    }

    let mut merged_ctx = TypeContext::new();
    for name in &sources.order {
        merged_ctx.merge(&file_contexts[name]);
    }
    expo_typecheck::resolve_packages(&mut merged_ctx);

    let files_ast: Vec<&Module> = sources
        .order
        .iter()
        .map(|name| &sources.files[name].ast)
        .collect();
    let file_packages: Vec<String> = sources
        .order
        .iter()
        .map(|name| fqn_to_package(name).to_string())
        .collect();
    let file_packages_refs: Vec<&str> = file_packages.iter().map(String::as_str).collect();

    let app_name = sources
        .entry
        .split('.')
        .next()
        .unwrap_or(&sources.entry)
        .to_string();

    let entry_type = sources.entry_type.as_deref();

    if emit_llvm {
        match expo_codegen::emit_llvm_ir(
            &files_ast,
            &file_packages_refs,
            &merged_ctx,
            &app_name,
            entry_type,
        ) {
            Ok(ir) => print!("{ir}"),
            Err(diagnostics) => {
                let entry_rm = &sources.files[&sources.entry];
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
        &files_ast,
        &file_packages_refs,
        &merged_ctx,
        Path::new(&obj_path),
        release,
        &app_name,
        entry_type,
    ) {
        let entry_rm = &sources.files[&sources.entry];
        render_diagnostics(
            entry_rm.path.to_str().unwrap_or(&entry_rm.name),
            &entry_rm.source,
            &diagnostics,
            color,
        );
        process::exit(1);
    }

    let link_libs = collect_link_libraries(&files_ast);
    link(&obj_path, output, quiet, release, &link_libs);
}

/// Walks all files and collects unique `@link` library names from function
/// annotations across structs, enums, impl blocks, and top-level items.
fn collect_link_libraries(files: &[&Module]) -> Vec<String> {
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
    for file in files {
        for item in &file.items {
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

/// Builds a project from its config: resolves files, type-checks, compiles, links.
pub fn build_project(
    config: &ProjectConfig,
    project_root: &Path,
    output: Option<&str>,
    quiet: bool,
    color: bool,
    emit_llvm: bool,
    release: bool,
) {
    let mut sources = match resolve::resolve_project_sources(config, project_root) {
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
    build_from_sources(&mut sources, output, quiet, color, emit_llvm, release);
}

/// Full single-file build pipeline: resolve sources from an entry file,
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

    let mut sources = match resolve::resolve_sources(&entry_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    prepend_stdlib(&mut sources);
    build_from_sources(
        &mut sources,
        &output,
        quiet,
        color,
        args.emit_llvm,
        args.release,
    );
}

/// Type-checks a single-file source set (without compiling).
///
/// When `emit_ast` is true, dumps every file's post-typecheck AST to stdout
/// after diagnostics run. Errors still gate the returned `has_errors` bool;
/// the dump happens either way (callers gate the OK-line on `!emit_ast`).
pub fn check_single_file(entry_path: &Path, color: bool, emit_ast: bool) -> bool {
    let mut sources = match resolve::resolve_sources(entry_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    prepend_stdlib(&mut sources);
    let (_, has_errors) = typecheck_sources(&mut sources, color);
    if emit_ast {
        emit_sources_ast(&sources);
    }
    has_errors
}

/// Type-checks a project source set (without compiling).
///
/// See [`check_single_file`] for `emit_ast` semantics.
pub fn check_project(
    config: &ProjectConfig,
    project_root: &Path,
    color: bool,
    emit_ast: bool,
) -> bool {
    let mut sources = match resolve::resolve_project_sources(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    let (_, has_errors) = typecheck_sources(&mut sources, color);
    if emit_ast {
        emit_sources_ast(&sources);
    }
    has_errors
}

/// Prints every file in `sources.order` to stdout as a pretty-Debug dump,
/// preceded by a `// === <name> ===` header. Used by `expo check --emit-ast`.
/// Pretty-Debug is intentional for now; a proper S-expression printer is a
/// separate slice (see `design/COMPILER-NORTHSTAR.md` "Per-phase debug emitters").
fn emit_sources_ast(sources: &SourceSet) {
    for name in &sources.order {
        let rm = &sources.files[name];
        println!("// === {name} ===");
        println!("{:#?}", rm.ast);
    }
}

/// Inserts stdlib files at the front of an existing source set's order.
/// Used for single-file mode where the source set is built without stdlib.
fn prepend_stdlib(sources: &mut SourceSet) {
    let user_order = mem::take(&mut sources.order);
    resolve::insert_stdlib(sources);
    sources.order.extend(user_order);
}

/// A discovered `@test` function inside a struct, called as `StructName.fn_name()`.
struct TestCase {
    struct_name: String,
    fn_name: String,
    description: String,
}

/// Discovers `@test` functions, generates a test harness, compiles and runs it.
pub fn test_project(config: &ProjectConfig, project_root: &Path, color: bool) {
    let mut sources = match resolve::resolve_test_project_sources(config, project_root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let tests = discover_tests(&sources, &config.name);

    if tests.is_empty() {
        println!("no tests found");
        return;
    }

    let harness_source = generate_harness(&tests, &sources);
    let harness_name = format!("{}.__test_harness__", config.name);

    let parse_result = expo_parser::parse(&harness_source);
    if !parse_result.errors.is_empty() {
        eprintln!("internal error: generated test harness failed to parse");
        for d in &parse_result.errors {
            eprintln!("  {}", d.message);
        }
        process::exit(1);
    }

    sources.order.push(harness_name.clone());
    sources.files.insert(
        harness_name.clone(),
        resolve::ResolvedFile {
            name: harness_name.clone(),
            path: PathBuf::from("<test_harness>"),
            source: harness_source,
            ast: parse_result.module,
            errors: parse_result.errors,
        },
    );
    sources.entry = harness_name;

    let target_dir = project_root.join("target").join("debug");
    fs::create_dir_all(&target_dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create target directory: {e}");
        process::exit(1);
    });
    let binary = target_dir.join(format!("{}_test", config.name));
    let output = binary.to_str().unwrap().to_string();

    build_from_sources(&mut sources, &output, true, color, false, false);

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

/// Walks the source set and collects `@test`-annotated functions inside
/// structs. Only scans files belonging to the current project (by name prefix).
fn discover_tests(sources: &SourceSet, project_name: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();
    let prefix = format!("{project_name}.");

    for name in &sources.order {
        if name != project_name && !name.starts_with(&prefix) {
            continue;
        }

        let rm = &sources.files[name];
        for item in &rm.ast.items {
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

/// Generates the Expo source for a test harness file.
///
/// Each `@test` function must return `Result<Bool, String>`. The harness
/// calls each test as `StructName.fn_name()`, matches on the result to
/// track pass/fail counts, and continues running all tests even when some
/// fail. A final non-zero exit (via panic) is triggered when any test failed.
///
/// No imports are needed -- the gather-then-check pipeline makes all project
/// types visible to every file automatically.
fn generate_harness(tests: &[TestCase], _sources: &SourceSet) -> String {
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

    body.push_str("  IO.puts(\"\")\n");
    body.push_str("  if failed > 0\n");
    body.push_str("    IO.puts(\"\")\n");
    body.push_str("    IO.puts(\"Failures:\")\n");
    body.push_str("    IO.puts(\"\")\n");
    body.push_str("    for f in failures\n");
    body.push_str("      IO.puts(f)\n");
    body.push_str("      IO.puts(\"\")\n");
    body.push_str("    end\n");
    body.push_str(&format!(
        "    IO.puts(\"{red}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
    ));
    body.push_str("    Kernel.exit(1)\n");
    body.push_str("  else\n");
    body.push_str(&format!(
        "    IO.puts(\"{green}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
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
    // Modern Debian/Ubuntu default `cc` to PIE, which rejects the
    // absolute (`R_X86_64_32`) relocations LLVM emits under
    // `RelocMode::Default`. Until codegen is switched to
    // `RelocMode::PIC`, ask the linker for a non-PIE binary on Linux.
    #[cfg(target_os = "linux")]
    args.push("-no-pie".to_string());
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
