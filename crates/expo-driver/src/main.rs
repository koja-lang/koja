//! Expo compiler CLI entry point.
//!
//! Parses the top-level subcommand and delegates to [`commands`].

mod commands;
mod diagnostics;
mod pipeline;
mod resolve;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("expo compiler v{}", env!("CARGO_PKG_VERSION"));
        eprintln!("Usage: expo <command> [args]");
        eprintln!("Commands: build, check, doc, format, lex, parse, run");
        process::exit(1);
    }

    let color = !args.contains(&"--no-color".to_string()) && env::var("NO_COLOR").is_err();
    let cmd_args: Vec<String> = args[2..]
        .iter()
        .filter(|a| *a != "--no-color")
        .cloned()
        .collect();

    match args[1].as_str() {
        "build" => commands::cmd_build(&cmd_args, color),
        "check" => commands::cmd_check(&cmd_args, color),
        "doc" => commands::cmd_doc(&cmd_args, color),
        "format" => commands::cmd_format(&cmd_args, color),
        "lex" => commands::cmd_lex(&cmd_args, color),
        "parse" => commands::cmd_parse(&cmd_args, color),
        "run" => commands::cmd_run(&cmd_args, color),
        other => {
            eprintln!("unknown command: {other}");
            process::exit(1);
        }
    }
}
