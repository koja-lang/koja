//! `expo alpha {check,shell,build,run}` subcommand handlers.
//!
//! The `alpha` namespace hosts experimental subcommands that drive
//! the alpha compiler pipeline (`expo-alpha-typecheck →
//! expo-alpha-ir → expo-alpha-ir-eval` / `expo-alpha-ir-llvm`).
//! Production users keep using `expo check` / `expo eval` /
//! `expo shell` (the v1 path); `expo alpha *` lets us iterate on
//! the alpha track end-to-end without touching the v1 surface.
//!
//! Each command carries its own copy of the pipeline driver since
//! they run a single source file and have no REPL state to thread.
//! The REPL itself lives in [`expo_alpha_shell`]; `cmd_shell` is
//! just a thin entry point that hands control off to it. When the
//! alpha shell grows file-input support all four handlers will
//! collapse into `expo_alpha_shell` and this module will retire
//! alongside the v1 `expo-shell` / `expo-ir-eval` crates.
//!
//! ## Mode dispatch
//!
//! Two orthogonal axes drive every command except `shell`:
//!
//! - **Source shape** — `.exps` (script, parsed [`ParseMode::Script`],
//!   lowered via [`lower_script`]) vs `.expo` (project file, parsed
//!   [`ParseMode::File`], lowered via `lower_program` once project
//!   mode lands).
//! - **Command verb** — `build` (compile, keep), `run` (execute),
//!   `check` (parse + typecheck only).
//!
//! [`resolve_alpha_mode`] categorizes the input into one of three
//! [`AlphaMode`] variants — `Script(.exps)`, `Program(.expo
//! standalone)`, or `Project(cwd + expo.toml)`. Each command then
//! decides what to do:
//!
//! | mode      | check                              | run / build                  |
//! |-----------|------------------------------------|------------------------------|
//! | `Script`  | parse Script + check               | full script pipeline         |
//! | `Program` | parse File + check (LSP-friendly)  | error: `.expo` needs project |
//! | `Project` | error: project mode is stubbed     | error: project mode stubbed  |
//!
//! `cmd_shell` has no file dimension and bypasses the resolver
//! entirely; REPL fragments are always script-mode. The `Project`
//! arm is a stub today (see [`PROJECT_MODE_STUB`]) — the resolver
//! detects an `expo.toml` so a follow-up PR can swap in the real
//! pipeline without revisiting the dispatch shape.
//!
//! ## Backend selection
//!
//! `run` and `build` accept `--backend={interpreter,llvm}` (see
//! [`Backend`]):
//!
//! - `run` defaults to [`Backend::Interpreter`]: lower → run via
//!   [`Interpreter::run_script`] → print the trailing value (Unit
//!   suppressed) → exit 0. Fast feedback, no link step.
//! - `run --backend=llvm`: lower → [`expo_alpha_ir_llvm::compile_script`]
//!   → link → exec the temp binary → forward its exit code.
//! - `build` defaults to [`Backend::Llvm`]: lower → compile →
//!   link → keep the binary at the output path.
//! - `build --backend=interpreter`: errors. The interpreter has
//!   no codegen surface, so there's nothing to write out.
//! - `check` and `shell` have no backend dimension.
//!
//! Scope today (mirrors `expo-alpha-typecheck` / `expo-alpha-ir`):
//! integer literals, integer arithmetic (`+ - * / %`),
//! boolean/comparison/unary operators, and parenthesized groups.
//! Anything richer typecheck-errors with a precise diagnostic.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use expo_alpha_ir::{IRScript, lower_script};
use expo_alpha_ir_eval::{Interpreter, Value};
use expo_alpha_typecheck::{CheckFailure, CheckedProgram, check_program, format_registry};
use expo_ast::ast::Diagnostic;
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};

use crate::pipeline::{self, BuildOptions};
use crate::project;

/// Shared error string for the (currently unimplemented) alpha
/// project pipeline. Resolver detection is wired in but every
/// command bails with this exact message when the user lands in
/// [`AlphaMode::Project`]. Tests pin on this string so the
/// follow-up PR that fills in project mode replaces a stub rather
/// than a structure.
const PROJECT_MODE_STUB: &str = "alpha project mode is not yet implemented";

