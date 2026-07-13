//! `koja {check,shell,build,run,eval,test}` subcommand handlers.
//!
//! Drives the compiler pipeline (`koja-typecheck -> koja-ir ->
//! koja-ir-eval` / `koja-ir-llvm`) for every command that touches
//! a source file or project.
//!
//! Each command carries its own copy of the pipeline driver since
//! they run a single source file and have no REPL state to thread.
//! The REPL itself lives in [`koja_shell`]; `cmd_shell` is just a
//! thin entry point that hands control off to it.
//!
//! ## Mode dispatch
//!
//! Two orthogonal axes drive every command except `shell`:
//!
//! - **Source shape** — `.kojs` (script, parsed [`ParseMode::Script`],
//!   lowered via [`lower_script`]) vs `.koja` (project file, parsed
//!   [`ParseMode::File`], lowered via [`lower_program`]).
//! - **Command verb** — `build` (compile, keep), `run` (execute),
//!   `check` (parse + typecheck only).
//!
//! [`resolve_source_shape`] categorizes the input into one of
//! three [`SourceShape`] variants — `Script(.kojs)`,
//! `Program(.koja standalone)`, or `Project { config, root }`.
//! Each command then decides what to do:
//!
//! | mode      | check                              | run / build                                |
//! |-----------|------------------------------------|--------------------------------------------|
//! | `Script`  | parse Script + check               | full script pipeline                       |
//! | `Program` | parse File + check (LSP-friendly)  | error: `.koja` needs project               |
//! | `Project` | parse + check whole project        | full project pipeline (either backend for `run`; `build` is always LLVM) |
//!
//! `cmd_shell` has no file dimension and bypasses the resolver
//! entirely; REPL fragments are always script-mode. Project mode
//! routes through [`koja_ir::lower_program`] +
//! [`koja_ir_llvm::compile_program`]. The manifest's `entry`
//! field names a PascalCase `Process<C, M, R>` state type; the
//! lowering synthesizes its entry wrapper.
//!
//! ## Backend selection
//!
//! Only `run` has a backend dimension — it accepts
//! `--backend={interpreter,llvm}` (see [`Backend`]):
//!
//! - `run` defaults to [`Backend::Interpreter`]: lower -> run via
//!   [`Interpreter::run_script`] (scripts, exit 0; the trailing
//!   expression's value is discarded — user code calls
//!   `IO.puts` / `value.print()` explicitly for output) or
//!   [`Interpreter::run_program`] (projects; the Process entry's
//!   exit code becomes the driver's exit status). Fast feedback,
//!   no link step.
//! - `run --backend=llvm`: lower -> [`koja_ir_llvm::compile_script`]
//!   / [`koja_ir_llvm::compile_program`] -> link -> exec the binary
//!   -> forward its exit code.
//! - `build` is always LLVM: lower -> compile -> link -> keep the
//!   binary at the output path. The interpreter has no codegen
//!   surface, so `build` carries no backend flag.
//! - `check` and `shell` have no backend dimension.
//!
//! Scope today (mirrors `koja-typecheck` / `koja-ir`): the full
//! feature set surface those crates expose. Anything beyond
//! typecheck-errors with a precise diagnostic.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use koja_ast::ast::Diagnostic;
use koja_ast::identifier::Identifier;
use koja_ir::{IRProgram, IRScript, lower_program, lower_script};
use koja_ir_eval::{Interpreter, RuntimeError, Value};
use koja_parser::{ParseMode, ParsedProgram, SourceFile, parse_file, parse_program};
use koja_test::{HARNESS_ENTRY, TestCase, TestOptions, discover_tests, generate_harness};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program, format_registry};

use crate::commands::load_project_or_exit;
use crate::link::{self, LinkOptions};
use crate::loader::{self, ErrorPolicy, LoadOptions, LoadedSource, ProjectLoader};
use crate::project::{self, ProjectConfig};

