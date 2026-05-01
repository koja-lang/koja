//! Expo compiler CLI entry point.
//!
//! Parses the top-level subcommand and delegates to [`commands`].

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
    },
    /// Run a source file through the IR interpreter
    Eval {
        /// Source file
        file: String,

        /// Entry function to invoke (defaults to `main`)
        #[arg(long)]
        entry: Option<String>,
    },
    /// Generate HTML documentation
    Doc {
        /// Source files or directories (omit to use expo.toml)
        files: Vec<String>,

        /// Output directory for generated HTML
        #[arg(short, long, default_value = "doc")]
        output: String,
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

fn main() {
    let cli = Cli::parse();
    let color = !cli.no_color && std::env::var("NO_COLOR").is_err();

    match cli.command {
        Command::Build {
            file,
            output,
            emit_llvm,
            release,
        } => commands::cmd_build(file, output, emit_llvm, release, color),
        Command::Check { files } => commands::cmd_check(files, color),
        Command::Doc { files, output } => commands::cmd_doc(files, output, color),
        Command::Eval { file, entry } => commands::cmd_eval(file, entry),
        Command::Format {
            files,
            check,
            write_back,
        } => commands::cmd_format(files, check, write_back, color),
        Command::Lex { files } => commands::cmd_lex(files, color),
        Command::New { name } => commands::cmd_new(name),
        Command::Parse { files } => commands::cmd_parse(files, color),
        Command::Run {
            file,
            release,
            args,
        } => commands::cmd_run(file, release, args, color),
        Command::Shell { project } => expo_shell::run(project, color),
        Command::Test => commands::cmd_test(color),
    }
}
