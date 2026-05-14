//! Compilation pipeline: type checking, context merging, code generation, and linking.
//!
//! This module contains the shared infrastructure that powers `build`, `run`,
//! and `check`. No CLI command functions live here -- those are in [`crate::commands`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::{env, fs, mem, process};

use expo_ast::ast::{Annotation, AnnotationValue, Diagnostic, File, ImplMember, Item};
use expo_parser::{ParseMode, ParsedProgram, SourceFile};
use expo_typecheck::{CheckedPackage, CheckedProgram, DiagnosticSink};

use crate::diagnostics::render_diagnostics;
use crate::project::ProjectConfig;
use crate::resolve::{self, SourceSet};

/// Parsed build arguments: what to build and where the output goes.
pub struct BuildArgs {
    pub output_name: Option<String>,
    pub source_file: Option<String>,
}

/// Knobs that apply to every build path: how loud, how aggressive, and what
/// to emit. Cheap to copy so callers can pass it by value.
#[derive(Clone, Copy)]
pub struct BuildOptions {
    pub color: bool,
    pub emit_llvm: bool,
    pub quiet: bool,
    pub release: bool,
}

/// Unwraps a `Result<T, String>` from one of the resolve entry points,
/// printing the error to stderr and exiting on failure.
fn resolve_or_exit<T>(result: Result<T, String>) -> T {
    result.unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    })
}

/// Renders a codegen failure (whose diagnostics are scoped to the
/// entry file) using the entry file's resolve-time source for context,
/// then exits non-zero. Reads from [`SourceSet::entry_source`] so
/// rendering survives `ParsedProgram` having been consumed by
/// `check_program`.
fn render_codegen_failure(sources: &SourceSet, diagnostics: &[Diagnostic], color: bool) -> ! {
    render_diagnostics(
        sources.entry.to_str().unwrap_or("<unknown>"),
        &sources.entry_source,
        diagnostics,
        color,
    );
    process::exit(1);
}

/// Driver-side [`DiagnosticSink`] that renders each batch through
/// [`render_diagnostics`] -- the same terminal renderer the rest of the
/// pipeline uses for codegen-failure / link-failure messages.
struct TerminalSink {
    color: bool,
}

impl DiagnosticSink for TerminalSink {
    fn emit(&mut self, path: &Path, source: &str, diagnostics: &[Diagnostic]) {
        render_diagnostics(
            path.to_str().unwrap_or("<unknown>"),
            source,
            diagnostics,
            self.color,
        );
    }
}

/// Runs the type-checking pipeline for every file in a parsed program.
/// Thin wrapper around [`expo_typecheck::check_program`]; the seven
/// sub-passes live there.
pub fn typecheck_sources(parsed: ParsedProgram, color: bool) -> CheckedProgram {
    let mut sink = TerminalSink { color };
    expo_typecheck::check_program(parsed, &mut sink)
}

/// Compiles a fully type-checked program into an executable.
///
/// Reads ASTs and the merged context off `checked` (codegen pulls each
/// file's package off `File.package` directly, no parallel slice
/// needed); reads entry metadata off `sources`. When `options.emit_llvm`
/// is true, prints LLVM IR to stdout instead of linking.
pub fn build_from_sources(
    sources: &SourceSet,
    checked: &CheckedProgram,
    output: &str,
    extra_lib_search_paths: &[&Path],
    options: BuildOptions,
) {
    let files_ast: Vec<&File> = checked
        .packages
        .iter()
        .flat_map(|pkg| pkg.ast.iter())
        .collect();

    let app_name = sources.entry_package.clone();
    let entry_type = sources.entry_type.as_deref();

    if options.emit_llvm {
        match expo_codegen::emit_llvm_ir(&files_ast, &checked.merged_ctx, &app_name, entry_type) {
            Ok(ir) => print!("{ir}"),
            Err(diagnostics) => render_codegen_failure(sources, &diagnostics, options.color),
        }
        return;
    }

    let obj_path = format!("{output}.o");
    // LLVM `write_to_file` and the linker both require the output's
    // parent directory to exist; create it up front so callers can
    // pass paths like `target/debug/binary` on a fresh checkout
    // without a manual `mkdir -p`.
    if let Some(parent) = Path::new(output).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = fs::create_dir_all(parent)
    {
        eprintln!(
            "error: failed to create output directory `{}`: {e}",
            parent.display(),
        );
        process::exit(1);
    }

    if let Err(diagnostics) = expo_codegen::compile_files(
        &files_ast,
        &checked.merged_ctx,
        Path::new(&obj_path),
        options.release,
        &app_name,
        entry_type,
    ) {
        render_codegen_failure(sources, &diagnostics, options.color);
    }

    let link_libs = collect_link_libraries(&files_ast);
    link(
        &obj_path,
        output,
        &link_libs,
        extra_lib_search_paths,
        options,
    );
}

