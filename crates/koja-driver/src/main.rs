//! Koja compiler CLI entry point.
//!
//! Parses the top-level subcommand and dispatches each one through
//! either [`pipeline`] (the compiler pipeline: `koja-typecheck ->
//! koja-ir -> koja-ir-llvm` / `koja-ir-eval`) or
//! [`commands`] (frontend / filesystem tooling: `parse`, `lex`,
//! `format`, `doc`). `koja new` is an alias for the self-hosted
//! `koja.new` toolchain task and rides the pipeline's task runner.
//!
//! Source dispatch follows [`pipeline::cmd_build`]'s extension
//! rules: `.kojs` files are scripts (top-level expressions, no
//! project context), while `.koja` files are project files. Omitting the
//! file argument falls back to discovering an `koja.toml` in the
//! current directory. Project mode runs the full pipeline through
//! [`koja_ir_llvm::compile_program`] (`build`, `run
//! --backend=llvm`) or [`koja_ir_eval::Interpreter`] (`run`).
//!
//! Backend selection: only `run` has a backend dimension. It
//! accepts `--backend={interpreter,llvm}` (see [`pipeline::Backend`])
//! and defaults to `interpreter` (fast feedback, no link step).
//! `build` is always LLVM (the only backend that produces a
//! binary), so it carries no backend flag.

mod commands;
mod deps;
mod diagnostics;
mod link;
mod loader;
mod pipeline;
pub mod project;
mod serve;
mod tasks;

use std::path::{Path, PathBuf};
use std::process;

use koja_runtime as _;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "koja", version, about = "The Koja language compiler")]
#[command(next_help_heading = "Global options")]
struct Cli {
    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    /// Diagnostic output format (defaults to `pretty` on terminals
    /// and `short` when stderr is piped, `KOJA_DIAGNOSTICS` overrides
    /// the detection)
    #[arg(long, global = true, value_enum)]
    diagnostics: Option<diagnostics::DiagnosticFormat>,

    /// Project directory to use instead of the current directory
    #[arg(short = 'S', long, global = true, value_name = "DIRECTORY")]
    project: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a source file or project to a native binary
    Build(pipeline::BuildOptions),
    /// Type-check a source file or project without compiling
    Check {
        /// Source file (`.koja` / `.kojs`, omit to use `koja.toml`)
        file: Option<String>,

        /// Print the type-checked AST to stdout instead of just OK/diagnostics
        #[arg(long)]
        emit_ast: bool,
    },
    /// Manage project dependencies (lists them when no subcommand is given)
    Deps {
        #[command(subcommand)]
        action: Option<DepsAction>,
    },
    /// Generate HTML documentation
    Doc(DocArgs),
    /// Run a script through the interpreter
    ///
    /// Thin alias for `koja run --backend=interpreter`. Output comes
    /// from `IO.puts` / `value.print()` calls. The script's trailing
    /// value is discarded.
    Eval {
        /// Source file (`.kojs` script)
        file: String,
    },
    /// Format source files in place
    Format {
        /// Files or directories to format (formats project if omitted)
        files: Vec<String>,

        /// Check if files need formatting (exit 1 if so) instead of writing
        #[arg(long)]
        check: bool,
    },
    /// Dump the token stream
    Lex {
        /// Source files to lex
        files: Vec<String>,
    },
    /// Create a new Koja project
    New {
        /// Project name (used as directory name)
        name: String,
    },
    /// Dump the parsed AST
    Parse {
        /// Source files to parse
        files: Vec<String>,

        /// Print the parsed AST to stdout instead of just an item count
        #[arg(long)]
        emit_ast: bool,
    },
    /// Compile and run a source file, project, or task
    Run(pipeline::RunOptions),
    /// Start an interactive REPL backed by the interpreter
    Shell,
    /// List tasks exported by the project, its dependencies, and the toolchain
    Tasks,
    /// Run tests (requires koja.toml)
    Test {
        /// Print each test name and per-test timing as it runs instead of progress dots
        #[arg(long)]
        trace: bool,
    },
}

/// Arguments for `koja doc`. The optional `action` subcommand
/// turns the bare `koja doc` into a one-shot generator and
/// `koja doc serve` into a generate-then-host preview server.
/// Shared flags live on the parent so they apply to both.
#[derive(Args)]
struct DocArgs {
    /// Source files or directories (omit to use koja.toml)
    files: Vec<String>,

    /// Output directory for generated HTML (defaults to `doc`, or a
    /// temp dir when documenting the stdlib outside a project)
    #[arg(short, long)]
    output: Option<String>,

    /// Skip bundled stdlib + path dependencies and document the project sources only
    #[arg(long)]
    project_only: bool,

    #[command(subcommand)]
    action: Option<DocAction>,
}