/// Which downstream backend a `run` invocation drives.
///
/// `koja run` defaults to [`Backend::Interpreter`] (fast feedback,
/// no link step) and accepts `--backend=llvm` to compile + exec.
/// `koja build` is LLVM-only and carries no backend flag — the
/// interpreter can't emit object files, so there's no choice to
/// expose.
///
/// Future-proofing: when a WASM backend lands it slots in as a
/// third variant here, and a `build --backend=wasm` flag can be
/// re-added then (it'll have two genuinely valid targets). `check`
/// and `shell` have no backend dimension and don't reference this
/// enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// Run via [`koja_ir_eval`]. Default for `run`. Not
    /// valid for `build` — the interpreter doesn't produce object
    /// files.
    Interpreter,
    /// Compile + link via [`koja_ir_llvm`]. Default for
    /// `build`. For `run`, compiles to a temp binary, execs it,
    /// and forwards the binary's exit code.
    Llvm,
}

/// Categorized source input for an `koja` command.
///
/// [`resolve_source_shape`] inspects the file extension (or, when no
/// file is provided, looks for an `koja.toml` in the current
/// directory) and produces one of these variants. Each command
/// decides which subset it accepts: `cmd_check` accepts all three
/// (the `Project` arm is stubbed for now); `cmd_build`, `cmd_run`,
/// and `cmd_eval` reject `Program` outright since executing a
/// `.koja` file outside a project requires guessing the entry
/// point and dependency graph.
enum SourceShape {
    /// Standalone script (`.kojs`). Top-level expressions are
    /// first-class; lowered via [`lower_script`].
    Script(PathBuf),
    /// Project file (`.koja`) provided directly. Only `cmd_check`
    /// accepts this — the others bail because executing a `.koja`
    /// outside a project has no entry-point story.
    Program(PathBuf),
    /// No file argument; an `koja.toml` was found in the current
    /// directory and parsed cleanly. Carries the parsed
    /// [`ProjectConfig`] and the project root (the directory the
    /// manifest sits in) so the per-command handlers can walk
    /// `src` directories and resolve dependencies without re-loading
    /// the manifest.
    Project {
        config: Box<ProjectConfig>,
        root: PathBuf,
    },
}

/// Categorize the user's input into an [`SourceShape`].
///
/// With a file argument: canonicalize, then dispatch on the
/// extension (`.kojs` -> [`SourceShape::Script`], `.koja` ->
/// [`SourceShape::Program`], anything else -> unrecognized-extension
/// error).
///
/// With no file argument: read `koja.toml` from the current
/// directory. `Some` -> [`SourceShape::Project`], `None` ->
/// "missing koja.toml" error.
///
/// Errors are returned as `Err(message)`; callers print them with
/// the usual `error: …` prefix and exit non-zero.
fn resolve_source_shape(file: Option<&str>) -> Result<SourceShape, String> {
    if let Some(arg) = file {
        let path = canonical_source_path(arg);
        return match path.extension().and_then(OsStr::to_str) {
            Some("kojs") => Ok(SourceShape::Script(path)),
            Some("koja") => Ok(SourceShape::Program(path)),
            _ => Err(format!(
                "unrecognized source extension for `{}`: expected `.koja` or `.kojs`",
                path.display()
            )),
        };
    }
    let cwd =
        env::current_dir().map_err(|err| format!("cannot determine current directory: {err}"))?;
    match project::load_project(&cwd).map_err(|err| err.to_string())? {
        Some(config) => Ok(SourceShape::Project {
            config: Box::new(config),
            root: cwd,
        }),
        None => {
            Err("no source file specified and no `koja.toml` found in current directory".into())
        }
    }
}

/// Bail when the user asks `cmd_build` / `cmd_run` to execute a
/// standalone `.koja` file. `.koja` files belong to a package.
/// Program entry points are `Process` types named by a manifest's
/// `entry` field, so a bare file has no entry-point story. Scripts
/// (`.kojs`) cover the zero-ceremony case.
fn bail_program_execution(path: &Path) -> ! {
    eprintln!(
        "error: `{}` is a `.koja` package source file and cannot be run directly. \
         Use a `.kojs` script for standalone programs, or create a `koja.toml` \
         with a `Process` entry type.",
        path.display()
    );
    process::exit(1);
}