/// Walks all files and collects unique `@link` library names from function
/// annotations across structs, enums, impl blocks, and top-level items.
fn collect_link_libraries(files: &[&File]) -> Vec<String> {
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
    options: BuildOptions,
) {
    let (sources, source_files) =
        resolve_or_exit(resolve::resolve_project_sources(config, project_root));
    let parsed = expo_parser::parse_program(source_files, ParseMode::File);
    let checked = typecheck_sources(parsed, options.color);
    if checked.has_errors {
        process::exit(1);
    }

    let default_output;
    let output = match output {
        Some(o) => o,
        None => {
            let target_dir = if options.release {
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
    build_from_sources(&sources, &checked, output, &[project_root], options);
}

/// Full single-file build pipeline: resolve sources from an entry file,
/// type-check, merge contexts, codegen, and link into an executable.
///
/// When `options.quiet` is true, the "compiled: <output>" message is suppressed
/// (used by `expo run` to avoid noise).
pub fn build(args: BuildArgs, options: BuildOptions) {
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

    let (sources, mut source_files) = resolve_or_exit(resolve::resolve_sources(&entry_path));
    prepend_stdlib(&mut source_files);
    let parsed = expo_parser::parse_program(source_files, ParseMode::File);
    let checked = typecheck_sources(parsed, options.color);
    if checked.has_errors {
        process::exit(1);
    }
    build_from_sources(&sources, &checked, &output, &[], options);
}

/// Type-checks a single-file source set (without compiling).
///
/// When `emit_ast` is true, dumps every file's post-typecheck AST to stdout
/// after diagnostics run. Errors still gate the returned `has_errors` bool;
/// the dump happens either way (callers gate the OK-line on `!emit_ast`).
pub fn check_single_file(entry_path: &Path, color: bool, emit_ast: bool) -> bool {
    let (_sources, mut source_files) = match resolve::resolve_sources(entry_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    prepend_stdlib(&mut source_files);
    let parsed = expo_parser::parse_program(source_files, ParseMode::File);
    let checked = typecheck_sources(parsed, color);
    if emit_ast {
        emit_checked_ast(&checked.packages);
    }
    checked.has_errors
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
    let (_sources, source_files) = match resolve::resolve_project_sources(config, project_root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return true;
        }
    };

    let parsed = expo_parser::parse_program(source_files, ParseMode::File);
    let checked = typecheck_sources(parsed, color);
    if emit_ast {
        emit_checked_ast(&checked.packages);
    }
    checked.has_errors
}

/// Prints every file in the checked program to stdout using
/// [`expo_ast::format_file`], the compact 2-space-indent tree format
/// used by `expo check --emit-ast` and `expo parse --emit-ast`. Each
/// file's `File` header line carries the package and path, so no
/// separate `// === <path> ===` banner is needed. A blank line
/// separates successive files when more than one is emitted. Iterates
/// package-by-package, files in per-package order, matching the
/// original `ParsedProgram.order` walk.
fn emit_checked_ast(packages: &[CheckedPackage]) {
    let mut first = true;
    for file in packages.iter().flat_map(|pkg| pkg.ast.iter()) {
        if !first {
            println!();
        }
        first = false;
        print!("{}", expo_ast::format_file(file));
    }
}

/// Prepends stdlib source files to a `Vec<SourceFile>`. Used for
/// single-file mode where the resolver builds the file vec without
/// stdlib.
fn prepend_stdlib(source_files: &mut Vec<SourceFile>) {
    let user_files = mem::take(source_files);
    resolve::insert_stdlib(source_files, None);
    source_files.extend(user_files);
}

/// A discovered `@test` function inside a struct, called as `StructName.fn_name()`.
struct TestCase {
    description: String,
    fn_name: String,
    struct_name: String,
}

/// Discovers `@test` functions, generates a test harness, compiles and runs it.
pub fn test_project(config: &ProjectConfig, project_root: &Path, color: bool) {
    let (mut sources, source_files) =
        resolve_or_exit(resolve::resolve_test_project_sources(config, project_root));
    let mut parsed = expo_parser::parse_program(source_files, ParseMode::File);

    let tests = discover_tests(&parsed, &config.name);

    if tests.is_empty() {
        println!("no tests found");
        return;
    }

    let harness_source = generate_harness(&tests);
    let harness_path = PathBuf::from(format!("<{}.__test_harness__>", config.name));
    let harness_parsed = expo_parser::parse_file(
        SourceFile {
            package: config.name.clone(),
            path: harness_path.clone(),
            source: harness_source.clone(),
        },
        ParseMode::File,
    );
    if !harness_parsed.diagnostics.is_empty() {
        eprintln!("internal error: generated test harness failed to parse");
        for d in &harness_parsed.diagnostics {
            eprintln!("  {}", d.message);
        }
        process::exit(1);
    }

    parsed.order.push(harness_path.clone());
    parsed.files.insert(harness_path.clone(), harness_parsed);
    sources.entry = harness_path;
    sources.entry_package = config.name.clone();
    sources.entry_source = harness_source;

    let target_dir = project_root.join("target").join("debug");
    fs::create_dir_all(&target_dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create target directory: {e}");
        process::exit(1);
    });
    let binary = target_dir.join(format!("{}_test", config.name));
    let output = binary.to_str().unwrap().to_string();

    let options = BuildOptions {
        color,
        emit_llvm: false,
        quiet: true,
        release: false,
    };
    let checked = typecheck_sources(parsed, options.color);
    if checked.has_errors {
        process::exit(1);
    }
    build_from_sources(&sources, &checked, &output, &[project_root], options);

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

/// Walks the parsed program and collects `@test`-annotated functions inside
/// structs. Only scans files belonging to the current project (matched by
/// the per-file `package` field).
fn discover_tests(parsed: &ParsedProgram, project_name: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();

    for file in parsed.iter() {
        if file.package != project_name {
            continue;
        }

        for item in &file.ast.items {
            if let Item::Struct(s) = item {
                for func in &s.functions {
                    if let Some(ann) = func.annotations.iter().find(|a| a.name == "test") {
                        let description = match &ann.value {
                            Some(AnnotationValue::String(s)) => s.clone(),
                            _ => func.name.clone(),
                        };
                        tests.push(TestCase {
                            description,
                            fn_name: func.name.clone(),
                            struct_name: s.name.clone(),
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
fn generate_harness(tests: &[TestCase]) -> String {
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
/// `extra_lib_search_paths` lets callers add directories the linker
/// should scan for `-l<name>` resolution (passed as `-L<dir>`).
/// Project-mode callers thread the directory holding `expo.toml`
/// through so a sibling `libfoo.a` is discoverable without the
/// user manually setting `LIBRARY_PATH` or running from a specific
/// `cwd`. The embedded-archive temp dir is always added on top of
/// these so the runtime / crypto archives stay resolvable.
///
/// Exposed at `pub(crate)` so `crate::alpha`'s `cmd_alpha_build` can
/// reuse the same `cc` invocation + embedded runtime + BoringSSL
/// archives that v1 already wires up. Keeping it crate-private keeps
/// the function out of the public driver API; alpha and v1 link side
/// by side rather than diverging.
pub(crate) fn link(
    obj_path: &str,
    output: &str,
    link_libraries: &[String],
    extra_lib_search_paths: &[&Path],
    options: BuildOptions,
) {
    #[cfg(not(target_os = "macos"))]
    let _ = options.release;

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
    for path in extra_lib_search_paths {
        args.push("-L".to_string());
        args.push(path.to_string_lossy().to_string());
    }
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

    let cleanup = |tmp: &Path, obj: &str| {
        let _ = fs::remove_dir_all(tmp);
        let _ = fs::remove_file(obj);
    };

    let link_output = cmd.output().unwrap_or_else(|e| {
        eprintln!("failed to run linker: {e}");
        cleanup(&tmp_dir, obj_path);
        process::exit(1);
    });

    let stderr = String::from_utf8_lossy(&link_output.stderr);
    for line in stderr.lines() {
        if !line.contains("reexported library") {
            eprintln!("{line}");
        }
    }

    if !link_output.status.success() {
        eprintln!(
            "linker failed with exit code: {}",
            link_output.status.code().unwrap_or(-1)
        );
        cleanup(&tmp_dir, obj_path);
        process::exit(1);
    }

    #[cfg(target_os = "macos")]
    if !options.release {
        let _ = process::Command::new("dsymutil")
            .arg(output)
            .stderr(process::Stdio::null())
            .status();
    }
    cleanup(&tmp_dir, obj_path);
    if !options.quiet {
        println!("compiled: {output}");
    }
}
