use std::env;
use std::fs;
use std::path::Path;
use std::process;

use expo_ast::ast::{Diagnostic, Severity};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("expo compiler v{}", env!("CARGO_PKG_VERSION"));
        eprintln!("Usage: expo <command> [args]");
        eprintln!("Commands: build, check, format, lex, parse, run");
        process::exit(1);
    }

    match args[1].as_str() {
        "build" => cmd_build(&args[2..]),
        "check" => cmd_check(&args[2..]),
        "format" => cmd_format(&args[2..]),
        "lex" => cmd_lex(&args[2..]),
        "parse" => cmd_parse(&args[2..]),
        "run" => cmd_run(&args[2..]),
        other => {
            eprintln!("unknown command: {other}");
            process::exit(1);
        }
    }
}

fn print_diagnostics(diagnostics: &[Diagnostic]) {
    for d in diagnostics {
        let severity = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        eprintln!(
            "  {}:{}: {}: {}",
            d.span.start.line, d.span.start.column, severity, d.message
        );
    }
}

fn cmd_build(args: &[String]) {
    build(args, false);
}

fn build(args: &[String], quiet: bool) {
    if args.is_empty() {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    }

    let mut source_file = None;
    let mut output_name = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" {
            if i + 1 < args.len() {
                output_name = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("-o requires an argument");
                process::exit(1);
            }
        } else {
            source_file = Some(args[i].clone());
            i += 1;
        }
    }

    let path = source_file.unwrap_or_else(|| {
        eprintln!("Usage: expo build <file.expo> [-o output]");
        process::exit(1);
    });

    let output = output_name.unwrap_or_else(|| {
        Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string()
    });

    let source = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            process::exit(1);
        }
    };

    let parse_result = expo_parser::parse(&source);
    if !parse_result.errors.is_empty() {
        eprintln!("{path}: {} parse error(s)", parse_result.errors.len());
        print_diagnostics(&parse_result.errors);
        process::exit(1);
    }

    let ctx = expo_typecheck::check(&parse_result.module);
    if !ctx.diagnostics.is_empty() {
        eprintln!("{path}: {} type error(s)", ctx.diagnostics.len());
        print_diagnostics(&ctx.diagnostics);
        process::exit(1);
    }

    let obj_path = format!("{output}.o");
    if let Err(diagnostics) =
        expo_codegen::compile(&parse_result.module, &ctx, Path::new(&obj_path))
    {
        eprintln!("{path}: {} codegen error(s)", diagnostics.len());
        print_diagnostics(&diagnostics);
        process::exit(1);
    }

    let status = process::Command::new("cc")
        .args([&obj_path, "-o", &output])
        .status();

    match status {
        Ok(s) if s.success() => {
            let _ = fs::remove_file(&obj_path);
            if !quiet {
                println!("compiled: {output}");
            }
        }
        Ok(s) => {
            eprintln!("linker failed with exit code: {}", s.code().unwrap_or(-1));
            let _ = fs::remove_file(&obj_path);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to run linker: {e}");
            let _ = fs::remove_file(&obj_path);
            process::exit(1);
        }
    }
}

fn cmd_run(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: expo run <file.expo>");
        process::exit(1);
    }

    let path = &args[0];
    let tmp_dir = env::temp_dir();
    let binary = tmp_dir.join(format!(
        "expo_run_{}",
        Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("out")
    ));
    let output = binary.to_str().unwrap().to_string();

    let build_args = vec![path.clone(), "-o".to_string(), output.clone()];
    build(&build_args, true);

    let status = process::Command::new(&binary).args(&args[1..]).status();

    let _ = fs::remove_file(&binary);

    match status {
        Ok(s) => process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("failed to run binary: {e}");
            process::exit(1);
        }
    }
}

fn cmd_check(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: expo check <file.expo>");
        process::exit(1);
    }

    for path in args {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let parse_result = expo_parser::parse(&source);
        if !parse_result.errors.is_empty() {
            println!("{path}: {} parse error(s)", parse_result.errors.len());
            print_diagnostics(&parse_result.errors);
            continue;
        }

        let ctx = expo_typecheck::check(&parse_result.module);
        if ctx.diagnostics.is_empty() {
            println!("{path}: OK");
        } else {
            println!("{path}: {} type error(s)", ctx.diagnostics.len());
            print_diagnostics(&ctx.diagnostics);
        }
    }
}

fn cmd_format(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: expo format <file.expo> [--check] [--write]");
        process::exit(1);
    }

    let check = args.contains(&"--check".to_string());
    let write = args.contains(&"--write".to_string());
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    let mut has_diff = false;
    for path in &files {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let formatted = match expo_fmt::format(&source) {
            expo_fmt::FormatResult::Ok(s) => s,
            expo_fmt::FormatResult::ParseErrors(errors) => {
                eprintln!("{path}: cannot format due to parse errors");
                print_diagnostics(&errors);
                has_diff = true;
                continue;
            }
        };

        if check {
            if source != formatted {
                println!("{path}: would reformat");
                has_diff = true;
            } else {
                println!("{path}: ok");
            }
        } else if write {
            if source != formatted {
                if let Err(e) = fs::write(path, &formatted) {
                    eprintln!("error writing {path}: {e}");
                    process::exit(1);
                }
                println!("{path}: formatted");
            } else {
                println!("{path}: unchanged");
            }
        } else {
            print!("{formatted}");
        }
    }

    if check && has_diff {
        process::exit(1);
    }
}

fn cmd_parse(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: expo parse <file.expo>");
        process::exit(1);
    }

    for path in args {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let result = expo_parser::parse(&source);

        if result.errors.is_empty() {
            println!("{path}: OK ({} items)", result.module.items.len());
        } else {
            println!("{path}: {} error(s)", result.errors.len());
            print_diagnostics(&result.errors);
        }
    }
}

fn cmd_lex(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: expo lex <file.expo>");
        process::exit(1);
    }

    for path in args {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let result = expo_lexer::lex(&source);

        if !result.errors.is_empty() {
            eprintln!("{path}: {} lex error(s)", result.errors.len());
            print_diagnostics(&result.errors);
        }

        println!(
            "{path}: {} tokens, {} comments",
            result.tokens.len(),
            result.comments.len()
        );
        for token in &result.tokens {
            println!(
                "  {:?} @ {}:{}",
                token.kind, token.span.start.line, token.span.start.column
            );
        }
    }
}