#[derive(Subcommand)]
enum DepsAction {
    /// Remove the materialized deps/ directory
    Clean {
        /// Also purge the global git mirror cache
        #[arg(long)]
        cache: bool,
    },
    /// Fetch dependencies and update koja.lock
    ///
    /// The only network step: build/check/run/test are strictly
    /// offline and materialize deps/ from koja.lock plus the cache.
    Get,
    /// Re-resolve refs against their remotes and update koja.lock
    Update {
        /// Package to update (all git dependencies when omitted)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum DocAction {
    /// Look up a symbol and print its doc to the terminal
    ///
    /// An exact name (`List`, `List.append`, `JSON.Decoder`) renders
    /// the full doc as plain markdown. Partial matches list the
    /// candidates. Exits non-zero when nothing matches.
    Search {
        /// Symbol name or substring to look up
        query: String,
    },
    /// Rebuild docs and serve them on a local HTTP port
    ///
    /// Sidesteps the `file://` CORS restriction that prevents the
    /// in-page fuzzy search from loading `search-index.json` when
    /// opening the static tree directly in a browser.
    Serve {
        /// Port to bind on 127.0.0.1 (auto-picked from 8000+ if omitted)
        #[arg(long)]
        port: Option<u16>,

        /// Skip regenerating and serve whatever's already in the output dir
        #[arg(long)]
        no_rebuild: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let color = !cli.no_color && std::env::var("NO_COLOR").is_err();
    diagnostics::init_style(cli.diagnostics, cli.no_color);
    let project_root = cli
        .project
        .as_deref()
        .map(project::resolve_project_root)
        .transpose()
        .unwrap_or_else(|err| {
            eprintln!("error: {err}");
            process::exit(1);
        });
    diagnostics::set_path_base(project_root.as_deref());

    // Keep the on-disk stdlib extraction alive for tooling.
    // Best-effort, the pipeline compiles from embedded sources.
    let _ = koja_stdlib::extract();

    match cli.command {
        Command::Build(options) => pipeline::cmd_build(project_root.as_deref(), options),
        Command::Check { file, emit_ast } => {
            pipeline::cmd_check(project_root.as_deref(), file, emit_ast)
        }
        Command::Deps { action } => match action {
            None => deps::cmd_status(project_root.as_deref()),
            Some(DepsAction::Clean { cache }) => deps::cmd_clean(project_root.as_deref(), cache),
            Some(DepsAction::Get) => deps::cmd_get(project_root.as_deref(), None),
            Some(DepsAction::Update { name }) => deps::cmd_get(project_root.as_deref(), Some(name)),
        },
        Command::Doc(args) => dispatch_doc(args, project_root.as_deref()),
        Command::Eval { file } => {
            reject_project(project_root.as_deref(), "eval");
            pipeline::cmd_run(None, pipeline::RunOptions::interpreted(file, Vec::new()))
        }
        Command::Format { files, check } => {
            commands::cmd_format(project_root.as_deref(), files, check)
        }
        Command::Lex { files } => {
            reject_project(project_root.as_deref(), "lex");
            commands::cmd_lex(files)
        }
        // Alias for the self-hosted `koja.new` toolchain task.
        Command::New { name } => {
            reject_project(project_root.as_deref(), "new");
            pipeline::cmd_run(
                None,
                pipeline::RunOptions::interpreted("koja.new".to_string(), vec![name]),
            )
        }
        Command::Parse { files, emit_ast } => {
            reject_project(project_root.as_deref(), "parse");
            commands::cmd_parse(files, emit_ast)
        }
        Command::Run(options) => pipeline::cmd_run(project_root.as_deref(), options),
        Command::Shell => pipeline::cmd_shell(project_root.as_deref()),
        Command::Tasks => pipeline::cmd_tasks(project_root.as_deref()),
        Command::Test { trace } => pipeline::cmd_test(project_root.as_deref(), trace, color),
    }
}

/// Route `koja doc [...]` and its subcommands to the right handler.
/// Bare `koja doc` falls through to the static generator, `koja doc
/// serve` rebuilds (unless `--no-rebuild`) then hands the output dir
/// to the preview server, and `koja doc search` prints matches to
/// stdout without touching disk (`-o` is ignored).
fn dispatch_doc(args: DocArgs, project_root: Option<&Path>) {
    let DocArgs {
        action,
        files,
        output,
        project_only,
    } = args;

    let options = commands::DocOptions {
        files,
        output,
        project_only,
    };
    match action {
        None => commands::cmd_doc(project_root, options),
        Some(DocAction::Search { query }) => {
            commands::cmd_doc_search(project_root, options, &query);
        }
        Some(DocAction::Serve { port, no_rebuild }) => {
            commands::cmd_doc_serve(
                project_root,
                options,
                commands::DocServeOptions { no_rebuild, port },
            );
        }
    }
}

fn reject_project(project_root: Option<&Path>, command: &str) {
    if project_root.is_some() {
        eprintln!("error: `--project` cannot be used with `koja {command}`");
        process::exit(2);
    }
}
