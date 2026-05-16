//! `expo {check,shell,build,run,eval,test}` subcommand handlers.
//!
//! Drives the compiler pipeline (`expo-typecheck → expo-ir →
//! expo-ir-eval` / `expo-ir-llvm`) for every command that touches
//! a source file or project.
//!
//! Each command carries its own copy of the pipeline driver since
//! they run a single source file and have no REPL state to thread.
//! The REPL itself lives in [`expo_shell`]; `cmd_shell` is just a
//! thin entry point that hands control off to it.
//!
//! ## Mode dispatch
//!
//! Two orthogonal axes drive every command except `shell`:
//!
//! - **Source shape** — `.exps` (script, parsed [`ParseMode::Script`],
//!   lowered via [`lower_script`]) vs `.expo` (project file, parsed
//!   [`ParseMode::File`], lowered via [`lower_program`]).
//! - **Command verb** — `build` (compile, keep), `run` (execute),
//!   `check` (parse + typecheck only).
//!
//! [`resolve_source_shape`] categorizes the input into one of
//! three [`SourceShape`] variants — `Script(.exps)`,
//! `Program(.expo standalone)`, or `Project { config, root }`.
//! Each command then decides what to do:
//!
//! | mode      | check                              | run / build                                |
//! |-----------|------------------------------------|--------------------------------------------|
//! | `Script`  | parse Script + check               | full script pipeline                       |
//! | `Program` | parse File + check (LSP-friendly)  | error: `.expo` needs project               |
//! | `Project` | parse + check whole project        | full project pipeline (LLVM backend only)  |
//!
//! `cmd_shell` has no file dimension and bypasses the resolver
//! entirely; REPL fragments are always script-mode. Project mode
//! routes through [`expo_ir::lower_program`] +
//! [`expo_ir_llvm::compile_program`]. PascalCase entries
//! (`entry = "App"`) name a `Process<C, M, R>` state type and
//! lower as [`ProjectEntry::Process`]; lowercase entries
//! (`entry = "main"`) name a `fn main` and lower as
//! [`ProjectEntry::Function`].
//!
//! ## Backend selection
//!
//! `run` and `build` accept `--backend={interpreter,llvm}` (see
//! [`Backend`]):
//!
//! - `run` defaults to [`Backend::Interpreter`]: lower → run via
//!   [`Interpreter::run_script`] → exit 0. The trailing
//!   expression's value is discarded; user code calls
//!   `IO.puts` / `value.print()` explicitly for output. Fast
//!   feedback, no link step.
//! - `run --backend=llvm`: lower → [`expo_ir_llvm::compile_script`]
//!   → link → exec the temp binary → forward its exit code.
//! - `build` defaults to [`Backend::Llvm`]: lower → compile →
//!   link → keep the binary at the output path.
//! - `build --backend=interpreter`: errors. The interpreter has
//!   no codegen surface, so there's nothing to write out.
//! - `check` and `shell` have no backend dimension.
//!
//! Scope today (mirrors `expo-typecheck` / `expo-ir`): the full
//! feature set surface those crates expose. Anything beyond
//! typecheck-errors with a precise diagnostic.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use expo_ast::ast::Diagnostic;
use expo_ast::identifier::Identifier;
use expo_ir::{IRProgram, IRScript, ProjectEntry, lower_program, lower_script};
use expo_ir_eval::Interpreter;
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_file, parse_program};
use expo_test::{TestCase, discover_tests, generate_harness};
use expo_typecheck::{CheckFailure, CheckedProgram, check_program, format_registry};

use crate::commands::load_project_or_exit;
use crate::link::{self, LinkOptions};
use crate::project::{self, ProjectConfig};

/// Which downstream backend a `run` / `build` invocation drives.
///
/// `expo run` defaults to [`Backend::Interpreter`] (fast
/// feedback, no link step); `expo build` defaults to
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
    /// Run via [`expo_ir_eval`]. Default for `run`. Not
    /// valid for `build` — the interpreter doesn't produce object
    /// files.
    Interpreter,
    /// Compile + link via [`expo_ir_llvm`]. Default for
    /// `build`. For `run`, compiles to a temp binary, execs it,
    /// and forwards the binary's exit code.
    Llvm,
}