/// Bail with a resolver error. Wraps the message in the standard
/// `error: …` prefix so each command's call site reads as a single
/// statement.
fn bail_resolve_error(message: String) -> ! {
    eprintln!("error: {message}");
    process::exit(1);
}

/// `koja check [file]` — parse and typecheck a single source
/// file (or, eventually, a whole project) through the
/// pipeline. Mirrors `koja check`'s contract: prints
/// `<path>: OK` on success, or the collected parse/type
/// diagnostics on failure (exit 1). When `emit_ast` is set, prints
/// the sealed AST in [`koja_ast::format_file`]'s compact tree
/// format instead of the OK line.
///
/// `cmd_check` is the only command that accepts a standalone
/// `.koja` file (parsed in [`ParseMode::File`]) — typecheck has no
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

/// `koja shell` — interactive REPL on top of the
/// pipeline. REPL fragments have no file dimension and are always
/// script-mode, so this command bypasses the resolver and the
/// `--backend` flag entirely (the REPL is interpreter-only by
/// design). Delegates to [`koja_shell::run`]; the REPL
/// crate owns session state, multiline detection, command
/// parsing, and its own pipeline driver.
pub fn cmd_shell() {
    let ShellSession {
        baseline,
        session_package,
    } = shell_session();
    koja_shell::run(baseline, session_package);
}

/// What the REPL evaluates against: the baseline sources plus the
/// package the session source belongs to (the project's package in a
/// project, so its modules resolve unqualified; otherwise `REPL`).
struct ShellSession {
    baseline: Vec<SourceFile>,
    session_package: String,
}

/// Resolve the REPL session. In a project, [`ProjectLoader`] supplies
/// stdlib + project + dependency sources and the session adopts the
/// project's package; with no readable `koja.toml` (or on any load
/// failure) fall back to a stdlib-only `REPL` session. A malformed
/// manifest or broken dependency warns but never aborts the shell.
fn shell_session() -> ShellSession {
    let Ok(cwd) = env::current_dir() else {
        return stdlib_session();
    };
    let config = match project::load_project(&cwd) {
        Ok(Some(config)) => config,
        Ok(None) => return stdlib_session(),
        Err(err) => {
            eprintln!("warning: ignoring koja.toml: {err}");
            return stdlib_session();
        }
    };
    println!("loading project `{}`", config.name);
    match ProjectLoader::new(&config, &cwd).sources(LoadOptions {
        extensions: &["koja"],
        include_dependencies: true,
        include_stdlib: true,
        include_tests: false,
        on_error: ErrorPolicy::Lenient,
    }) {
        Ok(sources) => ShellSession {
            baseline: sources.into_iter().map(into_source_file).collect(),
            session_package: config.name,
        },
        Err(_) => stdlib_session(),
    }
}

/// Stdlib-only `REPL` session for a bare `koja shell` (no project),
/// reusing the same primitive [`ProjectLoader`] loads stdlib from.
fn stdlib_session() -> ShellSession {
    ShellSession {
        baseline: loader::stdlib_sources()
            .into_iter()
            .map(into_source_file)
            .collect(),
        session_package: koja_shell::SESSION_PACKAGE.to_string(),
    }
}

fn into_source_file(loaded: LoadedSource) -> SourceFile {
    SourceFile {
        package: loaded.package,
        path: loaded.path,
        source: loaded.source,
    }
}