/// Which downstream backend a `run` / `build` invocation drives.
///
/// `expo alpha run` defaults to [`Backend::Interpreter`] (fast
/// feedback, no link step); `expo alpha build` defaults to
/// [`Backend::Llvm`] (the only backend that produces a binary
/// today). `build --backend=interpreter` is rejected up front
/// since the interpreter can't emit object files.
///
/// Future-proofing: when a WASM backend lands it slots in as a
/// third variant here and `build --backend=wasm` becomes a
/// one-line CLI extension. `check` and `shell` have no backend
/// dimension and don't reference this enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// Run via [`expo_alpha_ir_eval`]. Default for `run`. Not
    /// valid for `build` — the interpreter doesn't produce object
    /// files.
    Interpreter,
    /// Compile + link via [`expo_alpha_ir_llvm`]. Default for
    /// `build`. For `run`, compiles to a temp binary, execs it,
    /// and forwards the binary's exit code.
    Llvm,
}

/// Categorized source input for an `expo alpha` command.
///
/// [`resolve_alpha_mode`] inspects the file extension (or, when no
/// file is provided, looks for an `expo.toml` in the current
/// directory) and produces one of these variants. Each command
/// decides which subset it accepts: `cmd_check` accepts all three
/// (the `Project` arm is stubbed for now); `cmd_build`, `cmd_run`,
/// and `cmd_eval` reject `Program` outright since executing a
/// `.expo` file outside a project requires guessing the entry
/// point and dependency graph.
enum AlphaMode {
    /// Standalone script (`.exps`). Top-level expressions are
    /// first-class; lowered via [`lower_script`].
    Script(PathBuf),
    /// Project file (`.expo`) provided directly. Only `cmd_check`
    /// accepts this — the others bail because executing a `.expo`
    /// outside a project has no entry-point story.
    Program(PathBuf),
    /// No file argument; an `expo.toml` was found in the current
    /// directory and parsed cleanly. The pipeline for this variant
    /// is stubbed today (see [`PROJECT_MODE_STUB`]); the follow-up
    /// PR that fills in project mode will re-add `(PathBuf,
    /// ProjectConfig)` fields here once it actually consumes them.
    /// Until then the resolver still calls [`project::load_project`]
    /// to validate `expo.toml` is well-formed; we just don't carry
    /// the loaded config.
    Project,
}

/// Categorize the user's input into an [`AlphaMode`].
///
/// With a file argument: canonicalize, then dispatch on the
/// extension (`.exps` → [`AlphaMode::Script`], `.expo` →
/// [`AlphaMode::Program`], anything else → unrecognized-extension
/// error).
///
/// With no file argument: read `expo.toml` from the current
/// directory. `Some` → [`AlphaMode::Project`], `None` →
/// "missing expo.toml" error.
///
/// Errors are returned as `Err(message)`; callers print them with
/// the usual `error: …` prefix and exit non-zero.
fn resolve_alpha_mode(file: Option<&str>) -> Result<AlphaMode, String> {
    if let Some(arg) = file {
        let path = canonical_source_path(arg);
        return match path.extension().and_then(OsStr::to_str) {
            Some("exps") => Ok(AlphaMode::Script(path)),
            Some("expo") => Ok(AlphaMode::Program(path)),
            _ => Err(format!(
                "unrecognized source extension for `{}`: expected `.expo` or `.exps`",
                path.display()
            )),
        };
    }
    let cwd = std::env::current_dir()
        .map_err(|err| format!("cannot determine current directory: {err}"))?;
    match project::load_project(&cwd).map_err(|err| err.to_string())? {
        Some(_config) => Ok(AlphaMode::Project),
        None => {
            Err("no source file specified and no `expo.toml` found in current directory".into())
        }
    }
}