/// Categorized source input for an `expo` command.
///
/// [`resolve_source_shape`] inspects the file extension (or, when no
/// file is provided, looks for an `expo.toml` in the current
/// directory) and produces one of these variants. Each command
/// decides which subset it accepts: `cmd_check` accepts all three
/// (the `Project` arm is stubbed for now); `cmd_build`, `cmd_run`,
/// and `cmd_eval` reject `Program` outright since executing a
/// `.expo` file outside a project requires guessing the entry
/// point and dependency graph.
enum SourceShape {
    /// Standalone script (`.exps`). Top-level expressions are
    /// first-class; lowered via [`lower_script`].
    Script(PathBuf),
    /// Project file (`.expo`) provided directly. Only `cmd_check`
    /// accepts this — the others bail because executing a `.expo`
    /// outside a project has no entry-point story.
    Program(PathBuf),
    /// No file argument; an `expo.toml` was found in the current
    /// directory and parsed cleanly. Carries the parsed
    /// [`ProjectConfig`] and the project root (the directory the
    /// manifest sits in) so the per-command handlers can walk
    /// `src` directories and resolve dependencies without re-loading
    /// the manifest.
    Project {
        config: ProjectConfig,
        root: PathBuf,
    },
}

/// Categorize the user's input into an [`SourceShape`].
///
/// With a file argument: canonicalize, then dispatch on the
/// extension (`.exps` → [`SourceShape::Script`], `.expo` →
/// [`SourceShape::Program`], anything else → unrecognized-extension
/// error).
///
/// With no file argument: read `expo.toml` from the current
/// directory. `Some` → [`SourceShape::Project`], `None` →
/// "missing expo.toml" error.
///
/// Errors are returned as `Err(message)`; callers print them with
/// the usual `error: …` prefix and exit non-zero.
fn resolve_source_shape(file: Option<&str>) -> Result<SourceShape, String> {
    if let Some(arg) = file {
        let path = canonical_source_path(arg);
        return match path.extension().and_then(OsStr::to_str) {
            Some("exps") => Ok(SourceShape::Script(path)),
            Some("expo") => Ok(SourceShape::Program(path)),
            _ => Err(format!(
                "unrecognized source extension for `{}`: expected `.expo` or `.exps`",
                path.display()
            )),
        };
    }
    let cwd = std::env::current_dir()
        .map_err(|err| format!("cannot determine current directory: {err}"))?;
    match project::load_project(&cwd).map_err(|err| err.to_string())? {
        Some(config) => Ok(SourceShape::Project { config, root: cwd }),
        None => {
            Err("no source file specified and no `expo.toml` found in current directory".into())
        }
    }
}

/// Bail when the user asks `cmd_run` to run a standalone `.expo`
/// file under the interpreter. The interpreter is script-mode-only
/// (`run_script_interpreted` shells out to `Interpreter::run_script`,
/// which doesn't yet drive a `fn main` body); LLVM compiles the
/// file as a single-file program.
fn bail_program_interpreter(path: &Path) -> ! {
    eprintln!(
        "error: `{}` is a `.expo` program; the interpreter backend only runs `.exps` scripts. \
         Use --backend=llvm or rename to `.exps`.",
        path.display()
    );
    process::exit(1);
}

