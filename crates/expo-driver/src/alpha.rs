//! `expo alpha {check,eval,shell,build,run}` subcommand handlers.
//!
//! The `alpha` namespace hosts experimental subcommands that drive the
//! alpha compiler pipeline (`expo-alpha-typecheck → expo-alpha-ir →
//! expo-alpha-ir-eval` / `expo-alpha-ir-llvm`). Production users keep
//! using `expo check` / `expo eval` / `expo shell` (the v1 path);
//! `expo alpha *` lets us iterate on the alpha track end-to-end
//! without touching the v1 surface.
//!
//! Each command carries its own copy of the pipeline driver since
//! they run a single source file and have no REPL state to thread.
//! The REPL itself lives in [`expo_alpha_shell`]; `cmd_shell` is just
//! a thin entry point that hands control off to it. When the alpha
//! shell grows file-input support all five handlers will collapse
//! into `expo_alpha_shell` and this module will retire alongside the
//! v1 `expo-shell` / `expo-ir-eval` crates.
//!
//! ## Mode dispatch
//!
//! `cmd_build` is project-mode (parse `ParseMode::File`, lower via
//! [`lower_program`], compile via
//! [`expo_alpha_ir_llvm::compile_program`]); the source must contain
//! an explicit `fn main`. `cmd_eval`, `cmd_run`, and (transitively)
//! `cmd_shell` are script-mode (parse `ParseMode::Script`, lower via
//! [`lower_script`], drive via
//! [`Interpreter::run_script`] or
//! [`expo_alpha_ir_llvm::compile_script`]). `cmd_check` typechecks
//! either shape — its parse mode follows the v1 contract for now.
//!
//! POC scope today (mirrors `expo-alpha-typecheck` / `expo-alpha-ir`):
//! integer literals, integer arithmetic (`+ - * / %`), parenthesized
//! groups, and the boolean/comparison/unary operators. Anything
//! richer typecheck-errors with a precise diagnostic.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use expo_alpha_ir::{IRProgram, IRScript, lower_program, lower_script};
use expo_alpha_ir_eval::{Interpreter, Value};
use expo_alpha_typecheck::{CheckFailure, CheckedProgram, check_program, format_registry};
use expo_ast::ast::Diagnostic;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};

use crate::pipeline::{self, BuildOptions};

