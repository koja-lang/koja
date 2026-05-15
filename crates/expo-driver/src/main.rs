//! Expo compiler CLI entry point.
//!
//! Parses the top-level subcommand and dispatches each one through
//! either [`alpha`] (the compiler pipeline: `expo-alpha-typecheck →
//! expo-alpha-ir → expo-alpha-ir-llvm` / `expo-alpha-ir-eval`) or
//! [`commands`] (frontend / filesystem tooling: `parse`, `lex`,
//! `format`, `doc`, `new`).
//!
//! Source dispatch follows [`alpha::cmd_build`]'s extension rules:
//! `.exps` files are scripts (top-level expressions, no project
//! context); `.expo` files are project files. Omitting the file
//! argument falls back to discovering an `expo.toml` in the
//! current directory; project mode runs the full pipeline through
//! [`expo_alpha_ir_llvm::compile_program`].
//!
//! Backend selection: `run` and `build` accept
//! `--backend={interpreter,llvm}` (see [`alpha::Backend`]). `run`
//! defaults to `interpreter` (fast feedback, prints the trailing
//! value, exits 0); `build` defaults to `llvm` (only backend that
//! produces a binary). `build --backend=interpreter` errors. A
//! future WASM backend slots in as a third variant.

mod alpha;
mod commands;
mod diagnostics;
mod link;
pub mod project;

use expo_runtime as _;

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
    /// Compile a source file or project to a native binary
    Build {
        /// Source file (`.expo` / `.exps`; omit to use `expo.toml`)
        file: Option<String>,

        /// Backend to drive the build through (defaults to `llvm`; `interpreter` errors since it cannot produce a binary)
        #[arg(long, value_enum, default_value = "llvm")]
        backend: alpha::Backend,

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
        /// Source file (`.expo` / `.exps`; omit to use `expo.toml`)
        file: Option<String>,

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
    /// Run a source file through the interpreter
    ///
    /// Thin alias for `expo run --backend=interpreter`. Prints the
    /// trailing value and exits 0 on success.
    Eval {
        /// Source file (`.expo` / `.exps`)
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
    /// Compile and run a source file or project
    Run {
        /// Source file (`.expo` / `.exps`; omit to use `expo.toml`)
        file: Option<String>,

        /// Backend to drive execution through (defaults to `interpreter`; `llvm` compiles + execs and forwards the exit code)
        #[arg(long, value_enum, default_value = "interpreter")]
        backend: alpha::Backend,

        /// Build with aggressive optimizations (LLVM backend only)
        #[arg(long)]
        release: bool,

        /// Arguments passed to the compiled program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL backed by the interpreter
    Shell,
    /// Run tests (requires expo.toml)
    Test,
}

fn main() {
    let cli = Cli::parse();
    let color = !cli.no_color && std::env::var("NO_COLOR").is_err();

    match cli.command {
        Command::Build {
            file,
            backend,
            output,
            emit_llvm,
            release,
        } => alpha::cmd_build(file, backend, output, release, emit_llvm),
        Command::Check { file, emit_ast } => alpha::cmd_check(file, emit_ast),
        Command::Doc { files, output } => commands::cmd_doc(files, output, color),
        Command::Eval { file } => {
            alpha::cmd_run(Some(file), alpha::Backend::Interpreter, false, Vec::new())
        }
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
        } => alpha::cmd_run(file, backend, release, args),
        Command::Shell => alpha::cmd_shell(),
        Command::Test => alpha::cmd_test(),
    }
}