/// Bail when the user asks for project-mode execution under the
/// interpreter. Same rationale as [`bail_program_interpreter`]:
/// the interpreter's entry path doesn't drive a `fn main` body
/// today.
fn bail_project_interpreter() -> ! {
    eprintln!("error: project mode currently requires --backend=llvm");
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

/// `expo check [file]` — parse and typecheck a single source
/// file (or, eventually, a whole project) through the
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
    let mode = resolve_source_shape(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match mode {
        SourceShape::Script(path) => check_single_file(&path, ParseMode::Script, emit_ast),
        SourceShape::Program(path) => check_single_file(&path, ParseMode::File, emit_ast),
        SourceShape::Project { config, root } => check_project(&config, &root, emit_ast),
    }
}

/// `expo shell` — interactive REPL on top of the
/// pipeline. REPL fragments have no file dimension and are always
/// script-mode, so this command bypasses the resolver and the
/// `--backend` flag entirely (the REPL is interpreter-only by
/// design). Delegates to [`expo_shell::run`]; the REPL
/// crate owns session state, multiline detection, command
/// parsing, and its own pipeline driver.
pub fn cmd_shell() {
    expo_shell::run();
}

/// `expo build [file] [--backend=llvm|interpreter] [-o output]`
/// — produce a native binary for a `.exps` script (or, eventually,
/// a project) on disk.
///
/// `--backend` defaults to [`Backend::Llvm`] — the only backend
/// that emits object files. [`Backend::Interpreter`] errors here
/// since there's nothing useful to write out. For a `.exps`
/// argument: parse Script → check → [`lower_script`] →
/// [`expo_ir_llvm::compile_script`] → link. The script body
/// becomes `main`'s body, so executing the binary prints the
/// script's trailing value and exits 0 (via the temporary
/// auto-print wrapper in `expo-runtime/src/intrinsics.rs`; goes away
/// with `IO.puts`). `-o`/`--output` overrides the default
/// stem-based output name.
pub fn cmd_build(
    file: Option<String>,
    backend: Backend,
    output: Option<String>,
    release: bool,
    emit_llvm: bool,
) {
    let mode = resolve_source_shape(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match (mode, backend) {
        (SourceShape::Script(_), Backend::Interpreter) => bail_interpreter_no_binary(),
        (SourceShape::Script(path), Backend::Llvm) => {
            build_and_keep(&path, output, release, emit_llvm)
        }
        (SourceShape::Program(_), Backend::Interpreter) => bail_interpreter_no_binary(),
        (SourceShape::Program(path), Backend::Llvm) => {
            build_single_file_and_keep(&path, output, release, emit_llvm)
        }
        (SourceShape::Project { .. }, Backend::Interpreter) => bail_interpreter_no_binary(),
        (SourceShape::Project { config, root }, Backend::Llvm) => {
            build_project_and_keep(&config, &root, output, release, emit_llvm)
        }
    }
}

/// `expo run [file] [--backend=interpreter|llvm] [-- args...]`
/// — execute a `.exps` script (or, eventually, a project) through
/// the chosen backend.
///
/// `--backend` defaults to [`Backend::Interpreter`]: parse Script
/// → check → [`lower_script`] → [`Interpreter::run_script`] →
/// print the trailing value (Unit suppressed). Exit 0 on success,
/// 1 on any pipeline failure. [`Backend::Llvm`] takes the compiled
/// path: parse Script → check → [`lower_script`] →
/// [`expo_ir_llvm::compile_script`] → link → write the
/// binary to a temp path → exec it (forwarding `args`) → forward
/// its exit code → remove the temp binary. `cmd_run` leaves no
/// artifacts behind on either backend.
pub fn cmd_run(file: Option<String>, backend: Backend, release: bool, args: Vec<String>) {
    let mode = resolve_source_shape(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match (mode, backend) {
        (SourceShape::Script(path), Backend::Interpreter) => run_script_interpreted(&path),
        (SourceShape::Script(path), Backend::Llvm) => run_script_compiled(&path, release, &args),
        (SourceShape::Program(path), Backend::Interpreter) => bail_program_interpreter(&path),
        (SourceShape::Program(path), Backend::Llvm) => {
            run_single_file_compiled(&path, release, &args)
        }
        (SourceShape::Project { .. }, Backend::Interpreter) => bail_project_interpreter(),
        (SourceShape::Project { config, root }, Backend::Llvm) => {
            run_project_compiled(&config, &root, release, &args)
        }
    }
}

/// `expo test` — discover `@test`-annotated functions in the
/// current project, synthesize a `fn main` harness, lower the
/// whole thing through the pipeline, link via LLVM, and
/// exec the resulting binary so its exit code surfaces test
/// success/failure.
///
/// Requires an `expo.toml` in the current directory. Walks
/// `config.src` AND `config.test` for the project itself; deps
/// contribute only `src`. Autoimport is suppressed when the
/// project IS `Global`, since lib/global/src already provides the
/// stdlib roots and a second copy would collide at registration
/// time.
///
pub fn cmd_test() {
    let (config, root) = load_project_or_exit(&[
        "error: no expo.toml found",
        "Usage: expo test (run from a directory containing expo.toml)",
    ]);
    run_project_tests(&config, &root);
}

/// Build the `.exps` script at `path` through LLVM and keep the
/// resulting binary at `output` (or a stem-derived default). Used
/// by `cmd_build` when the user picks the LLVM backend. When
/// `emit_llvm` is set, print the textual LLVM IR to stdout and
/// short-circuit before linking — no `.o`, no binary.
fn build_and_keep(path: &Path, output: Option<String>, release: bool, emit_llvm: bool) {
    let script = build_script(path);
    let app_name = derive_package(path);
    if emit_llvm {
        print_script_ir(&script, &app_name);
        return;
    }
    let output = resolve_output_name(output, path);
    emit_and_link_script(&script, &app_name, &output, release);
    println!("compiled: {output}");
}

/// Build the `.exps` script at `path` into a temp binary, exec
/// it with `args`, forward the exit code, and remove the temp
/// binary. Diverges either way — we either exit with the binary's
/// status or print a launch error and exit 1. Used by `cmd_run`
/// when the user picks the LLVM backend.
fn run_script_compiled(path: &Path, release: bool, args: &[String]) -> ! {
    let script = build_script(path);
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("alpha_program");
    let output = std::env::temp_dir()
        .join(format!("expo-run-{}-{stem}", process::id()))
        .to_string_lossy()
        .to_string();
    emit_and_link_script(&script, &derive_package(path), &output, release);

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
/// discard the trailing value. Scripts always exit 0 on normal
/// completion; any pipeline failure prints `error: <details>` and
/// exits 1. The LLVM backend matches this contract — its `main`
/// trampoline (see `expo-ir-llvm/src/main_wrapper.rs`)
/// returns 0 after the user body's trailing expression evaluates.
/// Used by `cmd_run` when the user picks the interpreter backend.
fn run_script_interpreted(path: &Path) {
    let source = read_source_or_exit(path);
    let package = derive_package(path);
    if let Err(error) = run_script_pipeline(source, &package, path.to_path_buf()) {
        eprintln!("error: {error}");
        process::exit(1);
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

/// Wrap one user-supplied [`SourceFile`] with the curated
/// stdlib auto-import (`Global.time`, `Global.bitwise`, …) plus the
/// curated qualified packages (`Crypto.*`, …) so the driver,
/// test helpers, and `cmd_check` all feed the parser the same
/// compilation unit. Stdlib sources lead so the registry sees
/// `Global.*` and qualified declarations before any user code that
/// references them; the user file is appended last. Autoimports
/// land first, qualified packages second, user file last — order
/// is semantically irrelevant (every entry registers under its own
/// `Identifier`) but keeps debug listings stable.
///
/// Single-file callers (`.exps` scripts, standalone `.expo`,
/// `cmd_check` on one path) always pass `None` for `skip_package` —
/// those flows never declare project membership.
fn bundle_with_autoimport(user: SourceFile) -> Vec<SourceFile> {
    bundle_many_with_autoimport(vec![user], None)
}

/// Multi-file counterpart to [`bundle_with_autoimport`] for the
/// project-mode pipeline. Same lead-with-stdlib ordering; the
/// caller is expected to have already merged project + dependency
/// sources into `user_files`.
///
/// `skip_package` mirrors v1's
/// [`crate::resolve::insert_stdlib`] convention: when a project IS
/// one of the curated packages (e.g. building/testing `lib/global`,
/// `lib/json`, …) the on-disk sources already provide every decl
/// the autoimport would inject, and a second copy would collide at
/// registry seal time. Project-mode callers thread
/// `Some(&config.name)` through; single-file callers pass `None`.
fn bundle_many_with_autoimport(
    user_files: Vec<SourceFile>,
    skip_package: Option<&str>,
) -> Vec<SourceFile> {
    let mut sources = expo_stdlib::autoimport_sources();
    // Qualified stdlib packages (Crypto, HTTP, JSON, Net, …)
    // ship pre-baked against the published Global. Loading them
    // when the user IS compiling Global self-imports an
    // inconsistent pair — the user's edited `lib/global/src` would
    // co-exist with qualified packages typechecked against the
    // older baked Global, and protocol-impl resolution gets
    // confused (e.g. HTTP's `.clone()` calls fail to see the
    // user's `Global.clone` impls because the qualified
    // packages were lifted before user files joined the bundle).
    // Mirrors v1's behavior: qualified deps don't tag along on a
    // Global self-compile.
    if skip_package != Some("Global") {
        sources.extend(expo_stdlib::qualified_sources());
    }
    if let Some(skip) = skip_package {
        sources.retain(|file| file.package != skip);
    }
    sources.extend(user_files);
    sources
}

/// Read a source file and drive it through the script-mode
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
/// native binary at `output`, threading through [`link::link`] for
/// `cc` invocation, runtime archive embedding, and BoringSSL
/// linkage. `app_name` flows into the binary's `__expo_app_name`
/// global (panic backtrace label). `script.link_libraries`
/// (deduped at lower time from every `@extern "C" @link "lib"`)
/// flows through to `cc -l<name>` so FFI calls resolve at link
/// time.
/// Render the sealed [`IRScript`] as LLVM IR text and stream it to
/// stdout. Backs `expo build --emit-llvm` for script sources. The
/// IR matches what the compiled `.o` would carry — same module,
/// same `i64 main()` wrapper, same runtime helpers — minus the
/// object emission. Diverges with `process::exit(1)` on
/// codegen failure to keep the call site a single statement.
fn print_script_ir(script: &IRScript, app_name: &str) {
    match expo_ir_llvm::emit_script_llvm_ir(script, app_name) {
        Ok(ir) => print!("{ir}"),
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Render the sealed [`IRProgram`] as LLVM IR text and stream it
/// to stdout. Counterpart to [`print_script_ir`] for the project /
/// single-file `.expo` build paths.
fn print_program_ir(program: &IRProgram, app_name: &str) {
    match expo_ir_llvm::emit_llvm_ir(program, app_name) {
        Ok(ir) => print!("{ir}"),
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

fn emit_and_link_script(script: &IRScript, app_name: &str, output: &str, release: bool) {
    let object_path = format!("{output}.o");
    if let Err(err) = expo_ir_llvm::compile_script(script, app_name, Path::new(&object_path)) {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(&object_path, output, &script.link_libraries, &[], release);
}

fn link_object(
    object_path: &str,
    output: &str,
    link_libraries: &[String],
    extra_lib_search_paths: &[&Path],
    release: bool,
) {
    let options = LinkOptions {
        quiet: true,
        release,
    };
    link::link(
        object_path,
        output,
        link_libraries,
        extra_lib_search_paths,
        options,
    );
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

/// Run one source file end-to-end through the script-mode
/// pipeline. The trailing value is computed for its side effects
/// and discarded — scripts always exit 0 on normal completion. On
/// failure returns a formatted error string covering parse /
/// typecheck / lower / runtime failures.
fn run_script_pipeline(source: String, package: &str, path: PathBuf) -> Result<(), String> {
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
    Interpreter::run_script(&script)
        .map(|_| ())
        .map_err(|err| err.to_string())
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

/// `expo build` for a standalone `.expo` file. Parses,
/// checks, and lowers the file as its own one-file project (package
/// from the file stem, entry fixed to `main`), compiles via
/// `compile_program`, and links to a binary at `output` (defaulting
/// to the file stem). Mirrors v1's `expo build path/to/file.expo`
/// shape so users moving from v1 don't have to wrap every file in
/// an `expo.toml`.
fn build_single_file_and_keep(path: &Path, output: Option<String>, release: bool, emit_llvm: bool) {
    let program = build_single_file_program(path);
    let stem = single_file_package(path);
    if emit_llvm {
        print_program_ir(&program, &stem);
        return;
    }
    let output = resolve_output_name(output, path);
    emit_and_link_program(&program, &stem, &output, &[], release);
    println!("compiled: {output}");
}

/// `expo run` for a standalone `.expo` file: build into a
/// temp binary, exec with `args`, forward the exit code, and
/// remove the binary.
fn run_single_file_compiled(path: &Path, release: bool, args: &[String]) -> ! {
    let program = build_single_file_program(path);
    let stem = single_file_package(path);
    let output = std::env::temp_dir()
        .join(format!("expo-run-{}-{stem}", process::id()))
        .to_string_lossy()
        .to_string();
    emit_and_link_program(&program, &stem, &output, &[], release);

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

/// Drive a single-file `.expo` source through the full
/// pipeline (parse → check → `lower_program`). The package name
/// comes from the file stem; the entry function is fixed to
/// `main`. Bails with a formatted error on any pipeline failure.
fn build_single_file_program(path: &Path) -> IRProgram {
    let source = read_source_or_exit(path);
    let package = single_file_package(path);
    let parsed = parse_program(
        bundle_with_autoimport(SourceFile {
            package: package.clone(),
            path: path.to_path_buf(),
            source,
        }),
        ParseMode::File,
    );
    let checked = match check_program(parsed) {
        Ok(checked) => checked,
        Err(failure) => {
            eprintln!("error: {}", format_check_failure(failure));
            process::exit(1);
        }
    };
    let entry = Identifier::new(package, vec!["main".to_string()]);
    match lower_program(&checked, ProjectEntry::Function(entry)) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Derive the package name for a single-file `.expo` build. Falls
/// back to `App` when the path has no usable stem (matches
/// [`derive_package`]).
fn single_file_package(path: &Path) -> String {
    derive_package(path)
}

/// `expo check` for a project: walk every `src` directory,
/// resolve declared dependencies, parse + typecheck the whole set,
/// and print `<project>: OK` (or per-file ASTs when `emit_ast`
/// is set). Mirrors v1's `cmd_check`'s project arm but routes
/// through typecheck.
fn check_project(config: &ProjectConfig, root: &Path, emit_ast: bool) {
    let user_files = collect_project_sources_or_exit(config, root);
    let parsed = parse_program(
        bundle_many_with_autoimport(user_files, Some(&config.name)),
        ParseMode::File,
    );
    match check_program(parsed) {
        Ok(checked) => {
            if emit_ast {
                emit_checked_ast(&checked);
            } else {
                println!("{}: OK", config.name);
            }
        }
        Err(failure) => {
            eprintln!("error: {}", format_check_failure(failure));
            process::exit(1);
        }
    }
}

/// `expo build` for a project: parse + typecheck + lower the
/// whole project, compile via [`expo_ir_llvm::compile_program`],
/// and link to a binary at `output` (defaulting to
/// `target/debug/<config.name>`). Prints the final binary path.
fn build_project_and_keep(
    config: &ProjectConfig,
    root: &Path,
    output: Option<String>,
    release: bool,
    emit_llvm: bool,
) {
    let program = build_project_program(config, root);
    if emit_llvm {
        print_program_ir(&program, &config.name);
        return;
    }
    let output = match output {
        Some(o) => o,
        None => default_project_output(config, root, release),
    };
    emit_and_link_program(&program, &config.name, &output, &[root], release);
    println!("compiled: {output}");
}

/// `expo test` for a project: walk `src` + `test`, parse, discover
/// `@test` functions, splice a synthetic `fn main` harness into the
/// parsed program, lower with the harness as entry, link, exec the
/// binary, and forward its exit code. The temp binary is removed
/// after the run so repeated invocations don't accumulate artifacts
/// under `target/debug/`.
///
/// Diverges either way: success exits with the binary's status, any
/// pipeline failure or launch error prints `error: …` and exits 1.
/// The early `no tests found` path is the lone non-diverging branch.
fn run_project_tests(config: &ProjectConfig, root: &Path) {
    let user_files = collect_test_project_sources_or_exit(config, root);
    let bundled = bundle_many_with_autoimport(user_files, Some(&config.name));
    let mut parsed = parse_program(bundled, ParseMode::File);

    let tests = discover_tests(&parsed, &config.name);
    if tests.is_empty() {
        println!("no tests found");
        return;
    }

    splice_test_harness(&mut parsed, config, &tests);

    let checked = match check_program(parsed) {
        Ok(checked) => checked,
        Err(failure) => {
            eprintln!("error: {}", format_check_failure(failure));
            process::exit(1);
        }
    };
    let entry = Identifier::new(config.name.clone(), vec!["main".to_string()]);
    let program = match lower_program(&checked, ProjectEntry::Function(entry)) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    };

    let binary = project_target_dir(root, false)
        .join(format!("{}_test", config.name))
        .to_string_lossy()
        .to_string();
    emit_and_link_program(&program, &config.name, &binary, &[root], false);

    let status = process::Command::new(&binary).status();
    let _ = fs::remove_file(&binary);

    match status {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("error: failed to exec `{binary}`: {err}");
            process::exit(1);
        }
    }
}

/// Parse the generated harness source and splice it into `parsed`
/// under a synthetic `<Package.__test_harness__>` path. Bails the
/// process on a parse-time diagnostic — the harness is generated by
/// the driver and must always parse cleanly.
fn splice_test_harness(parsed: &mut ParsedProgram, config: &ProjectConfig, tests: &[TestCase]) {
    let harness_source = generate_harness(tests);
    let harness_path = PathBuf::from(format!("<{}.__test_harness__>", config.name));
    let harness_parsed = parse_file(
        SourceFile {
            package: config.name.clone(),
            path: harness_path.clone(),
            source: harness_source,
        },
        ParseMode::File,
    );
    if !harness_parsed.diagnostics.is_empty() {
        eprintln!("internal error: generated test harness failed to parse");
        for diag in &harness_parsed.diagnostics {
            eprintln!("  {}", diag.message);
        }
        process::exit(1);
    }
    parsed.order.push(harness_path.clone());
    parsed.files.insert(harness_path, harness_parsed);
}

/// Project-test source walk: every `src` file from the project AND
/// every dep, plus every `test` file from the project itself. Deps'
/// `test` directories are intentionally skipped — they only show up
/// when you `expo test` from inside that dep.
fn collect_test_project_sources_or_exit(config: &ProjectConfig, root: &Path) -> Vec<SourceFile> {
    match collect_test_project_sources(config, root) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

fn collect_test_project_sources(
    config: &ProjectConfig,
    root: &Path,
) -> Result<Vec<SourceFile>, String> {
    let mut files: Vec<SourceFile> = Vec::new();
    let mut seen_paths: BTreeSet<PathBuf> = BTreeSet::new();
    let mut seen_pkgs: BTreeSet<String> = BTreeSet::new();
    seen_pkgs.insert(config.name.clone());
    if config.name != "Global" {
        seen_pkgs.insert("Global".to_string());
    }

    push_package_sources(&config.name, &config.src, root, &mut files, &mut seen_paths)?;
    push_package_sources(
        &config.name,
        &config.test,
        root,
        &mut files,
        &mut seen_paths,
    )?;
    collect_project_dependencies(config, root, &mut files, &mut seen_paths, &mut seen_pkgs)?;
    Ok(files)
}

/// `expo run` for a project: build into a temp binary, exec
/// with `args`, forward the exit code, and remove the binary.
/// Diverges either way (binary status or launch error).
fn run_project_compiled(config: &ProjectConfig, root: &Path, release: bool, args: &[String]) -> ! {
    let program = build_project_program(config, root);
    let target = project_target_dir(root, release);
    let binary = target.join(&config.name).to_string_lossy().to_string();
    emit_and_link_program(&program, &config.name, &binary, &[root], release);

    let status = process::Command::new(&binary).args(args).status();
    match status {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("error: failed to exec `{binary}`: {err}");
            process::exit(1);
        }
    }
}

/// Drive the full project pipeline (collect → parse → check →
/// `lower_program`) and return the sealed [`IRProgram`]. Bails the
/// process with a formatted error on any failure.
fn build_project_program(config: &ProjectConfig, root: &Path) -> IRProgram {
    let user_files = collect_project_sources_or_exit(config, root);
    let parsed = parse_program(
        bundle_many_with_autoimport(user_files, Some(&config.name)),
        ParseMode::File,
    );
    let checked = match check_program(parsed) {
        Ok(checked) => checked,
        Err(failure) => {
            eprintln!("error: {}", format_check_failure(failure));
            process::exit(1);
        }
    };
    let entry = resolve_project_entry(config);
    match lower_program(&checked, entry) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Resolve the project's entry as a [`ProjectEntry`]. PascalCase
/// entries name a `Process<C, M, R>` state type
/// ([`ProjectEntry::Process`]); lowercase entries name a `fn main`
/// function ([`ProjectEntry::Function`]). The `Function` variant is
/// transitional and dies with v1 — every project will eventually
/// route through `Process`.
fn resolve_project_entry(config: &ProjectConfig) -> ProjectEntry {
    let entry = config.entry.as_deref().unwrap_or_else(|| {
        eprintln!("error: expo.toml has no `entry` field; required for build/run");
        process::exit(1);
    });
    let identifier = Identifier::new(config.name.clone(), vec![entry.to_string()]);
    if config.entry_type_name().is_some() {
        ProjectEntry::Process { state: identifier }
    } else {
        ProjectEntry::Function(identifier)
    }
}

/// Walk the project's `src` directories (and recursively, every
/// declared dep's `src` directories) and return one
/// [`SourceFile`] per `.expo` file with the right `package` field.
/// Bails on directory I/O errors or duplicate package names. Skips
/// `alpha_*` files belonging to dependencies (they're loaded
/// through the curated `AUTOIMPORT` set, not the dep's own
/// source tree).
fn collect_project_sources_or_exit(config: &ProjectConfig, root: &Path) -> Vec<SourceFile> {
    match collect_project_sources(config, root) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

fn collect_project_sources(config: &ProjectConfig, root: &Path) -> Result<Vec<SourceFile>, String> {
    let mut files: Vec<SourceFile> = Vec::new();
    let mut seen_paths: BTreeSet<PathBuf> = BTreeSet::new();
    let mut seen_pkgs: BTreeSet<String> = BTreeSet::new();
    seen_pkgs.insert(config.name.clone());
    if config.name != "Global" {
        seen_pkgs.insert("Global".to_string());
    }

    push_package_sources(&config.name, &config.src, root, &mut files, &mut seen_paths)?;
    collect_project_dependencies(config, root, &mut files, &mut seen_paths, &mut seen_pkgs)?;
    Ok(files)
}

/// Walk `[dependencies]`, load each dep's manifest, register its
/// package name, and push the dep's `src` files (excluding the
/// dep's own entry to avoid `fn main` collisions). Mirrors v1's
/// [`crate::resolve::resolve_dependencies`] without the
/// stdlib-collision short-circuit (the driver drives the curated stdlib
/// through `bundle_with_autoimport` instead of the embedded
/// `SOURCES` table).
fn collect_project_dependencies(
    config: &ProjectConfig,
    root: &Path,
    files: &mut Vec<SourceFile>,
    seen_paths: &mut BTreeSet<PathBuf>,
    seen_pkgs: &mut BTreeSet<String>,
) -> Result<(), String> {
    for (alias, dep) in &config.dependencies {
        let dep_root = match &dep.path {
            Some(p) => root.join(p),
            None => {
                return Err(format!(
                    "dependency `{alias}` has no `path` (git dependencies are not yet supported)"
                ));
            }
        };
        let dep_config = project::load_project(&dep_root)?.ok_or_else(|| {
            format!(
                "dependency `{alias}`: no expo.toml found at {}",
                dep_root.display()
            )
        })?;
        if !seen_pkgs.insert(dep_config.name.clone()) {
            return Err(format!(
                "duplicate package name `{}` in dependency graph (project, dependency `{alias}`, or implicit `Global`)",
                dep_config.name
            ));
        }
        push_package_sources(
            &dep_config.name,
            &dep_config.src,
            &dep_root,
            files,
            seen_paths,
        )?;
        if let Some(entry) = dep_config.entry.as_deref() {
            let entry_paths: Vec<PathBuf> = dep_config
                .src
                .iter()
                .map(|s| dep_root.join(s).join(format!("{entry}.expo")))
                .collect();
            files.retain(|f| !entry_paths.iter().any(|p| p == &f.path));
        }
    }
    Ok(())
}

/// Walk every `src` root under `package_root`, scoop up `.expo`
/// files, and push them as [`SourceFile`]s tagged with `package`.
/// `seen_paths` keeps overlapping roots from double-counting a file
/// across the multi-pass walk (project sources first, then each
/// dep's sources).
fn push_package_sources(
    package: &str,
    src_dirs: &[String],
    package_root: &Path,
    files: &mut Vec<SourceFile>,
    seen_paths: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    for src in src_dirs {
        let dir = package_root.join(src);
        if !dir.is_dir() {
            continue;
        }
        for path in walk_expo_files(&dir) {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let source = fs::read_to_string(&path)
                .map_err(|err| format!("error reading {}: {err}", path.display()))?;
            files.push(SourceFile {
                package: package.to_string(),
                path,
                source,
            });
        }
    }
    Ok(())
}

fn walk_expo_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_expo_files_into(dir, &mut out);
    out.sort();
    out
}

fn walk_expo_files_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_expo_files_into(&path, out);
        } else if path.extension().is_some_and(|e| e == "expo") {
            out.push(path);
        }
    }
}

/// Default output path for project builds:
/// `<root>/target/{debug,release}/<config.name>` depending on the
/// `release` flag.
fn default_project_output(config: &ProjectConfig, root: &Path, release: bool) -> String {
    project_target_dir(root, release)
        .join(&config.name)
        .to_string_lossy()
        .to_string()
}

fn project_target_dir(root: &Path, release: bool) -> PathBuf {
    let profile = if release { "release" } else { "debug" };
    let dir = root.join("target").join(profile);
    fs::create_dir_all(&dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create target directory: {e}");
        process::exit(1);
    });
    dir
}

/// Compile the [`IRProgram`] to an object file and link it into a
/// native binary at `output`. Sibling to [`emit_and_link_script`];
/// the only difference is the IR variant fed into the LLVM
/// backend. `app_name` flows into `__expo_app_name` and
/// `program.link_libraries` becomes the `cc -l<name>` set.
/// `extra_lib_search_paths` lets project-mode callers add the
/// project root to `-L` so a sibling `libfoo.a` resolves without
/// the user setting `LIBRARY_PATH` or invoking `expo` from a
/// specific working directory.
fn emit_and_link_program(
    program: &IRProgram,
    app_name: &str,
    output: &str,
    extra_lib_search_paths: &[&Path],
    release: bool,
) {
    if let Some(parent) = Path::new(output).parent()
        && !parent.as_os_str().is_empty()
        && let Err(err) = fs::create_dir_all(parent)
    {
        eprintln!(
            "error: failed to create output directory `{}`: {err}",
            parent.display()
        );
        process::exit(1);
    }
    let object_path = format!("{output}.o");
    if let Err(err) = expo_ir_llvm::compile_program(program, app_name, Path::new(&object_path)) {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(
        &object_path,
        output,
        &program.link_libraries,
        extra_lib_search_paths,
        release,
    );
}

/// Prints every file in the sealed program to stdout using
/// [`expo_ast::format_file`], followed by the compact registry
/// sidecar from [`expo_typecheck::format_registry`] so the ids
/// that appear on AST reference sites are decodable without a
/// separate lookup. Mirrors what `expo check --emit-ast` does for the
/// v1 pipeline on the AST side; the registry sidecar is pipeline-only.
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
