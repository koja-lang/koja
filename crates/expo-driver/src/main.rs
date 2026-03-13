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

    let color = !args.contains(&"--no-color".to_string()) && env::var("NO_COLOR").is_err();
    let cmd_args: Vec<String> = args[2..]
        .iter()
        .filter(|a| *a != "--no-color")
        .cloned()
        .collect();

    match args[1].as_str() {
        "build" => cmd_build(&cmd_args, color),
        "check" => cmd_check(&cmd_args, color),
        "format" => cmd_format(&cmd_args, color),
        "lex" => cmd_lex(&cmd_args, color),
        "parse" => cmd_parse(&cmd_args, color),
        "run" => cmd_run(&cmd_args, color),
        other => {
            eprintln!("unknown command: {other}");
            process::exit(1);
        }
    }
}

fn render_diagnostics(filename: &str, source: &str, diagnostics: &[Diagnostic], color: bool) {
    let lines: Vec<&str> = source.lines().collect();
    let max_line = diagnostics
        .iter()
        .map(|d| d.span.start.line as usize)
        .max()
        .unwrap_or(1);
    let gutter_width = max_line.to_string().len();

    for d in diagnostics {
        let severity_label = match (&d.severity, color) {
            (Severity::Error, true) => "\x1b[1;31merror\x1b[0m",
            (Severity::Error, false) => "error",
            (Severity::Warning, true) => "\x1b[1;33mwarning\x1b[0m",
            (Severity::Warning, false) => "warning",
            (Severity::Note, _) => "note",
        };

        eprintln!("{severity_label}: {}", d.message);
        eprintln!(
            "{:>gutter_width$}--> {filename}:{}:{}",
            " ", d.span.start.line, d.span.start.column
        );

        let line_idx = d.span.start.line.saturating_sub(1) as usize;
        if let Some(source_line) = lines.get(line_idx) {
            eprintln!("{:>gutter_width$} |", "");
            eprintln!("{:>gutter_width$} | {source_line}", d.span.start.line);

            let col_start = d.span.start.column.saturating_sub(1) as usize;
            let col_end = if d.span.start.line == d.span.end.line {
                (d.span.end.column as usize).max(col_start + 1)
            } else {
                source_line.len().max(col_start + 1)
            };
            let caret_count = col_end.saturating_sub(col_start).max(1);
            let padding = " ".repeat(col_start);
            let carets = "^".repeat(caret_count);
            eprintln!("{:>gutter_width$} | {padding}{carets}", "");
        }

        if let Some(hint) = &d.hint {
            eprintln!("{:>gutter_width$} |", "");
            eprintln!("{:>gutter_width$} = hint: {hint}", "");
        }

        eprintln!();
    }
}

fn cmd_build(args: &[String], color: bool) {
    build(args, false, color);
}

fn build(args: &[String], quiet: bool, color: bool) {
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
        render_diagnostics(&path, &source, &parse_result.errors, color);
        process::exit(1);
    }

    let ctx = expo_typecheck::check(&parse_result.module);
    if !ctx.diagnostics.is_empty() {
        render_diagnostics(&path, &source, &ctx.diagnostics, color);
        process::exit(1);
    }

    let obj_path = format!("{output}.o");
    if let Err(diagnostics) =
        expo_codegen::compile(&parse_result.module, &ctx, Path::new(&obj_path))
    {
        render_diagnostics(&path, &source, &diagnostics, color);
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

fn cmd_run(args: &[String], color: bool) {
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
    build(&build_args, true, color);

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

fn cmd_check(args: &[String], color: bool) {
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
            render_diagnostics(path, &source, &parse_result.errors, color);
            continue;
        }

        let ctx = expo_typecheck::check(&parse_result.module);
        if ctx.diagnostics.is_empty() {
            println!("{path}: OK");
        } else {
            render_diagnostics(path, &source, &ctx.diagnostics, color);
        }
    }
}

fn cmd_format(args: &[String], color: bool) {
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
                render_diagnostics(path, &source, &errors, color);
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

fn cmd_parse(args: &[String], color: bool) {
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
            render_diagnostics(path, &source, &result.errors, color);
        }
    }
}

fn cmd_lex(args: &[String], color: bool) {
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
            render_diagnostics(path, &source, &result.errors, color);
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
