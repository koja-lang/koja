//! CLI command implementations.
//!
//! Each `cmd_*` function handles argument parsing for its subcommand and
//! delegates to [`crate::pipeline`] for compilation or directly to the
//! relevant crate (`expo_parser`, `expo_fmt`, `expo_doc`) for simpler tools.

use std::path::Path;
use std::{env, fs, process};

use crate::diagnostics::render_diagnostics;
use crate::pipeline;
use crate::resolve;

/// `expo build <file.expo> [-o output]` -- compiles an Expo program to an executable.
pub fn cmd_build(args: &[String], color: bool) {
    pipeline::build(args, false, color);
}

/// `expo run <file.expo>` -- compiles to a temporary binary, runs it, then cleans up.
pub fn cmd_run(args: &[String], color: bool) {
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
    pipeline::build(&build_args, true, color);

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

/// `expo check <file.expo>` -- type-checks without producing an executable.
pub fn cmd_check(args: &[String], color: bool) {
    if args.is_empty() {
        eprintln!("Usage: expo check <file.expo>");
        process::exit(1);
    }

    for path in args {
        let entry_path = match Path::new(path).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {path}: {e}");
                process::exit(1);
            }
        };

        let graph = match resolve::resolve_modules(&entry_path) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("error: {e}");
                continue;
            }
        };

        let stdlib = pipeline::parse_stdlib();
        let (_, has_errors) = pipeline::typecheck_modules(&graph, &stdlib, color);

        if !has_errors {
            println!("{path}: OK");
        }
    }
}

/// `expo doc <file.expo ...> [-o output_dir]` -- generates HTML documentation.
pub fn cmd_doc(args: &[String], color: bool) {
    if args.is_empty() {
        eprintln!("Usage: expo doc <file.expo ...> [-o output_dir]");
        process::exit(1);
    }

    let mut inputs = Vec::new();
    let mut output_dir = "doc".to_string();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" {
            if i + 1 < args.len() {
                output_dir = args[i + 1].clone();
                i += 2;
            } else {
                eprintln!("-o requires an argument");
                process::exit(1);
            }
        } else {
            inputs.push(args[i].clone());
            i += 1;
        }
    }

    let mut files: Vec<(String, String)> = Vec::new();
    for input in &inputs {
        let p = Path::new(input);
        if p.is_dir() {
            collect_expo_files(p, p, &mut files);
        } else {
            let name = Path::new(input)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            files.push((input.clone(), name));
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));

    let mut doc_modules = Vec::new();

    for (path, module_name) in &files {
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

        if let Some(doc_module) = expo_doc::extract_module(module_name, &parse_result.module) {
            doc_modules.push(doc_module);
        }
    }

    if doc_modules.is_empty() {
        println!("no modules to document");
        return;
    }

    let out_path = Path::new(&output_dir);
    if let Err(e) = fs::create_dir_all(out_path) {
        eprintln!("error creating output directory: {e}");
        process::exit(1);
    }

    let all_module_names: Vec<String> = doc_modules.iter().map(|m| m.name.clone()).collect();

    for m in &doc_modules {
        let html = expo_doc::render_module(m, &all_module_names);
        let file_path = out_path.join(format!("{}.html", m.name));
        if let Err(e) = fs::write(&file_path, &html) {
            eprintln!("error writing {}: {e}", file_path.display());
            process::exit(1);
        }
        println!("  {}", file_path.display());
    }

    let index_html = expo_doc::render_index(&doc_modules);
    let index_path = out_path.join("index.html");
    if let Err(e) = fs::write(&index_path, &index_html) {
        eprintln!("error writing {}: {e}", index_path.display());
        process::exit(1);
    }
    println!("  {}", index_path.display());
    println!("docs generated: {}", out_path.display());
}

/// Recursively collects `.expo` files from a directory, building module names
/// from relative paths (e.g. `foo/bar.expo` becomes module `foo.bar`).
fn collect_expo_files(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error reading directory {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_expo_files(&path, root, out);
        } else if path.extension().is_some_and(|ext| ext == "expo") {
            let module_name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .with_extension("")
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(s) = path.to_str() {
                out.push((s.to_string(), module_name));
            }
        }
    }
}

/// `expo format <file.expo> [--check] [--write]` -- formats Expo source files.
pub fn cmd_format(args: &[String], color: bool) {
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

/// `expo parse <file.expo>` -- parses and reports item count or errors.
pub fn cmd_parse(args: &[String], color: bool) {
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

/// `expo lex <file.expo>` -- lexes and prints every token with its position.
pub fn cmd_lex(args: &[String], color: bool) {
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