/// `expo alpha check <file>` — parse and typecheck a single source
/// file through the alpha pipeline, without lowering or running it.
///
/// Mirrors `expo check`'s contract: prints `<file>: OK` on success,
/// or the collected parse/type diagnostics on failure (exit 1). When
/// `emit_ast` is set, prints the sealed, resolved AST in
/// [`expo_ast::format_file`]'s compact tree format instead of the OK
/// line. The alpha pipeline is single-file today; a project-aware
/// variant will come with `expo-alpha-shell` file-input support.
pub fn cmd_check(file: String, emit_ast: bool) {
    let path = Path::new(&file);
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: cannot read `{}`: {err}", path.display());
            process::exit(1);
        }
    };
    let package = derive_package(path);
    match run_check(source, &package, path.to_path_buf()) {
        Ok(checked) => {
            if emit_ast {
                emit_checked_ast(&checked);
            } else {
                println!("{}: OK", path.display());
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// `expo alpha eval <file>` — run a single source file through the
/// script-mode alpha pipeline and print the trailing expression's
/// [`Value`].
///
/// `expo alpha eval` always treats its input as a script: top-level
/// expressions and assignments are first-class, and an explicit
/// `fn main` is a helper definition rather than the entry point. The
/// pipeline lowers via [`lower_script`] and runs through
/// [`Interpreter::run_script`].
///
/// Mirrors `expo eval`'s contract for the print rule: `Value::Unit`
/// suppresses the trailing line so void scripts don't print `()` (the
/// driver still exits 0). Any pipeline failure (filesystem, parse,
/// typecheck, lower, runtime) prints `error: <details>` to stderr
/// and exits 1. The `entry` parameter is accepted for CLI parity
/// with v1's `expo eval` but is ignored — script mode has no named
/// entry point.
pub fn cmd_eval(file: String, _entry: Option<String>) {
    let path = Path::new(&file);
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: cannot read `{}`: {err}", path.display());
            process::exit(1);
        }
    };
    let package = derive_package(path);
    match run_script_pipeline(source, &package, path.to_path_buf()) {
        Ok(Value::Unit) => {}
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// `expo alpha shell` — interactive REPL on top of the alpha pipeline.
///
/// Delegates entirely to [`expo_alpha_shell::run`]; the REPL crate
/// owns Session state, multiline detection, command parsing, and its
/// own pipeline driver.
pub fn cmd_shell() {
    expo_alpha_shell::run();
}

/// `expo alpha build <file>` — compile a single source file through
/// the alpha pipeline (`parse → check → lower → compile_program →
/// link`) into a native binary.
///
/// Slice scope mirrors the rest of the alpha pipeline: a single
/// `fn main -> Int` whose body returns an `Int` arithmetic
/// expression. The output binary's exit code is the i64 return value
/// truncated to 8 bits — matches the OS exit-code contract. The
/// runtime + BoringSSL static archives are linked in (link-time
/// parity with v1) but not called yet; see
/// [`expo_alpha_ir_llvm`]'s crate docstring for the entry-point
/// wrapper deferral.
pub fn cmd_build(file: String, output: Option<String>) {
    let path = canonical_source_path(&file);
    let output = resolve_output_name(output, &path);
    let program = build_program(&path);
    emit_and_link(&program, &output);
    println!("compiled: {output}");
}

/// `expo alpha run <file>` — build the source file as a script
/// (top-level expressions are first-class), execute the resulting
/// binary, and forward its exit code. The binary is written to a
/// temp path and removed after exec, so this leaves no artifacts
/// behind in the working directory.
///
/// Pipeline: parse (script mode) → check → [`lower_script`] →
/// [`expo_alpha_ir_llvm::compile_script`] → link.
pub fn cmd_run(file: String, args: Vec<String>) {
    let path = canonical_source_path(&file);
    let script = build_script(&path);

    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("alpha_program");
    let output = std::env::temp_dir()
        .join(format!("expo-alpha-run-{}-{stem}", process::id()))
        .to_string_lossy()
        .to_string();

    emit_and_link_script(&script, &output);

    let status = process::Command::new(&output).args(&args).status();
    let _ = fs::remove_file(&output);

    match status {
        Ok(s) => process::exit(s.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("error: failed to exec `{output}`: {err}");
            process::exit(1);
        }
    }
}

/// Read a source file and drive it through the project-mode alpha
/// pipeline (`parse → check → lower_program`). Returns the sealed
/// [`IRProgram`] on success; bails the process on any pipeline
/// failure with an `error: <details>` line matching `cmd_eval`'s
/// contract. `cmd_build` uses this; the source must contain an
/// explicit `fn main`.
fn build_program(path: &Path) -> IRProgram {
    let (checked, package) = read_and_check(path, ParseMode::File);
    let entry = Identifier::new(&package, vec!["main".to_string()]);
    match lower_program(&checked, entry) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Read a source file and drive it through the script-mode alpha
/// pipeline (`parse → check → lower_script`). Returns the sealed
/// [`IRScript`] on success; bails the process on any pipeline
/// failure. `cmd_run` uses this — script-mode treats top-level
/// expressions as first-class.
fn build_script(path: &Path) -> IRScript {
    let (checked, _package) = read_and_check(path, ParseMode::Script);
    match lower_script(&checked) {
        Ok(script) => script,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Shared parse + check helper for the build / run paths. Returns
/// the sealed [`CheckedProgram`] and the derived package name.
/// Bails the process with a formatted error on read / parse /
/// typecheck failures.
fn read_and_check(path: &Path, mode: ParseMode) -> (CheckedProgram, String) {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: cannot read `{}`: {err}", path.display());
            process::exit(1);
        }
    };
    let package = derive_package(path);
    let parsed = parse_program(
        vec![SourceFile {
            package: package.clone(),
            path: path.to_path_buf(),
            source,
        }],
        mode,
    );
    let checked = match check_program(parsed) {
        Ok(checked) => checked,
        Err(failure) => {
            eprintln!("error: {}", format_check_failure(failure));
            process::exit(1);
        }
    };
    (checked, package)
}

/// Compile the [`IRProgram`] to an object file and link it into a
/// native binary at `output`, reusing v1's
/// [`pipeline::link`](crate::pipeline) helper for `cc` invocation,
/// runtime archive embedding, and BoringSSL linkage. Bails the
/// process on any LLVM emission failure.
fn emit_and_link(program: &IRProgram, output: &str) {
    let object_path = format!("{output}.o");
    if let Err(err) = expo_alpha_ir_llvm::compile_program(program, Path::new(&object_path)) {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(&object_path, output);
}

/// Script-mode counterpart of [`emit_and_link`]: drives
/// [`expo_alpha_ir_llvm::compile_script`] and shares the same
/// link path.
fn emit_and_link_script(script: &IRScript, output: &str) {
    let object_path = format!("{output}.o");
    if let Err(err) = expo_alpha_ir_llvm::compile_script(script, Path::new(&object_path)) {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(&object_path, output);
}

fn link_object(object_path: &str, output: &str) {
    let options = BuildOptions {
        color: false,
        emit_llvm: false,
        quiet: true,
        release: false,
    };
    pipeline::link(object_path, output, &[], options);
}

/// Canonicalize a user-supplied source path, exiting on miss with a
/// matching error message to v1's `expo build`.
fn canonical_source_path(file: &str) -> PathBuf {
    Path::new(file).canonicalize().unwrap_or_else(|_| {
        eprintln!("error: file not found: {file}");
        process::exit(1);
    })
}

/// Pick the output binary name. Honors a user-supplied `--output`,
/// otherwise mirrors v1: drop the source extension to derive the
/// binary name, falling back to `output` if there's no usable stem.
fn resolve_output_name(output: Option<String>, path: &Path) -> String {
    output.unwrap_or_else(|| {
        path.file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("output")
            .to_string()
    })
}

/// Run one source file end-to-end through the script-mode alpha
/// pipeline. Returns the script body's trailing value on success,
/// or a formatted error string covering parse / typecheck / lower /
/// runtime failures.
///
/// Parses in [`ParseMode::Script`]; lowers via [`lower_script`] and
/// drives through [`Interpreter::run_script`]. Used by `cmd_eval`.
fn run_script_pipeline(source: String, package: &str, path: PathBuf) -> Result<Value, String> {
    let parsed = parse_program(
        vec![SourceFile {
            package: package.to_string(),
            path,
            source,
        }],
        ParseMode::Script,
    );
    let checked = check_program(parsed).map_err(format_check_failure)?;
    let script = lower_script(&checked).map_err(|err| err.to_string())?;
    Interpreter::run_script(script).map_err(|err| err.to_string())
}

/// Parse + typecheck one source file. Returns the sealed
/// [`CheckedProgram`] on success, or a formatted error string on
/// parse/typecheck failure. Shares parse mode and package derivation
/// with [`run_script_pipeline`] so `expo alpha check` and `expo
/// alpha eval` see the same frontend shape.
fn run_check(source: String, package: &str, path: PathBuf) -> Result<CheckedProgram, String> {
    let parsed = parse_program(
        vec![SourceFile {
            package: package.to_string(),
            path,
            source,
        }],
        ParseMode::Script,
    );
    check_program(parsed).map_err(format_check_failure)
}

/// Prints every file in the sealed program to stdout using
/// [`expo_ast::format_file`], followed by the compact registry
/// sidecar from [`expo_alpha_typecheck::format_registry`] so the ids
/// that appear on AST reference sites are decodable without a
/// separate lookup. Mirrors what `expo check --emit-ast` does for the
/// v1 pipeline on the AST side; the registry sidecar is alpha-only.
///
/// A blank line separates the AST section(s) from the registry
/// section, and successive files from each other.
fn emit_checked_ast(checked: &CheckedProgram) {
    if !checked.registry.is_empty() {
        println!();
        println!("{}", format_registry(&checked.registry));
    }
    let mut first = true;
    for file in checked.packages.iter().flat_map(|pkg| pkg.files.iter()) {
        if !first {
            println!();
        }
        first = false;
        print!("{}", expo_ast::format_file(file));
    }
}

/// Derive the package name from the source file's stem. Falls back to
/// `App` when the path has no usable stem; user-facing files always
/// have a stem in practice.
fn derive_package(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("App")
        .to_string()
}

/// Render a [`CheckFailure`] as the multi-line error string the
/// driver prints. Sources diagnostics from both the typecheck pass
/// itself and the partial parse output (parse errors live there, not
/// on `failure.diagnostics`).
fn format_check_failure(failure: CheckFailure) -> String {
    let CheckFailure {
        diagnostics,
        partial,
    } = failure;
    let parse_diags = parse_diagnostics(&partial);
    let parse_block = (!parse_diags.is_empty()).then(|| format_block("parse error", &parse_diags));
    let type_block = (!diagnostics.is_empty()).then(|| {
        format_block(
            "type error",
            diagnostics.iter().collect::<Vec<_>>().as_slice(),
        )
    });
    match (parse_block, type_block) {
        (Some(parse), Some(types)) => format!("{parse}\n{types}"),
        (Some(parse), None) => parse,
        (None, Some(types)) => types,
        (None, None) => "check failed with no diagnostics".to_string(),
    }
}

fn parse_diagnostics(parsed: &ParsedProgram) -> Vec<&Diagnostic> {
    parsed
        .files
        .values()
        .flat_map(|file| file.diagnostics.iter())
        .collect()
}

fn format_block(prefix: &str, diagnostics: &[&Diagnostic]) -> String {
    let mut out = format!("{prefix}:");
    for diag in diagnostics {
        out.push_str("\n  ");
        out.push_str(&diag.message);
    }
    out
}
