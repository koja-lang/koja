//! Expo compiler CLI entry point.
//!
//! Parses the top-level subcommand and delegates to [`commands`].

mod commands;
mod diagnostics;
mod pipeline;
pub mod project;
mod resolve;

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
    /// Compile a source file to a native binary
    Build {
        /// Source file (omit to use project.expo)
        file: Option<String>,

        /// Output binary name
        #[arg(short, long)]
        output: Option<String>,

        /// Print LLVM IR to stdout instead of producing a binary
        #[arg(long)]
        emit_llvm: bool,
    },
    /// Type-check a source file without compiling
    Check {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Generate HTML documentation
    Doc {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Format source files
    Format {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Dump the token stream
    Lex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Dump the parsed AST
    Parse {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compile and run a source file
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
        } => commands::cmd_build(file, output, emit_llvm, color),
        Command::Check { args } => commands::cmd_check(&args, color),
        Command::Doc { args } => commands::cmd_doc(&args, color),
        Command::Format { args } => commands::cmd_format(&args, color),
        Command::Lex { args } => commands::cmd_lex(&args, color),
        Command::Parse { args } => commands::cmd_parse(&args, color),
        Command::Run { args } => commands::cmd_run(&args, color),
    }
}