/// `koja build [file] [-o output]` — produce a native binary for a
/// `.kojs` script (or a project) on disk. LLVM is the only backend
/// that emits object files, so `build` has no backend dimension.
///
/// For a `.kojs` argument: parse Script -> check -> [`lower_script`] ->
/// [`koja_ir_llvm::compile_script`] -> link. The script body becomes
/// `main`'s body, so executing the binary prints the script's
/// trailing value and exits 0 (via the temporary auto-print wrapper
/// in `koja-runtime-posix/src/intrinsics.rs`; goes away with
/// `IO.puts`). `-o`/`--output` overrides the default stem-based
/// output name.
pub fn cmd_build(file: Option<String>, output: Option<String>, release: bool, emit_llvm: bool) {
    let mode = resolve_source_shape(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match mode {
        SourceShape::Script(path) => build_and_keep(&path, output, release, emit_llvm),
        SourceShape::Program(path) => bail_program_execution(&path),
        SourceShape::Project { config, root } => {
            build_project_and_keep(&config, &root, output, release, emit_llvm)
        }
    }
}

/// `koja run [file] [--backend=interpreter|llvm] [-- args...]`
/// — execute a `.kojs` script or a project through the chosen
/// backend.
///
/// `--backend` defaults to [`Backend::Interpreter`]. Scripts:
/// parse Script -> check -> [`lower_script`] ->
/// [`Interpreter::run_script`]; exit 0 on success, 1 on any
/// pipeline failure. Projects: collect -> parse -> check ->
/// [`lower_program`] -> [`Interpreter::run_program`] (with `args`
/// as the argv-shaped config); the Process entry's exit code
/// becomes the driver's exit status. [`Backend::Llvm`] takes the
/// compiled path: lower -> compile -> link -> exec the binary
/// (forwarding `args`) -> forward its exit code; script binaries
/// are temp files removed after the run.
pub fn cmd_run(file: Option<String>, backend: Backend, release: bool, args: Vec<String>) {
    let mode = resolve_source_shape(file.as_deref()).unwrap_or_else(|err| bail_resolve_error(err));
    match (mode, backend) {
        (SourceShape::Script(path), Backend::Interpreter) => run_script_interpreted(&path),
        (SourceShape::Script(path), Backend::Llvm) => run_script_compiled(&path, release, &args),
        (SourceShape::Program(path), Backend::Interpreter)
        | (SourceShape::Program(path), Backend::Llvm) => bail_program_execution(&path),
        (SourceShape::Project { config, root }, Backend::Interpreter) => {
            run_project_interpreted(&config, &root, &args)
        }
        (SourceShape::Project { config, root }, Backend::Llvm) => {
            run_project_compiled(&config, &root, release, &args)
        }
    }
}

/// `koja test` — discover `@test`-annotated functions in the
/// current project, synthesize a Process-shaped harness type,
/// lower the whole thing through the pipeline, link via LLVM, and
/// exec the resulting binary so its exit code surfaces test
/// success/failure.
///
/// Requires an `koja.toml` in the current directory. Walks
/// `config.src` AND `config.test` for the project itself; deps
/// contribute only `src`. Autoimport is suppressed when the
/// project IS `Global`, since lib/global/src already provides the
/// stdlib roots and a second copy would collide at registration
/// time.
///
pub fn cmd_test(trace: bool, color: bool) {
    let (config, root) = load_project_or_exit(&[
        "error: no koja.toml found",
        "Usage: koja test (run from a directory containing koja.toml)",
    ]);
    run_project_tests(&config, &root, TestOptions { color, trace });
}

/// Build the `.kojs` script at `path` through LLVM and keep the
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

/// Build the `.kojs` script at `path` into a temp binary, exec
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
    let output = env::temp_dir()
        .join(format!("koja-run-{}-{stem}", process::id()))
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

/// Run the `.kojs` script at `path` through the interpreter and
/// discard the trailing value. Scripts always exit 0 on normal
/// completion; any pipeline failure prints `error: <details>` and
/// exits 1. The LLVM backend matches this contract — its `main`
/// trampoline (see `koja-ir-llvm/src/main_wrapper.rs`)
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
/// Single-file callers (`.kojs` scripts, standalone `.koja`,
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
    let mut sources = koja_stdlib::autoimport_sources();
    // Qualified stdlib packages (Crypto, HTTP, JSON, Net, …)
    // ship pre-baked against the published Global. Loading them
    // when the user IS compiling Global self-imports an
    // inconsistent pair — the user's edited `lib/global/src` would
    // co-exist with qualified packages typechecked against the
    // older baked Global, and protocol-impl resolution gets
    // confused (e.g. HTTP's `format`/`eq` calls fail to see the
    // user's edited `Global` protocol impls because the qualified
    // packages were lifted before user files joined the bundle).
    // Mirrors v1's behavior: qualified deps don't tag along on a
    // Global self-compile.
    if skip_package != Some("Global") {
        sources.extend(koja_stdlib::qualified_sources());
    }
    if let Some(skip) = skip_package {
        sources.retain(|file| file.package != skip);
    }
    sources.extend(user_files);
    sources
}

