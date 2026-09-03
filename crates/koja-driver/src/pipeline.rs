//! `koja {check,shell,build,run,eval,test}` subcommand handlers.
//!
//! Drives the compiler pipeline (`koja-typecheck -> koja-ir ->
//! koja-ir-eval` / `koja-ir-llvm`) for every command that touches
//! a source file or project.
//!
//! Each command carries its own copy of the pipeline driver since
//! they run a single source file and have no REPL state to thread.
//! The REPL itself lives in [`koja_shell`]. `cmd_shell` is just a
//! thin entry point that hands control off to it.
//!
//! ## Mode dispatch
//!
//! Two orthogonal axes drive every command except `shell`:
//!
//! - **Source shape**: `.kojs` (script, parsed [`ParseMode::Script`],
//!   lowered via [`lower_script`]) vs `.koja` (project file, parsed
//!   [`ParseMode::File`], lowered via [`lower_program`]).
//! - **Command verb**: `build` (compile, keep), `run` (execute),
//!   `check` (parse + typecheck only).
//!
//! [`resolve_source_shape`] categorizes the input into one of
//! three [`SourceShape`] variants: `Script(.kojs)`,
//! `Program(.koja standalone)`, or `Project { config, root }`.
//! Each command then decides what to do:
//!
//! | mode      | check                              | run / build                                |
//! |-----------|------------------------------------|--------------------------------------------|
//! | `Script`  | parse Script + check               | full script pipeline                       |
//! | `Program` | parse File + check (LSP-friendly)  | error: `.koja` needs project               |
//! | `Project` | parse + check whole project        | full project pipeline (either backend for `run`, always LLVM for `build`) |
//!
//! `cmd_shell` has no file dimension and bypasses the resolver
//! entirely. REPL fragments are always script-mode. Project mode
//! routes through [`koja_ir::lower_program`] +
//! [`koja_ir_llvm::compile_program`]. The manifest's `entry`
//! field names a PascalCase `Process<C, M, R>` state type, and the
//! lowering synthesizes its entry wrapper.
//!
//! ## Backend selection
//!
//! Only `run` has a backend dimension. It accepts
//! `--backend={interpreter,llvm}` (see [`Backend`]):
//!
//! - `run` defaults to [`Backend::Interpreter`]: lower -> run via
//!   [`Interpreter::run_script`] (scripts, exit 0, and the trailing
//!   expression's value is discarded, so user code calls
//!   `IO.puts` / `value.print()` explicitly for output) or
//!   [`Interpreter::run_program`] (projects, where the Process entry's
//!   exit code becomes the driver's exit status). Fast feedback,
//!   no link step.
//! - `run --backend=llvm`: lower -> [`koja_ir_llvm::compile_script`]
//!   / [`koja_ir_llvm::compile_program`] -> link -> exec the binary
//!   -> forward its exit code.
//! - `build` is always LLVM: lower -> compile -> link -> keep the
//!   binary at the output path. The interpreter has no codegen
//!   surface, so `build` carries no backend flag.
//! - `check` and `shell` have no backend dimension.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use koja_ast::ast::{Diagnostic, Severity};
use koja_ast::identifier::Identifier;
use koja_ir::{IRProgram, IRScript, lower_program, lower_script};
use koja_ir_eval::{Interpreter, RuntimeError, Value};
use koja_ir_llvm::CompileOptions;
use koja_parser::{FileId, ParseMode, ParsedProgram, SourceFile, parse_file, parse_program};
use koja_test::{HARNESS_ENTRY, TestOptions, discover_tests, generate_harness};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program, format_registry};

use crate::commands::{load_project_or_exit, try_load_project};
use crate::diagnostics::{SourceTable, render_program_diagnostics};
use crate::link::{self, LinkOptions};
use crate::loader::{self, ErrorPolicy, LoadOptions, LoadedSource, ProjectLoader};
use crate::project::{self, ProjectConfig};
use crate::tasks::{TASK_HARNESS_ENTRY, TaskProvider, generate_task_harness, resolve_tasks};

/// Which downstream backend a `run` invocation drives.
///
/// `koja run` defaults to [`Backend::Interpreter`] (fast feedback,
/// no link step) and accepts `--backend=llvm` to compile + exec.
/// Any code generation flag also selects `llvm` (see
/// [`resolve_backend`]). `koja build` carries no backend flag: only
/// LLVM emits object files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// Run in-process through the interpreter
    Interpreter,
    /// Compile and link a native binary, run it, and forward its exit code
    Llvm,
}

/// CLI spelling of [`koja_ir_llvm::TargetCpu`]. Lives here so the
/// backend crate stays free of clap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum TargetCpu {
    /// Every instruction the build machine supports.
    Native,
    /// A baseline that runs on any machine of the build architecture.
    #[default]
    Portable,
}

impl From<TargetCpu> for koja_ir_llvm::TargetCpu {
    fn from(target_cpu: TargetCpu) -> Self {
        match target_cpu {
            TargetCpu::Native => Self::Native,
            TargetCpu::Portable => Self::Portable,
        }
    }
}