/// Bail with the standalone-`.expo` error. Reused by every command
/// except `cmd_check` — execution-flavored commands need a project
/// context (entry point selection, dependency graph) that a bare
/// file can't supply.
fn bail_program_outside_project(path: &Path) -> ! {
    eprintln!(
        "error: `{}` is a project file; place it in a directory with `expo.toml`, \
         or rename to `.exps` to run it as a standalone script",
        path.display()
    );
    process::exit(1);
}

/// Bail with the project-mode stub message. Shared by all four
/// `cmd_*` handlers so they emit identical text and tests can pin
/// on a single string.
fn bail_project_mode_stub() -> ! {
    eprintln!("error: {PROJECT_MODE_STUB}");
    process::exit(1);
}

/// Bail with a resolver error. Wraps the message in the standard
/// `error: …` prefix so each command's call site reads as a single
/// statement.
fn bail_resolve_error(message: String) -> ! {
    eprintln!("error: {message}");
    process::exit(1);
}

/// Bail when the user asks `cmd_build` to use the interpreter.
/// The interpreter has no codegen surface so it can't write an
/// object file — there's nothing useful for `build` to produce.
fn bail_interpreter_no_binary() -> ! {
    eprintln!(
        "error: --backend=interpreter cannot produce a binary; \
         use --backend=llvm or omit the flag"
    );
    process::exit(1);
}