/// Read a source file and drive it through the script-mode
/// pipeline (`parse -> check -> lower_script`). Returns the sealed
/// [`IRScript`] on success; bails the process on any pipeline
/// failure. `cmd_run` and `cmd_build` use this for the `.kojs`
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
/// linkage. `app_name` flows into the binary's `__koja_app_name`
/// global (panic backtrace label). `script.link_libraries`
/// (deduped at lower time from every `@extern "C" @link "lib"`)
/// flows through to `cc -l<name>` so FFI calls resolve at link
/// time.
/// Render the sealed [`IRScript`] as LLVM IR text and stream it to
/// stdout. Backs `koja build --emit-llvm` for script sources. The
/// IR matches what the compiled `.o` would carry — same module,
/// same `i64 main()` wrapper, same runtime helpers — minus the
/// object emission. Diverges with `process::exit(1)` on
/// codegen failure to keep the call site a single statement.
fn print_script_ir(script: &IRScript, app_name: &str) {
    match koja_ir_llvm::emit_script_llvm_ir(script, app_name) {
        Ok(ir) => print!("{ir}"),
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Render the sealed [`IRProgram`] as LLVM IR text and stream it
/// to stdout. Counterpart to [`print_script_ir`] for the project /
/// single-file `.koja` build paths.
fn print_program_ir(program: &IRProgram, app_name: &str) {
    match koja_ir_llvm::emit_llvm_ir(program, app_name) {
        Ok(ir) => print!("{ir}"),
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

fn emit_and_link_script(script: &IRScript, app_name: &str, output: &str, release: bool) {
    let object_path = format!("{output}.o");
    if let Err(err) = koja_ir_llvm::compile_script(
        script,
        app_name,
        Path::new(&object_path),
        &koja_ir_llvm::CompileOptions { release },
    ) {
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
/// a matching error message to v1's `koja build`.
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

/// `koja check` for a project: walk every `src` directory,
/// resolve declared dependencies, parse + typecheck the whole set,
/// and print `<project>: OK` (or per-file ASTs when `emit_ast`
/// is set). Mirrors v1's `cmd_check`'s project arm but routes
/// through typecheck.
fn check_project(config: &ProjectConfig, root: &Path, emit_ast: bool) {
    let user_files = collect_project_sources_or_exit(config, root, false);
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

/// `koja build` for a project: parse + typecheck + lower the
/// whole project, compile via [`koja_ir_llvm::compile_program`],
/// and link to a binary at `output` (defaulting to
/// `build/debug/<config.name>`). Prints the final binary path.
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

/// `koja test` for a project: walk `src` + `test`, parse, discover
/// `@test` functions, splice the synthetic Process harness into the
/// parsed program, lower with the harness as entry, link, exec the
/// binary, and forward its exit code. The temp binary is removed
/// after the run so repeated invocations don't accumulate artifacts
/// under `build/debug/`.
///
/// Diverges either way: success exits with the binary's status, any
/// pipeline failure or launch error prints `error: …` and exits 1.
/// The early `no tests found` path is the lone non-diverging branch.
fn run_project_tests(config: &ProjectConfig, root: &Path, opts: TestOptions) {
    let user_files = collect_project_sources_or_exit(config, root, true);
    let bundled = bundle_many_with_autoimport(user_files, Some(&config.name));
    let mut parsed = parse_program(bundled, ParseMode::File);

    let tests = discover_tests(&parsed, &config.name, root);
    if tests.is_empty() {
        println!("no tests found");
        return;
    }

    splice_test_harness(&mut parsed, config, &tests, opts);

    let checked = match check_program(parsed) {
        Ok(checked) => checked,
        Err(failure) => {
            eprintln!("error: {}", format_check_failure(failure));
            process::exit(1);
        }
    };
    let entry = Identifier::new(config.name.clone(), vec![HARNESS_ENTRY.to_string()]);
    let program = match lower_program(&checked, &entry) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    };

    let binary = project_build_dir(root, false)
        .join(format!("{}_test", config.binary_name()))
        .to_string_lossy()
        .to_string();
    emit_and_link_program(&program, &config.name, &binary, &[root], false);

    // Trace runs are meant for interactive debugging (and the
    // per-binary timeout would kill a long diagnostic session), so
    // skip the deadline there, matching `mix test --trace`.
    let timeout = (!opts.trace).then_some(TEST_BINARY_TIMEOUT);
    let status = run_test_binary_with_timeout(&binary, timeout);
    let _ = fs::remove_file(&binary);

    match status {
        TestBinaryOutcome::Exited(code) => process::exit(code),
        TestBinaryOutcome::TimedOut => {
            eprintln!(
                "error: test binary `{binary}` exceeded {}s timeout and was killed",
                TEST_BINARY_TIMEOUT.as_secs(),
            );
            process::exit(1);
        }
        TestBinaryOutcome::LaunchFailed(err) => {
            eprintln!("error: failed to exec `{binary}`: {err}");
            process::exit(1);
        }
    }
}

/// Wall-clock cap on a `koja test` binary so a deadlocked runtime
/// surfaces as a failed test instead of hanging the dev loop.
const TEST_BINARY_TIMEOUT: Duration = Duration::from_secs(60);

enum TestBinaryOutcome {
    Exited(i32),
    LaunchFailed(io::Error),
    TimedOut,
}

/// Spawn `binary` and poll `try_wait` until it exits or the
/// deadline passes. On timeout, kill the child and report. A `None`
/// timeout waits indefinitely (used by `--trace`).
fn run_test_binary_with_timeout(binary: &str, timeout: Option<Duration>) -> TestBinaryOutcome {
    let mut child = match process::Command::new(binary).spawn() {
        Ok(c) => c,
        Err(e) => return TestBinaryOutcome::LaunchFailed(e),
    };

    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return TestBinaryOutcome::Exited(status.code().unwrap_or(1)),
            Ok(None) if deadline.is_some_and(|d| Instant::now() >= d) => {
                let _ = child.kill();
                let _ = child.wait();
                return TestBinaryOutcome::TimedOut;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return TestBinaryOutcome::LaunchFailed(e),
        }
    }
}

/// Parse the generated harness source and splice it into `parsed`
/// under a synthetic `<Package.__test_harness__>` path. Bails the
/// process on a parse-time diagnostic — the harness is generated by
/// the driver and must always parse cleanly.
fn splice_test_harness(
    parsed: &mut ParsedProgram,
    config: &ProjectConfig,
    tests: &[TestCase],
    opts: TestOptions,
) {
    let harness_source = generate_harness(tests, opts);
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
/// when you `koja test` from inside that dep.
/// `koja run` for a project under the interpreter: lower the full
/// project and execute the Process entry in-process via
/// [`Interpreter::run_program`] — no codegen, no link, no binary.
/// The entry body's returned exit code becomes the driver's exit
/// status. The entry process gets blocking socket/TLS externs and
/// `receive` over lifecycle signals + `after` timeouts; features the
/// interpreter doesn't cover yet (spawn, cross-process messaging)
/// surface a runtime error plus a `--backend=llvm` hint. Diverges
/// either way.
fn run_project_interpreted(config: &ProjectConfig, root: &Path, args: &[String]) -> ! {
    let program = build_project_program(config, root);
    match Interpreter::run_program(&program, args) {
        Ok(Value::Int(code)) => process::exit(code as i32),
        Ok(other) => {
            eprintln!("error: process entry returned non-integer exit value `{other}`");
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            if matches!(error, RuntimeError::Unsupported { .. }) {
                eprintln!(
                    "hint: this project uses process features the interpreter does not \
                     support yet; run with --backend=llvm"
                );
            }
            process::exit(1);
        }
    }
}

/// `koja run` for a project: build into a temp binary, exec
/// with `args`, forward the exit code, and remove the binary.
/// Diverges either way (binary status or launch error).
fn run_project_compiled(config: &ProjectConfig, root: &Path, release: bool, args: &[String]) -> ! {
    let program = build_project_program(config, root);
    let build_dir = project_build_dir(root, release);
    let binary = build_dir
        .join(config.binary_name())
        .to_string_lossy()
        .to_string();
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

/// Drive the full project pipeline (collect -> parse -> check ->
/// `lower_program`) and return the sealed [`IRProgram`]. Bails the
/// process with a formatted error on any failure.
fn build_project_program(config: &ProjectConfig, root: &Path) -> IRProgram {
    let user_files = collect_project_sources_or_exit(config, root, false);
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
    match lower_program(&checked, &entry) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Resolve the project's entry identifier. The manifest's `entry`
/// field names a PascalCase type implementing `Process<C, M, R>`;
/// `lower_program` synthesizes the entry wrapper for it. Lowercase
/// (function-shaped) entries are rejected here — `fn main` is no
/// longer an entry point.
fn resolve_project_entry(config: &ProjectConfig) -> Identifier {
    let entry = config.entry.as_deref().unwrap_or_else(|| {
        eprintln!("error: koja.toml has no `entry` field; required for build/run");
        process::exit(1);
    });
    if config.entry_type_name().is_none() {
        eprintln!(
            "error: koja.toml `entry = \"{entry}\"` must name a type implementing \
             `Process` (PascalCase). `fn main` entries are no longer supported; \
             use a `.kojs` script for entry-free programs."
        );
        process::exit(1);
    }
    Identifier::new(config.name.clone(), vec![entry.to_string()])
}

/// Collect the project's compiler inputs: the project's own `src`
/// (plus `test` when `include_tests`) and every path dependency's
/// `src`, each tagged with its package. Bails the process on any I/O
/// or dependency-graph error. Stdlib rides in separately via
/// `bundle_*_with_autoimport`, so it is not collected here.
fn collect_project_sources_or_exit(
    config: &ProjectConfig,
    root: &Path,
    include_tests: bool,
) -> Vec<SourceFile> {
    let loaded = ProjectLoader::new(config, root)
        .sources(LoadOptions {
            extensions: &["koja"],
            include_dependencies: true,
            include_stdlib: false,
            include_tests,
            on_error: ErrorPolicy::Strict,
        })
        .unwrap_or_else(|err| {
            eprintln!("error: {err}");
            process::exit(1);
        });
    loaded
        .into_iter()
        .map(|source| SourceFile {
            package: source.package,
            path: source.path,
            source: source.source,
        })
        .collect()
}

/// Default output path for project builds:
/// `<root>/build/{debug,release}/<config.name>` depending on the
/// `release` flag.
fn default_project_output(config: &ProjectConfig, root: &Path, release: bool) -> String {
    project_build_dir(root, release)
        .join(config.binary_name())
        .to_string_lossy()
        .to_string()
}

fn project_build_dir(root: &Path, release: bool) -> PathBuf {
    let profile = if release { "release" } else { "debug" };
    let dir = root.join("build").join(profile);
    fs::create_dir_all(&dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create build directory: {e}");
        process::exit(1);
    });
    dir
}

/// Compile the [`IRProgram`] to an object file and link it into a
/// native binary at `output`. Sibling to [`emit_and_link_script`];
/// the only difference is the IR variant fed into the LLVM
/// backend. `app_name` flows into `__koja_app_name` and
/// `program.link_libraries` becomes the `cc -l<name>` set.
/// `extra_lib_search_paths` lets project-mode callers add the
/// project root to `-L` so a sibling `libfoo.a` resolves without
/// the user setting `LIBRARY_PATH` or invoking `koja` from a
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
    if let Err(err) = koja_ir_llvm::compile_program(
        program,
        app_name,
        Path::new(&object_path),
        &koja_ir_llvm::CompileOptions { release },
    ) {
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
/// [`koja_ast::format_file`], followed by the compact registry
/// sidecar from [`koja_typecheck::format_registry`] so the ids
/// that appear on AST reference sites are decodable without a
/// separate lookup. Mirrors what `koja check --emit-ast` does for the
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
        print!("{}", koja_ast::format_file(file));
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