/// Flags that shape the emitted machine code, shared by every command
/// that drives the LLVM backend. Flattened into [`BuildOptions`] and
/// [`RunOptions`] so a new knob lands once and reads the same in
/// both `--help` outputs.
#[derive(Clone, Copy, Debug, Default, clap::Args)]
pub(crate) struct CodegenArgs {
    /// Build with aggressive optimizations
    #[arg(long, help_heading = CODEGEN_HEADING)]
    pub(crate) release: bool,

    /// CPU the binary may assume (defaults to `portable`). `portable` runs on any machine of the build architecture, `native` uses every instruction the build machine supports
    #[arg(long, value_enum, help_heading = CODEGEN_HEADING)]
    pub(crate) target_cpu: Option<TargetCpu>,
}

/// `--help` section for [`CodegenArgs`]. Set per arg rather than
/// through `next_help_heading`, which would leak onto whatever the
/// parent declares after the flatten.
const CODEGEN_HEADING: &str = "Code generation";

impl CodegenArgs {
    fn compile_options(self) -> CompileOptions {
        CompileOptions {
            release: self.release,
            target_cpu: self.target_cpu.unwrap_or_default().into(),
        }
    }

    /// The first flag the user passed explicitly, if any. Asking for
    /// code generation is how `koja run` learns the user wants the
    /// compiled path.
    fn explicit_flag(self) -> Option<&'static str> {
        if self.release {
            return Some("--release");
        }
        self.target_cpu.map(|_| "--target-cpu")
    }
}

#[derive(clap::Args)]
pub(crate) struct BuildOptions {
    #[command(flatten)]
    pub(crate) codegen: CodegenArgs,

    /// Print LLVM IR to stdout instead of producing a binary
    #[arg(long)]
    pub(crate) emit_llvm: bool,

    /// Source file (`.koja` / `.kojs`, omit to use `koja.toml`)
    pub(crate) file: Option<String>,