/// `expo alpha check [file]` — parse and typecheck a single source
/// file (or, eventually, a whole project) through the alpha
/// pipeline. Mirrors `expo check`'s contract: prints
/// `<path>: OK` on success, or the collected parse/type
/// diagnostics on failure (exit 1). When `emit_ast` is set, prints
/// the sealed AST in [`expo_ast::format_file`]'s compact tree
/// format instead of the OK line.
///
/// `cmd_check` is the only command that accepts a standalone
/// `.expo` file (parsed in [`ParseMode::File`]) — typecheck has no
/// runtime semantics, so the absence of project context isn't a
/// problem and LSP/editor flows lean on this.
pub fn cmd_check(file: Option<String>, emit_ast: bool) {
    let mode = resolve_alpha_mode(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match mode {
        AlphaMode::Script(path) => check_single_file(&path, ParseMode::Script, emit_ast),
        AlphaMode::Program(path) => check_single_file(&path, ParseMode::File, emit_ast),
        AlphaMode::Project => bail_project_mode_stub(),
    }
}

/// `expo alpha shell` — interactive REPL on top of the alpha
/// pipeline. REPL fragments have no file dimension and are always
/// script-mode, so this command bypasses the resolver and the
/// `--backend` flag entirely (the REPL is interpreter-only by
/// design). Delegates to [`expo_alpha_shell::run`]; the REPL
/// crate owns session state, multiline detection, command
/// parsing, and its own pipeline driver.
pub fn cmd_shell() {
    expo_alpha_shell::run();
}

/// `expo alpha build [file] [--backend=llvm|interpreter] [-o output]`
/// — produce a native binary for a `.exps` script (or, eventually,
/// a project) on disk.
///
/// `--backend` defaults to [`Backend::Llvm`] — the only backend
/// that emits object files. [`Backend::Interpreter`] errors here
/// since there's nothing useful to write out. For a `.exps`
/// argument: parse Script → check → [`lower_script`] →
/// [`expo_alpha_ir_llvm::compile_script`] → link. The script body
/// becomes `main`'s body, so executing the binary prints the
/// script's trailing value and exits 0 (via the temporary
/// auto-print wrapper in `expo-runtime/src/alpha.rs`; goes away
/// with `IO.puts`). `-o`/`--output` overrides the default
/// stem-based output name.
pub fn cmd_build(file: Option<String>, backend: Backend, output: Option<String>) {
    let mode = resolve_alpha_mode(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match (mode, backend) {
        (AlphaMode::Script(_), Backend::Interpreter) => bail_interpreter_no_binary(),
        (AlphaMode::Script(path), Backend::Llvm) => build_and_keep(&path, output),
        (AlphaMode::Program(path), _) => bail_program_outside_project(&path),
        (AlphaMode::Project, _) => bail_project_mode_stub(),
    }
}

/// `expo alpha run [file] [--backend=interpreter|llvm] [-- args...]`
/// — execute a `.exps` script (or, eventually, a project) through
/// the chosen backend.
///
/// `--backend` defaults to [`Backend::Interpreter`]: parse Script
/// → check → [`lower_script`] → [`Interpreter::run_script`] →
/// print the trailing value (Unit suppressed). Exit 0 on success,
/// 1 on any pipeline failure. [`Backend::Llvm`] takes the compiled
/// path: parse Script → check → [`lower_script`] →
/// [`expo_alpha_ir_llvm::compile_script`] → link → write the
/// binary to a temp path → exec it (forwarding `args`) → forward
/// its exit code → remove the temp binary. `cmd_run` leaves no
/// artifacts behind on either backend.
pub fn cmd_run(file: Option<String>, backend: Backend, args: Vec<String>) {
    let mode = resolve_alpha_mode(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match (mode, backend) {
        (AlphaMode::Script(path), Backend::Interpreter) => run_script_interpreted(&path),
        (AlphaMode::Script(path), Backend::Llvm) => run_script_compiled(&path, &args),
        (AlphaMode::Program(path), _) => bail_program_outside_project(&path),
        (AlphaMode::Project, _) => bail_project_mode_stub(),
    }
}

/// Build the `.exps` script at `path` through LLVM and keep the
/// resulting binary at `output` (or a stem-derived default). Used
/// by `cmd_build` when the user picks the LLVM backend.
fn build_and_keep(path: &Path, output: Option<String>) {
    let script = build_script(path);
    let output = resolve_output_name(output, path);
    emit_and_link_script(&script, &derive_package(path), &output);
    println!("compiled: {output}");
}

/// Build the `.exps` script at `path` into a temp binary, exec
/// it with `args`, forward the exit code, and remove the temp
/// binary. Diverges either way — we either exit with the binary's
/// status or print a launch error and exit 1. Used by `cmd_run`
/// when the user picks the LLVM backend.
fn run_script_compiled(path: &Path, args: &[String]) -> ! {
    let script = build_script(path);
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("alpha_program");
    let output = std::env::temp_dir()
        .join(format!("expo-alpha-run-{}-{stem}", process::id()))
        .to_string_lossy()
        .to_string();
    emit_and_link_script(&script, &derive_package(path), &output);

    let status = process::Command::new(&output).args(args).status();
    let _ = fs::remove_file(&output);

    match status {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("error: failed to exec `{output}`: {err}");
            process::exit(1);
        }
    }
}

/// Run the `.exps` script at `path` through the interpreter and
/// print the trailing value via the `Debug.format` instance for
/// its static type — `value.print()` semantics, so the auto-print
/// surface matches what user code would see writing
/// `IO.puts(value.format())`. [`Value::Unit`] suppresses the print
/// so void scripts don't render `()`. Any pipeline failure prints
/// `error: <details>` and exits 1; success exits 0. The LLVM
/// backend matches this contract via the auto-print wrapper in
/// `expo-runtime/src/alpha.rs`. Used by `cmd_run` when the user
/// picks the interpreter backend.
fn run_script_interpreted(path: &Path) {
    let source = read_source_or_exit(path);
    let package = derive_package(path);
    match run_script_pipeline(source, &package, path.to_path_buf()) {
        Ok((_, Value::Unit)) => {}
        Ok((script, value)) => match Interpreter::format_via_debug(&script, value.clone()) {
            Ok(Some(bytes)) => println!("{}", String::from_utf8_lossy(&bytes)),
            Ok(None) => println!("{value}"),
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// Typecheck a single source file in the requested parse mode.
/// Shared by the `Script` and `Program` arms of `cmd_check` — the
/// only difference between them is the parse mode; the rest of the
/// frontend (typecheck, OK/AST emission, error rendering) is
/// identical.
fn check_single_file(path: &Path, mode: ParseMode, emit_ast: bool) {
    let source = read_source_or_exit(path);
    let package = derive_package(path);
    match run_check(source, &package, path.to_path_buf(), mode) {
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

/// Wrap one user-supplied [`SourceFile`] with the curated alpha
/// stdlib auto-import (`Global.time`, `Global.bitwise`, …) plus the
/// curated qualified packages (`Crypto.*`, …) so the driver, alpha
/// test helpers, and `cmd_check` all feed the parser the same
/// compilation unit. Stdlib sources lead so the registry sees
/// `Global.*` and qualified declarations before any user code that
/// references them; the user file is appended last. Autoimports
/// land first, qualified packages second, user file last — order
/// is semantically irrelevant (every entry registers under its own
/// `Identifier`) but keeps debug listings stable.
fn bundle_with_autoimport(user: SourceFile) -> Vec<SourceFile> {
    let mut sources = expo_stdlib::alpha_autoimport_sources();
    sources.extend(expo_stdlib::alpha_qualified_sources());
    sources.push(user);
    sources
}

/// Read a source file and drive it through the script-mode alpha
/// pipeline (`parse → check → lower_script`). Returns the sealed
/// [`IRScript`] on success; bails the process on any pipeline
/// failure. `cmd_run` and `cmd_build` use this for the `.exps`
/// path.
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
    let source = read_source_or_exit(path);
    let package = derive_package(path);
    let parsed = parse_program(
        bundle_with_autoimport(SourceFile {
            package: package.clone(),
            path: path.to_path_buf(),
            source,
        }),
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

/// Read a source file or bail with `error: cannot read …`. Used by
/// every command that opens a file directly; the `build_*` family
/// goes through [`read_and_check`] which calls this helper
/// internally.
fn read_source_or_exit(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: cannot read `{}`: {err}", path.display());
            process::exit(1);
        }
    }
}

/// Compile the [`IRScript`] to an object file and link it into a
/// native binary at `output`, reusing v1's
/// [`pipeline::link`](crate::pipeline) helper for `cc` invocation,
/// runtime archive embedding, and BoringSSL linkage. `app_name`
/// flows into the binary's `__expo_app_name` global (panic
/// backtrace label). `script.link_libraries` (deduped at lower
/// time from every `@extern "C" @link "lib"`) flows through to
/// `cc -l<name>` so FFI calls resolve at link time.
fn emit_and_link_script(script: &IRScript, app_name: &str, output: &str) {
    let object_path = format!("{output}.o");
    if let Err(err) = expo_alpha_ir_llvm::compile_script(script, app_name, Path::new(&object_path))
    {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(&object_path, output, &script.link_libraries);
}

fn link_object(object_path: &str, output: &str, link_libraries: &[String]) {
    let options = BuildOptions {
        color: false,
        emit_llvm: false,
        quiet: true,
        release: false,
    };
    pipeline::link(object_path, output, link_libraries, options);
}

/// Canonicalize a user-supplied source path, exiting on miss with
/// a matching error message to v1's `expo build`.
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
/// pipeline. Returns the sealed [`IRScript`] alongside the trailing
/// value so the caller can dispatch follow-up helpers (e.g.
/// `Debug.format` auto-print) without re-lowering the source. On
/// failure returns a formatted error string covering parse /
/// typecheck / lower / runtime failures.
fn run_script_pipeline(
    source: String,
    package: &str,
    path: PathBuf,
) -> Result<(IRScript, Value), String> {
    let parsed = parse_program(
        bundle_with_autoimport(SourceFile {
            package: package.to_string(),
            path,
            source,
        }),
        ParseMode::Script,
    );
    let checked = check_program(parsed).map_err(format_check_failure)?;
    let script = lower_script(&checked).map_err(|err| err.to_string())?;
    let value = Interpreter::run_script(&script).map_err(|err| err.to_string())?;
    Ok((script, value))
}

/// Parse + typecheck one source file in the requested parse mode.
/// Returns the sealed [`CheckedProgram`] on success, or a formatted
/// error string on parse/typecheck failure. Used by
/// [`check_single_file`].
fn run_check(
    source: String,
    package: &str,
    path: PathBuf,
    mode: ParseMode,
) -> Result<CheckedProgram, String> {
    let parsed = parse_program(
        bundle_with_autoimport(SourceFile {
            package: package.to_string(),
            path,
            source,
        }),
        mode,
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
