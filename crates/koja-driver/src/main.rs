//! Koja compiler CLI entry point.
//!
//! Parses the top-level subcommand and dispatches each one through
//! either [`pipeline`] (the compiler pipeline: `koja-typecheck ->
//! koja-ir -> koja-ir-llvm` / `koja-ir-eval`) or
//! [`commands`] (frontend / filesystem tooling: `parse`, `lex`,
//! `format`, `doc`, `new`).
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
//! binary), so it carries no backend flag. A future WASM backend
//! slots in as a third variant.

mod commands;
mod deps;
mod diagnostics;
mod link;
mod loader;
mod pipeline;
pub mod project;
mod serve;

use koja_runtime as _;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "koja", version, about = "The Koja language compiler")]
struct Cli {
    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a source file or project to a native binary
    Build {
        /// Source file (`.koja` / `.kojs`, omit to use `koja.toml`)
        file: Option<String>,

        /// Output binary name
        #[arg(short, long)]
        output: Option<String>,

        /// Print LLVM IR to stdout instead of producing a binary
        #[arg(long)]
        emit_llvm: bool,

        /// Build with aggressive optimizations
        #[arg(long)]
        release: bool,
    },
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
    /// Run a source file through the interpreter
    ///
    /// Thin alias for `koja run --backend=interpreter`. Prints the
    /// trailing value and exits 0 on success.
    Eval {
        /// Source file (`.koja` / `.kojs`)
        file: String,
    },
    /// Format source files
    Format {
        /// Files or directories to format (formats project if omitted)
        files: Vec<String>,

        /// Check if files need formatting (exit 1 if so)
        #[arg(long)]
        check: bool,

        /// Write formatted output back to files
        #[arg(long = "write")]
        write_back: bool,
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
    /// Compile and run a source file or project
    Run {
        /// Source file (`.koja` / `.kojs`, omit to use `koja.toml`)
        file: Option<String>,

        /// Execution backend: `interpreter` runs in-process for fast startup, while `llvm` compiles to a native binary, runs it, and forwards its exit code
        #[arg(long, value_enum, default_value = "interpreter")]
        backend: pipeline::Backend,

        /// Build with aggressive optimizations (LLVM backend only)
        #[arg(long)]
        release: bool,

        /// Arguments passed to the compiled program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL backed by the interpreter
    Shell,
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

    /// Output directory for generated HTML
    #[arg(short, long, default_value = "doc")]
    output: String,

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

    match cli.command {
        Command::Build {
            file,
            output,
            emit_llvm,
            release,
        } => pipeline::cmd_build(file, output, release, emit_llvm),
        Command::Check { file, emit_ast } => pipeline::cmd_check(file, emit_ast),
        Command::Deps { action } => match action {
            None => deps::cmd_status(),
            Some(DepsAction::Clean { cache }) => deps::cmd_clean(cache),
            Some(DepsAction::Get) => deps::cmd_get(None),
            Some(DepsAction::Update { name }) => deps::cmd_get(Some(name)),
        },
        Command::Doc(args) => dispatch_doc(args, color),
        Command::Eval { file } => pipeline::cmd_run(
            Some(file),
            pipeline::Backend::Interpreter,
            false,
            Vec::new(),
        ),
        Command::Format {
            files,
            check,
            write_back,
        } => commands::cmd_format(files, check, write_back, color),
        Command::Lex { files } => commands::cmd_lex(files, color),
        Command::New { name } => commands::cmd_new(name),
        Command::Parse { files, emit_ast } => commands::cmd_parse(files, color, emit_ast),
        Command::Run {
            file,
            backend,
            release,
            args,
        } => pipeline::cmd_run(file, backend, release, args),
        Command::Shell => pipeline::cmd_shell(),
        Command::Test { trace } => pipeline::cmd_test(trace, color),
    }
}

/// Route `koja doc [...]` and `koja doc serve [...]` to the
/// right handler. Bare `koja doc` falls through to the static
/// generator, while `koja doc serve` rebuilds (unless `--no-rebuild`)
/// then hands the output dir to the preview server.
fn dispatch_doc(args: DocArgs, color: bool) {
    let DocArgs {
        action,
        files,
        output,
        project_only,
    } = args;

    match action {
        None => commands::cmd_doc(files, output, project_only, color),
        Some(DocAction::Serve { port, no_rebuild }) => {
            commands::cmd_doc_serve(files, output, project_only, port, no_rebuild, color);
        }
    }
}
