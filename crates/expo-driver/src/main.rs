//! Expo compiler CLI entry point.
//!
//! Parses the top-level subcommand and delegates to [`commands`].

mod alpha;
mod commands;
mod diagnostics;
mod pipeline;
pub mod project;
mod resolve;

use expo_runtime as _;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "expo", version, about = "The Expo language compiler")]
struct Cli {
    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Experimental alpha-pipeline subcommands (subject to breaking changes)
    Alpha {
        #[command(subcommand)]
        command: AlphaCommand,
    },
    /// Compile a source file to a native binary
    Build {
        /// Source file (omit to use expo.toml)
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
    /// Type-check a source file without compiling
    Check {
        /// Source files (omit to use expo.toml)
        files: Vec<String>,

        /// Print the type-checked AST to stdout instead of just OK/diagnostics
        #[arg(long)]
        emit_ast: bool,
    },
    /// Generate HTML documentation
    Doc {
        /// Source files or directories (omit to use expo.toml)
        files: Vec<String>,

        /// Output directory for generated HTML
        #[arg(short, long, default_value = "doc")]
        output: String,
    },
    /// Run a source file through the IR interpreter
    Eval {
        /// Source file
        file: String,

        /// Entry function to invoke (defaults to `main`)
        #[arg(long)]
        entry: Option<String>,
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
    /// Create a new Expo project
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
    /// Compile and run a source file
    Run {
        /// Source file (omit to use expo.toml)
        file: Option<String>,

        /// Build with aggressive optimizations
        #[arg(long)]
        release: bool,

        /// Arguments passed to the compiled program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL backed by the IR interpreter
    Shell {
        /// Load a project directory before starting the REPL
        /// (currently ignored -- project loading is a future
        /// enhancement; the MVP shell evaluates each input as a
        /// self-contained expression).
        #[arg(short = 'S', long = "project")]
        project: Option<PathBuf>,
    },
    /// Run tests (requires expo.toml)
    Test,
}

/// Subcommands under `expo alpha`. These drive the alpha compiler
/// pipeline (`expo-alpha-typecheck → expo-alpha-ir → expo-alpha-ir-eval`) and
/// are intentionally namespaced so the production `expo eval` /
/// `expo shell` paths can keep their full v1 feature set during the
/// alpha build-out.
///
/// Source dispatch follows [`alpha::cmd_build`]'s extension rules:
/// `.exps` files are scripts (top-level expressions, no project
/// context); `.expo` files are project members. Omitting the file
/// argument falls back to discovering an `expo.toml` in the current
/// directory; project mode runs the full alpha pipeline through
/// [`expo_alpha_ir_llvm::compile_program`].
///
/// Backend selection: `run` and `build` accept
/// `--backend={interpreter,llvm}` (see [`alpha::Backend`]). `run`
/// defaults to `interpreter` (fast feedback, prints the trailing
/// value, exits 0); `build` defaults to `llvm` (only backend that
/// produces a binary). `build --backend=interpreter` errors. The
/// future WASM backend slots in here as a third variant.
#[derive(Subcommand)]
enum AlphaCommand {
    /// Compile a source file through the alpha pipeline to a native binary (`.exps` scripts and `expo.toml` projects)
    Build {
        /// Source file (omit to use `expo.toml` in the current directory)
        file: Option<String>,

        /// Backend to drive the build through (defaults to `llvm`; `interpreter` errors since it cannot produce a binary)
        #[arg(long, value_enum, default_value = "llvm")]
        backend: alpha::Backend,

        /// Output binary name
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Type-check a source file through the alpha pipeline without lowering or running it
    Check {
        /// Source file (omit to use `expo.toml` in the current directory)
        file: Option<String>,

        /// Print the type-checked AST to stdout instead of just OK/diagnostics
        #[arg(long)]
        emit_ast: bool,
    },
    /// Run a source file through the alpha pipeline
    Run {
        /// Source file (omit to use `expo.toml` in the current directory)
        file: Option<String>,

        /// Backend to drive execution through (defaults to `interpreter`; `llvm` compiles + execs and forwards the exit code)
        #[arg(long, value_enum, default_value = "interpreter")]
        backend: alpha::Backend,

        /// Arguments passed to the compiled program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL backed by the alpha pipeline
    Shell,
}

fn main() {
    let cli = Cli::parse();
    let color = !cli.no_color && std::env::var("NO_COLOR").is_err();

    match cli.command {
        Command::Alpha { command } => match command {
            AlphaCommand::Build {
                file,
                backend,
                output,
            } => alpha::cmd_build(file, backend, output),
            AlphaCommand::Check { file, emit_ast } => alpha::cmd_check(file, emit_ast),
            AlphaCommand::Run {
                file,
                backend,
                args,
            } => alpha::cmd_run(file, backend, args),
            AlphaCommand::Shell => alpha::cmd_shell(),
        },
        Command::Build {
            file,
            output,
            emit_llvm,
            release,
        } => commands::cmd_build(file, output, emit_llvm, release, color),
        Command::Check { files, emit_ast } => commands::cmd_check(files, color, emit_ast),
        Command::Doc { files, output } => commands::cmd_doc(files, output, color),
        Command::Eval { file, entry } => commands::cmd_eval(file, entry),
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
            release,
            args,
        } => commands::cmd_run(file, release, args, color),
        Command::Shell { project } => expo_shell::run(project, color),
        Command::Test => commands::cmd_test(color),
    }
}