    /// Output binary name
    #[arg(short, long)]
    pub(crate) output: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct RunOptions {
    /// Arguments passed to the compiled program
    #[arg(index = 2, last = true)]
    pub(crate) args: Vec<String>,

    /// Execution backend. `interpreter` runs in-process for fast startup, while `llvm` compiles to a native binary, runs it, and forwards its exit code. Defaults to `interpreter`, or `llvm` when any code generation flag is passed
    #[arg(long, value_enum)]
    pub(crate) backend: Option<Backend>,

    #[command(flatten)]
    pub(crate) codegen: CodegenArgs,

    /// Source file (`.koja` / `.kojs`), task name (`postgres.migrate`),
    /// or omit to run the project's entry via `koja.toml`
    #[arg(index = 1)]
    pub(crate) file: Option<String>,
}

impl RunOptions {
    /// Interpreter-backed invocation of `file` with `args`, for the
    /// `eval` and `new` aliases in `main.rs`.
    pub(crate) fn interpreted(file: String, args: Vec<String>) -> Self {
        Self {
            args,
            backend: Some(Backend::Interpreter),
            codegen: CodegenArgs::default(),
            file: Some(file),
        }
    }
}

/// Pick the `run` backend. `--backend` wins when given. Otherwise a
/// code generation flag selects the compiled path, since asking for
/// `--release` is asking for the compiler, and a bare `koja run`
/// interprets. A code generation flag on an explicit interpreter run
/// is an error rather than a silent no-op.
fn resolve_backend(explicit: Option<Backend>, codegen: CodegenArgs) -> Backend {
    match (explicit, codegen.explicit_flag()) {
        (Some(Backend::Interpreter), Some(flag)) => {
            eprintln!(
                "error: `{flag}` has no effect on the interpreter. Drop it or pass `--backend=llvm`."
            );
            process::exit(1);
        }
        (Some(backend), _) => backend,
        (None, Some(_)) => Backend::Llvm,
        (None, None) => Backend::Interpreter,
    }
}

/// Categorized source input for a `koja` command.
///
/// [`resolve_source_shape`] inspects the file extension (or, with
/// no file, the current directory's `koja.toml`) and produces one
/// of these variants. `cmd_check` accepts all three. `cmd_build`
/// and `cmd_run` reject `Program`, since a bare `.koja` file has
/// no entry point or dependency graph.
enum SourceShape {
    /// Standalone script (`.kojs`). Top-level expressions are
    /// first-class, lowered via [`lower_script`].
    Script(PathBuf),
    /// Project file (`.koja`) provided directly.
    Program(PathBuf),
    /// No file argument, `koja.toml` found in the current
    /// directory. Carries the parsed [`ProjectConfig`] and the
    /// project root so handlers need not re-load the manifest.
    Project {
        config: Box<ProjectConfig>,
        root: PathBuf,
    },
}

/// Categorize the user's input into a [`SourceShape`]. Errors are
/// returned as `Err(message)` for the caller to print and exit
/// non-zero.
fn resolve_source_shape(
    file: Option<&str>,
    project_root: Option<&Path>,
) -> Result<SourceShape, String> {
    if let Some(arg) = file {
        if project_root.is_some() {
            return Err("`--project` cannot be used with an explicit source file".into());
        }
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
    let root = match project_root {
        Some(root) => root.to_path_buf(),
        None => env::current_dir()
            .map_err(|err| format!("cannot determine current directory: {err}"))?,
    };
    match project::load_project(&root).map_err(|err| err.to_string())? {
        Some(config) => Ok(SourceShape::Project {
            config: Box::new(config),
            root,
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

/// `koja check [file]`: parse and typecheck a file or project.
/// Prints `<path>: OK` on success, or the collected diagnostics on
/// failure (exit 1). When `emit_ast` is set, prints the sealed AST
/// in [`koja_ast::format_file`]'s compact tree format instead.
///
/// This is the only command that accepts a standalone `.koja` file
/// (parsed in [`ParseMode::File`]): typecheck needs no project
/// context, and LSP/editor flows lean on this.
pub fn cmd_check(project_root: Option<&Path>, file: Option<String>, emit_ast: bool) {
    let mode = resolve_source_shape(file.as_deref(), project_root)
        .unwrap_or_else(|err| bail_resolve_error(err));
    match mode {
        SourceShape::Script(path) => check_single_file(&path, ParseMode::Script, emit_ast),
        SourceShape::Program(path) => check_single_file(&path, ParseMode::File, emit_ast),
        SourceShape::Project { config, root } => check_project(&config, &root, emit_ast),
    }
}

/// `koja shell`: interactive REPL on top of the
/// pipeline. REPL fragments have no file dimension and are always
/// script-mode, so this command bypasses the resolver and the
/// `--backend` flag entirely (the REPL is interpreter-only by
/// design). Delegates to [`koja_shell::run`]. The REPL
/// crate owns session state, multiline detection, command
/// parsing, and its own pipeline driver.
pub fn cmd_shell(project_root: Option<&Path>) {
    let ShellSession {
        baseline,
        session_package,
    } = shell_session(project_root);
    koja_shell::run(baseline, session_package);
}

/// What the REPL evaluates against: the baseline sources plus the
/// package the session source belongs to (the project's package in a
/// project, so its modules resolve unqualified, otherwise `REPL`).
struct ShellSession {
    baseline: Vec<SourceFile>,
    session_package: String,
}

/// Resolve the REPL session. In a project, [`ProjectLoader`] supplies
/// stdlib + project + dependency sources and the session adopts the
/// project's package. With no readable `koja.toml` (or on any load
/// failure) fall back to a stdlib-only `REPL` session. A malformed
/// manifest or broken dependency warns but never aborts the shell.
fn shell_session(project_root: Option<&Path>) -> ShellSession {
    let root = match project_root {
        Some(root) => root.to_path_buf(),
        None => {
            let Ok(cwd) = env::current_dir() else {
                return stdlib_session();
            };
            cwd
        }
    };
    let config = match project::load_project(&root) {
        Ok(Some(config)) => config,
        Ok(None) => return stdlib_session(),
        Err(err) => {
            eprintln!("warning: ignoring koja.toml: {err}");
            return stdlib_session();
        }
    };
    println!("loading project `{}`", config.name);
    match ProjectLoader::new(&config, &root).sources(LoadOptions {
        extensions: &["koja"],
        include_dependencies: true,
        include_stdlib: true,
        include_tests: false,
        on_error: ErrorPolicy::Lenient,
    }) {
        Ok(sources) => ShellSession {
            baseline: sources.into_iter().map(into_source_file).collect(),
            session_package: config.namespace(),
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

/// `koja build [file] [-o output]`: produce a native binary for a
/// `.kojs` script or a project. LLVM is the only backend that emits
/// object files, so `build` has no backend dimension.
/// `-o`/`--output` overrides the default stem-based output name.
pub fn cmd_build(project_root: Option<&Path>, options: BuildOptions) {
    let BuildOptions {
        codegen,
        emit_llvm,
        file,
        output,
    } = options;
    let compile = codegen.compile_options();
    let mode = resolve_source_shape(file.as_deref(), project_root)
        .unwrap_or_else(|err| bail_resolve_error(err));
    match mode {
        SourceShape::Script(path) => build_and_keep(&path, output, compile, emit_llvm),
        SourceShape::Program(path) => bail_program_execution(&path),
        SourceShape::Project { config, root } => {
            build_project_and_keep(&config, &root, output, compile, emit_llvm)
        }
    }
}

/// `koja run [file] [--backend=interpreter|llvm] [-- args...]`:
/// execute a `.kojs` script or a project through the chosen
/// backend.
///
/// `--backend` resolves through [`resolve_backend`]. Scripts:
/// parse Script -> check -> [`lower_script`] ->
/// [`Interpreter::run_script`], exiting 0 on success and 1 on any
/// pipeline failure. Projects: collect -> parse -> check ->
/// [`lower_program`] -> [`Interpreter::run_program`] (with `args`
/// as the argv-shaped config), where the Process entry's exit code
/// becomes the driver's exit status. [`Backend::Llvm`] takes the
/// compiled path: lower -> compile -> link -> exec the binary
/// (forwarding `args`) -> forward its exit code. Script binaries
/// are temp files removed after the run.
pub fn cmd_run(project_root: Option<&Path>, options: RunOptions) {
    let RunOptions {
        args,
        backend,
        codegen,
        file,
    } = options;
    let backend = resolve_backend(backend, codegen);
    let compile = codegen.compile_options();
    if let Some(task_name) = file.as_deref().filter(|arg| looks_like_task_name(arg)) {
        run_task(task_name, project_root, backend, compile, &args);
    }
    let mode = resolve_source_shape(file.as_deref(), project_root)
        .unwrap_or_else(|err| bail_resolve_error(err));
    match (mode, backend) {
        (SourceShape::Script(path), Backend::Interpreter) => run_script_interpreted(&path),
        (SourceShape::Script(path), Backend::Llvm) => run_script_compiled(&path, compile, &args),
        (SourceShape::Program(path), Backend::Interpreter)
        | (SourceShape::Program(path), Backend::Llvm) => bail_program_execution(&path),
        (SourceShape::Project { config, root }, Backend::Interpreter) => {
            run_project_interpreted(&config, &root, &args)
        }
        (SourceShape::Project { config, root }, Backend::Llvm) => {
            run_project_compiled(&config, &root, compile, &args)
        }
    }
}

/// `koja test`: discover `@test`-annotated functions in the
/// current project, synthesize a Process-shaped harness type,
/// lower the whole thing through the pipeline, link via LLVM, and
/// exec the resulting binary so its exit code surfaces test
/// success/failure.
///
/// Requires an `koja.toml` in the current directory. Walks
/// `config.src` AND `config.test` for the project itself, while
/// deps contribute only `src`. Autoimport is suppressed when the
/// project IS `Global`, since lib/global/src already provides the
/// stdlib roots and a second copy would collide at registration
/// time.
///
pub fn cmd_test(project_root: Option<&Path>, trace: bool, color: bool) {
    let (config, root) = load_project_or_exit(
        project_root,
        &[
            "error: no koja.toml found",
            "Usage: koja test (run from a directory containing koja.toml)",
        ],
    );
    run_project_tests(&config, &root, TestOptions { color, trace });
}

/// `koja tasks`: list every task name in scope. Inside a project
/// that's the project's + dependencies' + the toolchain's. Outside
/// one, the toolchain's only.
pub fn cmd_tasks(project_root: Option<&Path>) {
    let project = try_load_project(project_root);
    let tasks = resolve_tasks(
        project
            .as_ref()
            .map(|(config, root)| (config, root.as_path())),
    )
    .unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });
    if tasks.is_empty() {
        println!("no tasks defined");
        return;
    }
    for name in tasks.keys() {
        println!("{name}");
    }
}

/// Whether a `koja run` argument names a task rather than a file. It
/// must be dotted (task names are package-prefixed), not carry a
/// source extension, and not exist on disk (an on-disk path always
/// wins).
fn looks_like_task_name(arg: &str) -> bool {
    arg.contains('.')
        && !arg.contains('/')
        && !arg.ends_with(".koja")
        && !arg.ends_with(".kojs")
        && !Path::new(arg).exists()
}

/// `koja run <task.name>`: resolve the task, synthesize its harness,
/// and execute through the chosen backend. Diverges either way.
///
/// Works without a `koja.toml`. Projectless invocations see the
/// toolchain's tasks only (so `koja new` runs anywhere). Toolchain
/// tasks compile against a stdlib-only bundle even inside a project,
/// since scaffolding must not require the surrounding project to
/// build.
fn run_task(
    name: &str,
    project_root: Option<&Path>,
    backend: Backend,
    compile: CompileOptions,
    args: &[String],
) -> ! {
    let project = try_load_project(project_root);
    let tasks = resolve_tasks(
        project
            .as_ref()
            .map(|(config, root)| (config, root.as_path())),
    )
    .unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });
    let Some(provider) = tasks.get(name) else {
        eprintln!("error: no task named `{name}` (see `koja tasks`)");
        process::exit(1);
    };

    let program = if provider.toolchain {
        lower_task_harness(bundle_many_with_autoimport(Vec::new(), None), provider)
    } else {
        let (config, root) = project
            .as_ref()
            .expect("non-toolchain tasks only resolve inside a project");
        build_task_program(config, root, name, provider)
    };
    match backend {
        Backend::Interpreter => interpret_program(&program, args),
        Backend::Llvm => {
            run_task_compiled(&program, name, provider, project.as_ref(), compile, args)
        }
    }
}

/// LLVM leg of [`run_task`]. Project/dep tasks link into the project
/// build dir and stay cached, while toolchain tasks link into a temp
/// binary removed after the run (there may be no project to cache
/// under).
fn run_task_compiled(
    program: &IRProgram,
    name: &str,
    provider: &TaskProvider,
    project: Option<&(ProjectConfig, PathBuf)>,
    compile: CompileOptions,
    args: &[String],
) -> ! {
    let binary_stem = format!("task_{}", name.replace('.', "_"));
    let (binary, app_name, link_roots, remove_after) = match project.filter(|_| !provider.toolchain)
    {
        Some((config, root)) => (
            project_build_dir(root, compile.release).join(binary_stem),
            config.name.as_str(),
            vec![root.as_path()],
            false,
        ),
        None => (
            temp_binary_path(&binary_stem),
            provider.package.as_str(),
            Vec::new(),
            true,
        ),
    };
    let binary = binary.to_string_lossy().to_string();
    emit_and_link_program(program, app_name, &binary, &link_roots, compile);
    exec_binary(&binary, args, remove_after)
}

/// Drive the project pipeline with the task harness spliced into the
/// provider's package and [`TASK_HARNESS_ENTRY`] as the entry. Bails
/// the process on any failure.
///
/// Checks twice: first the project as written, so a bad `[tasks]`
/// entry (missing type, no `Koja.Task` impl) surfaces as a task error
/// against a clean program instead of a typecheck failure inside the
/// synthesized harness, then the program with the harness spliced in.
/// Toolchain tasks skip the pre-pass (the stdlib is trusted) and go
/// straight through [`lower_task_harness`].
fn build_task_program(
    config: &ProjectConfig,
    root: &Path,
    task_name: &str,
    provider: &TaskProvider,
) -> IRProgram {
    let user_files = collect_project_sources_or_exit(config, root, false);
    let bundled = bundle_many_with_autoimport(user_files, Some(&config.namespace()));

    let checked = check_bundle(bundled.clone(), ParseMode::File);
    check_task_conformance(&checked, task_name, provider);

    lower_task_harness(bundled, provider)
}

/// Splice the task harness into the provider's package, check, and
/// lower with [`TASK_HARNESS_ENTRY`] as the entry. Bails the process
/// on any failure.
fn lower_task_harness(bundled: Vec<SourceFile>, provider: &TaskProvider) -> IRProgram {
    let mut parsed = parse_program(bundled, ParseMode::File);
    splice_generated_source(
        &mut parsed,
        provider.namespace.clone(),
        "__task_harness__",
        generate_task_harness(&provider.type_name),
    );
    let sources = capture_sources(&parsed);
    let checked =
        check_program(parsed).unwrap_or_else(|failure| bail_check_failure(failure, &sources));

    let entry = Identifier::new(
        provider.namespace.clone(),
        vec![TASK_HARNESS_ENTRY.to_string()],
    );
    match lower_program(&checked, &entry) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("error: {err}");
            process::exit(1);
        }
    }
}

/// Verify the manifest-declared task type exists and implements
/// `Koja.Task`, so a bad `[tasks]` entry reads as a task error rather
/// than a typecheck failure inside the synthesized harness.
fn check_task_conformance(checked: &CheckedProgram, task_name: &str, provider: &TaskProvider) {
    let type_id = Identifier::new(
        provider.namespace.clone(),
        provider.type_name.split('.').map(String::from).collect(),
    );
    let Some((target_id, _)) = checked.registry.lookup(&type_id) else {
        eprintln!(
            "error: task `{task_name}` names type `{}`, which does not exist in package `{}`",
            provider.type_name, provider.package
        );
        process::exit(1);
    };
    let protocol_id = Identifier::new("Koja".to_string(), vec!["Task".to_string()]);
    let Some((protocol_id, _)) = checked.registry.lookup(&protocol_id) else {
        eprintln!("internal error: stdlib protocol `Koja.Task` is not registered");
        process::exit(1);
    };
    if !checked.registry.conforms_any(target_id, protocol_id) {
        eprintln!(
            "error: task `{task_name}` names type `{}.{}`, which does not implement `Koja.Task`",
            provider.namespace, provider.type_name
        );
        process::exit(1);
    }
}

/// Build the `.kojs` script at `path` through LLVM and keep the
/// resulting binary at `output` (or a stem-derived default). Used
/// by `cmd_build` when the user picks the LLVM backend. When
/// `emit_llvm` is set, print the textual LLVM IR to stdout and
/// short-circuit before linking. No `.o`, no binary.
fn build_and_keep(path: &Path, output: Option<String>, compile: CompileOptions, emit_llvm: bool) {
    let script = build_script(path);
    let app_name = derive_package(path);
    if emit_llvm {
        print_script_ir(&script, &app_name);
        return;
    }
    let output = resolve_output_name(output, path);
    emit_and_link_script(&script, &app_name, &output, compile);
    println!("compiled: {output}");
}

/// Build the `.kojs` script at `path` into a temp binary, exec
/// it with `args`, forward the exit code, and remove the temp
/// binary. Diverges either way (binary status or launch error).
fn run_script_compiled(path: &Path, compile: CompileOptions, args: &[String]) -> ! {
    let script = build_script(path);
    let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or("app");
    let output = temp_binary_path(stem).to_string_lossy().to_string();
    emit_and_link_script(&script, &derive_package(path), &output, compile);
    exec_binary(&output, args, true)
}

/// Run the `.kojs` script at `path` through the interpreter and
/// discard the trailing value. Scripts always exit 0 on normal
/// completion, matching the LLVM backend's `main` trampoline (see
/// `koja-ir-llvm/src/main_wrapper.rs`). Runtime failures print
/// `error: …` and exit 1.
fn run_script_interpreted(path: &Path) {
    let script = build_script(path);
    if let Err(error) = Interpreter::run_script(&script) {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

/// Typecheck a single source file in the requested parse mode.
/// Shared by the `Script` and `Program` arms of `cmd_check`.
fn check_single_file(path: &Path, mode: ParseMode, emit_ast: bool) {
    let (checked, _) = read_and_check(path, mode);
    if emit_ast {
        emit_checked_ast(&checked);
    } else {
        println!("{}: OK", path.display());
    }
}

/// Wrap one user-supplied [`SourceFile`] with the embedded stdlib
/// (auto-import plus qualified packages) so every pipeline feeds
/// the parser the same compilation unit. Stdlib sources lead so
/// the registry sees their declarations before any user code that
/// references them. Single-file callers never declare project
/// membership, hence `skip_package: None`.
fn bundle_with_autoimport(user: SourceFile) -> Vec<SourceFile> {
    bundle_many_with_autoimport(vec![user], None)
}

/// Multi-file counterpart to [`bundle_with_autoimport`] for
/// project mode, where `user_files` already merges project and
/// dependency sources.
///
/// `skip_package` handles the stdlib self-compile: when the project
/// IS an embedded package (building or testing `lib/global`,
/// `lib/json`, …) the on-disk sources already provide every decl
/// the autoimport would inject, and a second copy would collide at
/// registry seal time.
fn bundle_many_with_autoimport(
    user_files: Vec<SourceFile>,
    skip_package: Option<&str>,
) -> Vec<SourceFile> {
    let mut sources = koja_stdlib::autoimport_sources();
    // Qualified stdlib packages (Crypto, HTTP, JSON, Net, …)
    // ship pre-baked against the published Global. Loading them
    // when the user IS compiling Global self-imports an
    // inconsistent pair. The user's edited `lib/global/src` would
    // co-exist with qualified packages typechecked against the
    // older baked Global, and protocol-impl resolution gets
    // confused (e.g. HTTP's `format`/`equals?` calls fail to see the
    // user's edited `Global` protocol impls because the qualified
    // packages were lifted before user files joined the bundle).
    // Qualified deps don't tag along on a Global self-compile.
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
/// [`IRScript`] on success. Bails the process on any pipeline
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

/// Read, bundle, parse, and typecheck one source file. Returns the
/// sealed [`CheckedProgram`] and the derived package name. Bails
/// the process on read / parse / typecheck failures.
fn read_and_check(path: &Path, mode: ParseMode) -> (CheckedProgram, String) {
    let source = read_source_or_exit(path);
    let package = derive_package(path);
    let checked = check_bundle(
        bundle_with_autoimport(SourceFile {
            package: package.clone(),
            path: path.to_path_buf(),
            source,
        }),
        mode,
    );
    (checked, package)
}

/// Parse and typecheck a bundled compilation unit, printing any
/// warnings. Bails the process with rendered diagnostics on
/// failure.
fn check_bundle(bundled: Vec<SourceFile>, mode: ParseMode) -> CheckedProgram {
    check_parsed(parse_program(bundled, mode))
}

/// [`check_bundle`] for an already-parsed program, for callers that
/// splice generated sources between parse and check.
fn check_parsed(parsed: ParsedProgram) -> CheckedProgram {
    let sources = capture_sources(&parsed);
    let checked =
        check_program(parsed).unwrap_or_else(|failure| bail_check_failure(failure, &sources));
    print_check_warnings(&checked, &sources);
    checked
}

/// Read a source file or bail with `error: cannot read …`. Used by
/// every command that opens a file directly. The `build_*` family
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

/// Render the sealed [`IRScript`] as LLVM IR text on stdout. Backs
/// `koja build --emit-llvm` for scripts: the same module the
/// compiled `.o` would carry, minus object emission.
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

/// Compile the [`IRScript`] to an object file and link it into a
/// native binary at `output`. `app_name` flows into the binary's
/// `__koja_app_name` global (panic backtrace label) and
/// `script.link_libraries` becomes the `cc -l<name>` set.
fn emit_and_link_script(script: &IRScript, app_name: &str, output: &str, compile: CompileOptions) {
    let object_path = format!("{output}.o");
    if let Err(err) =
        koja_ir_llvm::compile_script(script, app_name, Path::new(&object_path), &compile)
    {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(
        &object_path,
        output,
        &script.link_libraries,
        &[],
        compile.release,
    );
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

/// Canonicalize a user-supplied source path, exiting with a clear
/// error message when the file does not exist.
fn canonical_source_path(file: &str) -> PathBuf {
    Path::new(file).canonicalize().unwrap_or_else(|_| {
        eprintln!("error: file not found: {file}");
        process::exit(1);
    })
}

/// Pick the output binary name. Honors a user-supplied `--output`,
/// otherwise drops the source extension to derive the binary name.
fn resolve_output_name(output: Option<String>, path: &Path) -> String {
    output.unwrap_or_else(|| {
        path.file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("app")
            .to_string()
    })
}

/// `koja check` for a project: walk the project's `src` and `test`
/// directories, resolve declared dependencies, parse + typecheck the
/// whole set, and print `<project>: OK` (or per-file ASTs when
/// `emit_ast` is set).
fn check_project(config: &ProjectConfig, root: &Path, emit_ast: bool) {
    let user_files = collect_project_sources_or_exit(config, root, true);
    let checked = check_bundle(
        bundle_many_with_autoimport(user_files, Some(&config.namespace())),
        ParseMode::File,
    );
    if emit_ast {
        emit_checked_ast(&checked);
    } else {
        println!("{}: OK", config.name);
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
    compile: CompileOptions,
    emit_llvm: bool,
) {
    let program = build_project_program(config, root);
    if emit_llvm {
        print_program_ir(&program, &config.name);
        return;
    }
    let output = match output {
        Some(o) => o,
        None => default_project_output(config, root, compile.release),
    };
    emit_and_link_program(&program, &config.name, &output, &[root], compile);
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
/// Parse errors bail before discovery so a broken only-test-file
/// reports its diagnostics instead of reading as "no tests found".
fn run_project_tests(config: &ProjectConfig, root: &Path, opts: TestOptions) {
    let namespace = config.namespace();
    let user_files = collect_project_sources_or_exit(config, root, true);
    let bundled = bundle_many_with_autoimport(user_files, Some(&namespace));
    let mut parsed = parse_program(bundled, ParseMode::File);
    if parsed.has_errors() {
        let sources = capture_sources(&parsed);
        eprintln!(
            "{}",
            render_program_diagnostics(&parse_diagnostics(&parsed), &sources)
        );
        process::exit(1);
    }

    let tests = discover_tests(&parsed, &namespace, root);
    if tests.is_empty() {
        println!("no tests found");
        return;
    }

    splice_generated_source(
        &mut parsed,
        namespace.clone(),
        "__test_harness__",
        generate_harness(&tests, opts),
    );

    let checked = check_parsed(parsed);
    let entry = Identifier::new(namespace, vec![HARNESS_ENTRY.to_string()]);
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
    emit_and_link_program(
        &program,
        &config.name,
        &binary,
        &[root],
        CompileOptions::default(),
    );

    // Trace runs are meant for interactive debugging (and the
    // per-binary timeout would kill a long diagnostic session), so
    // skip the deadline there, matching `mix test --trace`.
    let timeout = (!opts.trace).then_some(TEST_BINARY_TIMEOUT);
    let status = run_test_binary_with_timeout(&binary, timeout);
    let _ = fs::remove_file(&binary);

    match status {
        TestBinaryOutcome::Exited(code) => process::exit(code),
        TestBinaryOutcome::LaunchFailed(err) => {
            eprintln!("error: failed to exec `{binary}`: {err}");
            process::exit(1);
        }
        TestBinaryOutcome::Signaled(signal) => {
            let name = signal_name(signal).map_or_else(String::new, |n| format!(" ({n})"));
            eprintln!("error: test binary terminated by signal {signal}{name}");
            process::exit(1);
        }
        TestBinaryOutcome::TimedOut => {
            eprintln!(
                "error: test binary `{binary}` exceeded {}s timeout and was killed",
                TEST_BINARY_TIMEOUT.as_secs(),
            );
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
    Signaled(i32),
    TimedOut,
}

/// Names for the fatal signals a test binary plausibly dies from, so a
/// runtime crash reads as `signal 11 (SIGSEGV)` rather than a bare number.
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        2 => "SIGINT",
        4 => "SIGILL",
        6 => "SIGABRT",
        #[cfg(target_os = "linux")]
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        #[cfg(target_os = "macos")]
        10 => "SIGBUS",
        11 => "SIGSEGV",
        15 => "SIGTERM",
        _ => return None,
    })
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
            Ok(Some(status)) => {
                return match status.code() {
                    Some(code) => TestBinaryOutcome::Exited(code),
                    // A killed child has no exit code. Surface the signal so
                    // a runtime crash can't masquerade as a plain failure.
                    None => TestBinaryOutcome::Signaled(status.signal().unwrap_or(0)),
                };
            }
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

/// Parse a driver-generated source and splice it into `parsed` under
/// a synthetic `<package.tag>` path. Bails the process on a
/// parse-time diagnostic. Generated sources must always parse
/// cleanly. Shared by the test and task harness paths.
fn splice_generated_source(parsed: &mut ParsedProgram, package: String, tag: &str, source: String) {
    let path = PathBuf::from(format!("<{package}.{tag}>"));
    let file = FileId(parsed.order.len() as u32);
    let generated = parse_file(
        SourceFile {
            package,
            path: path.clone(),
            source,
        },
        ParseMode::File,
        file,
    );
    if !generated.diagnostics.is_empty() {
        eprintln!("internal error: generated {tag} source failed to parse");
        for diag in &generated.diagnostics {
            eprintln!("  {}", diag.message);
        }
        process::exit(1);
    }
    parsed.order.push(path.clone());
    parsed.files.insert(path, generated);
}

/// `koja run` for a project under the interpreter: lower the full
/// project and execute the Process entry in-process, no codegen or
/// link. Features the interpreter does not cover yet surface a
/// runtime error plus a `--backend=llvm` hint. Diverges either way.
fn run_project_interpreted(config: &ProjectConfig, root: &Path, args: &[String]) -> ! {
    let program = build_project_program(config, root);
    interpret_program(&program, args)
}

/// Execute a lowered [`IRProgram`]'s Process entry in-process via
/// [`Interpreter::run_program`] and exit with its code. Shared by the
/// project and task interpreter paths.
fn interpret_program(program: &IRProgram, args: &[String]) -> ! {
    match Interpreter::run_program(program, args) {
        Ok(Value::Int(code)) => process::exit(code as i32),
        Ok(other) => {
            eprintln!("error: process entry returned non-integer exit value `{other}`");
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            if matches!(error, RuntimeError::Unsupported { .. }) {
                eprintln!(
                    "hint: this program uses process features the interpreter does not \
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
fn run_project_compiled(
    config: &ProjectConfig,
    root: &Path,
    compile: CompileOptions,
    args: &[String],
) -> ! {
    let program = build_project_program(config, root);
    let build_dir = project_build_dir(root, compile.release);
    let binary = build_dir
        .join(config.binary_name())
        .to_string_lossy()
        .to_string();
    emit_and_link_program(&program, &config.name, &binary, &[root], compile);
    exec_binary(&binary, args, false)
}

/// Exec a freshly linked binary with `args`, forward its exit code,
/// and optionally remove it afterwards. Diverges either way.
/// Path for a `koja run` temp binary: `$TMPDIR/koja-run/<pid>-<stem>`.
/// [`exec_binary`] removes it after the child exits, but the file
/// leaks when the driver dies first (Ctrl-C, a test harness kill).
/// Sweeping the shared directory on the way in makes those leaks
/// self-healing.
fn temp_binary_path(stem: &str) -> PathBuf {
    let dir = env::temp_dir().join("koja-run");
    let _ = fs::create_dir_all(&dir);
    sweep_stale_binaries(&dir);
    dir.join(format!("{}-{stem}", process::id()))
}

/// Remove entries in `dir` untouched for over a day. Directories
/// (macOS `.dSYM` bundles) remove recursively. Best-effort hygiene:
/// every error is ignored.
fn sweep_stale_binaries(dir: &Path) {
    const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > MAX_AGE);
        if !stale {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

fn exec_binary(binary: &str, args: &[String], remove_after: bool) -> ! {
    let status = process::Command::new(binary).args(args).status();
    if remove_after {
        let _ = fs::remove_file(binary);
        // macOS emits a debug-symbol bundle next to the binary.
        let _ = fs::remove_dir_all(format!("{binary}.dSYM"));
    }
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
    let checked = check_bundle(
        bundle_many_with_autoimport(user_files, Some(&config.namespace())),
        ParseMode::File,
    );
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
/// field names a PascalCase type implementing `Process<C, M, R>`,
/// and `lower_program` synthesizes the entry wrapper for it. Lowercase
/// (function-shaped) entries are rejected here, since `fn main` is no
/// longer an entry point.
fn resolve_project_entry(config: &ProjectConfig) -> Identifier {
    let entry = config.entry.as_deref().unwrap_or_else(|| {
        eprintln!("error: koja.toml has no `entry` field (required for build/run)");
        process::exit(1);
    });
    if config.entry_type_name().is_none() {
        eprintln!(
            "error: koja.toml `entry = \"{entry}\"` must name a type implementing \
             `Process` (PascalCase). `fn main` entries are no longer supported. \
             Use a `.kojs` script for entry-free programs."
        );
        process::exit(1);
    }
    Identifier::new(config.namespace(), vec![entry.to_string()])
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
    loaded.into_iter().map(into_source_file).collect()
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
/// native binary at `output`. Sibling to [`emit_and_link_script`].
/// The only difference is the IR variant fed into the LLVM
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
    compile: CompileOptions,
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
    if let Err(err) =
        koja_ir_llvm::compile_program(program, app_name, Path::new(&object_path), &compile)
    {
        eprintln!("error: {err}");
        process::exit(1);
    }
    link_object(
        &object_path,
        output,
        &program.link_libraries,
        extra_lib_search_paths,
        compile.release,
    );
}

/// Prints every file in the sealed program to stdout using
/// [`koja_ast::format_file`], followed by the compact registry
/// sidecar from [`koja_typecheck::format_registry`] so the ids
/// that appear on AST reference sites are decodable without a
/// separate lookup.
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
/// `App` when the path has no usable stem. User-facing files always
/// have a stem in practice.
fn derive_package(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("App")
        .to_string()
}

/// Print the warning-severity diagnostics riding a successful check
/// (deprecation notices, match reachability) to stderr. Every
/// command that continues past `check_program` calls this so
/// warnings surface regardless of how the compile is invoked.
fn print_check_warnings(checked: &CheckedProgram, sources: &SourceTable) {
    let warnings: Vec<Diagnostic> = checked
        .diagnostics
        .iter()
        .filter(|diag| diag.severity == Severity::Warning)
        .cloned()
        .collect();
    if !warnings.is_empty() {
        eprintln!("{}", render_program_diagnostics(&warnings, sources));
    }
}

/// Snapshot every parsed file's source before `check_program`
/// consumes the parse, for snippet rendering. `CheckFailure::partial`
/// is rebuilt without sources on the typecheck-failure path, so the
/// driver keeps its own copy, indexed by [`koja_parser::FileId`].
fn capture_sources(parsed: &ParsedProgram) -> SourceTable {
    SourceTable::new(
        parsed
            .order
            .iter()
            .map(|path| (path.clone(), parsed.files[path].source.clone()))
            .collect(),
    )
}

/// Collect every diagnostic the parse produced, across all files.
fn parse_diagnostics(parsed: &ParsedProgram) -> Vec<Diagnostic> {
    parsed
        .files
        .values()
        .flat_map(|file| file.diagnostics.iter())
        .cloned()
        .collect()
}

/// Render a [`CheckFailure`]'s diagnostics to stderr and exit 1.
/// Parse diagnostics live on the partial parse, not on
/// `failure.diagnostics`, so both sets print.
fn bail_check_failure(failure: CheckFailure, sources: &SourceTable) -> ! {
    let CheckFailure {
        diagnostics,
        partial,
        ..
    } = failure;
    let mut all = parse_diagnostics(&partial);
    all.extend(diagnostics);
    if all.is_empty() {
        eprintln!("error: check failed with no diagnostics");
    } else {
        eprintln!("{}", render_program_diagnostics(&all, sources));
    }
    process::exit(1);
}
