//! CLI command implementations.
//!
//! Each `cmd_*` function handles argument parsing for its subcommand and
//! delegates to [`crate::pipeline`] for compilation or directly to the
//! relevant crate (`expo_parser`, `expo_fmt`, `expo_doc`) for simpler tools.

use std::path::{Path, PathBuf};
use std::{env, fs, process};

use expo_ast::util::dedent;
use expo_parser::ParseMode;

use crate::diagnostics::render_diagnostics;
use crate::pipeline;
use crate::project::{self, ProjectConfig};
use crate::resolve;

/// Replaces the current process with the given binary via `exec`. Never returns on success.
fn exec_binary(binary: &Path, args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let err = process::Command::new(binary).args(args).exec();
    eprintln!("failed to run binary: {err}");
    process::exit(1);
}

/// Returns the `target/debug/` directory under the given root, creating it
/// if it doesn't exist.
fn target_debug_dir(project_root: &Path) -> PathBuf {
    let dir = project_root.join("target").join("debug");
    fs::create_dir_all(&dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create target directory: {e}");
        process::exit(1);
    });
    dir
}

/// Returns the process's current directory, or prints an error to stderr and
/// exits non-zero.
fn current_dir_or_exit() -> PathBuf {
    env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: cannot determine current directory: {e}");
        process::exit(1);
    })
}

/// Loads `expo.toml` from the current directory, returning `(config, cwd)`.
///
/// On a missing `expo.toml`, prints each line in `missing_message` to stderr
/// and exits non-zero. On any other error, prints `error: {e}` and exits.
fn load_project_or_exit(missing_message: &[&str]) -> (ProjectConfig, PathBuf) {
    let cwd = current_dir_or_exit();
    let config = match project::load_project(&cwd) {
        Ok(Some(c)) => c,
        Ok(None) => {
            for line in missing_message {
                eprintln!("{line}");
            }
            process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    (config, cwd)
}

/// `expo build [file.expo] [-o output] [--emit-llvm]` -- compiles an Expo program to an executable.
///
/// With no arguments, looks for `expo.toml` in the current directory.
/// With `--emit-llvm`, prints LLVM IR to stdout instead of producing a binary.
pub fn cmd_build(
    file: Option<String>,
    output: Option<String>,
    emit_llvm: bool,
    release: bool,
    color: bool,
) {
    let options = pipeline::BuildOptions {
        color,
        emit_llvm,
        quiet: false,
        release,
    };

    if let Some(source) = file {
        let args = pipeline::BuildArgs {
            output_name: output,
            source_file: Some(source),
        };
        pipeline::build(args, options);
    } else {
        let (config, cwd) = load_project_or_exit(&[
            "error: no source file specified and no expo.toml found",
            "Usage: expo build <file.expo> [-o output]",
            "  or:  create an expo.toml in the current directory",
        ]);

        pipeline::build_project(&config, &cwd, output.as_deref(), options);
    }
}

/// `expo run [file.expo] [-- args...]` -- compiles and runs an Expo program.
///
/// With no arguments, looks for `expo.toml` in the current directory.
/// The compiled binary is placed in `target/debug/` for project mode
/// or a temp directory for single-file mode. On Unix, the current process
/// is replaced with the binary via `exec` so signals reach it directly.
pub fn cmd_run(file: Option<String>, release: bool, run_args: Vec<String>, color: bool) {
    let options = pipeline::BuildOptions {
        color,
        emit_llvm: false,
        quiet: true,
        release,
    };

    if let Some(path) = file {
        let tmp_dir = env::temp_dir();
        let binary = tmp_dir.join(format!(
            "expo_run_{}",
            Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("out")
        ));
        let output = binary.to_str().unwrap().to_string();

        let args = pipeline::BuildArgs {
            output_name: Some(output),
            source_file: Some(path),
        };
        pipeline::build(args, options);
        exec_binary(&binary, &run_args);
    } else {
        let (config, cwd) = load_project_or_exit(&[
            "error: no source file specified and no expo.toml found",
            "Usage: expo run <file.expo>",
            "  or:  create an expo.toml in the current directory",
        ]);

        let binary = target_debug_dir(&cwd).join(&config.name);
        let output = binary.to_str().unwrap().to_string();

        pipeline::build_project(&config, &cwd, Some(&output), options);
        exec_binary(&binary, &run_args);
    }
}

/// `expo check [file.expo ...] [--emit-ast]` -- type-checks without producing an executable.
///
/// With no arguments, looks for `expo.toml` in the current directory.
/// With `--emit-ast`, prints each type-checked file's AST (`{:#?}`) to stdout
/// instead of the per-file/project OK line. The dump runs even when typecheck
/// reports diagnostics, but a non-zero exit is still gated on errors -- mirrors
/// how `--emit-llvm` works on `expo build`.
pub fn cmd_check(files: Vec<String>, color: bool, emit_ast: bool) {
    if files.is_empty() {
        let (config, cwd) = load_project_or_exit(&[
            "error: no source file specified and no expo.toml found",
            "Usage: expo check <file.expo>",
            "  or:  create an expo.toml in the current directory",
        ]);

        let has_errors = pipeline::check_project(&config, &cwd, color, emit_ast);
        if has_errors {
            process::exit(1);
        }
        if !emit_ast {
            println!("{}: OK", config.name);
        }
        return;
    }

    for path in &files {
        let entry_path = match Path::new(path).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {path}: {e}");
                process::exit(1);
            }
        };

        let has_errors = pipeline::check_single_file(&entry_path, color, emit_ast);

        if !has_errors && !emit_ast {
            println!("{path}: OK");
        }
    }
}

/// `expo doc [file.expo ...] [-o output_dir]` -- generates HTML documentation.
///
/// With no arguments, looks for `expo.toml` in the current directory.
pub fn cmd_doc(files: Vec<String>, output: String, color: bool) {
    let (mut inputs, project_name) = discover_doc_inputs(&files);
    inputs.sort_by(|a, b| a.1.cmp(&b.1));

    let project = extract_doc_project(&inputs, &project_name, color);
    if project.items.is_empty() {
        println!("no items to document");
        return;
    }

    let out_path = Path::new(&output);
    if let Err(e) = fs::create_dir_all(out_path) {
        eprintln!("error creating output directory: {e}");
        process::exit(1);
    }

    write_doc_files(&project, out_path);
    println!("docs generated: {}", out_path.display());
}

/// Resolves the list of source files `expo doc` will process, as
/// `(path, file_fqn)` pairs, plus the display name shown in the sidebar.
/// Empty `files` means project mode (walk `src` from `expo.toml` and every
/// dep's `src`) and uses `expo.toml`'s `name`; otherwise treat each entry
/// as a path or a directory of `.expo` files and fall back to "Docs".
fn discover_doc_inputs(files: &[String]) -> (Vec<(String, String)>, String) {
    let mut inputs = Vec::new();

    if files.is_empty() {
        let (config, cwd) = load_project_or_exit(&[
            "error: no source file specified and no expo.toml found",
            "Usage: expo doc <file.expo ...> [-o output_dir]",
            "  or:  create an expo.toml in the current directory",
        ]);

        for src_dir in &config.src {
            let dir = cwd.join(src_dir);
            if dir.is_dir() {
                collect_expo_files(&dir, &dir, Some(&config.name), &mut inputs);
            }
        }
        discover_dep_doc_inputs(&config, &cwd, &mut inputs);
        return (inputs, config.name);
    }

    for input in files {
        let p = Path::new(input);
        if p.is_dir() {
            collect_expo_files(p, p, None, &mut inputs);
        } else {
            let name = Path::new(input)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            inputs.push((input.clone(), name));
        }
    }
    (inputs, "Docs".to_string())
}

/// Walks every dependency declared in `[dependencies]` and appends its source
/// files to `out`. Missing paths or unreadable `expo.toml` files emit a
/// warning and skip the dep rather than aborting the doc build.
fn discover_dep_doc_inputs(config: &ProjectConfig, cwd: &Path, out: &mut Vec<(String, String)>) {
    for (alias, dep) in &config.dependencies {
        let dep_path = match &dep.path {
            Some(p) => cwd.join(p),
            None => {
                eprintln!("warning: dependency `{alias}` has no path, skipping docs");
                continue;
            }
        };
        let dep_config = match project::load_project(&dep_path) {
            Ok(Some(c)) => c,
            Ok(None) => {
                eprintln!(
                    "warning: dependency `{alias}` has no expo.toml at {}",
                    dep_path.display()
                );
                continue;
            }
            Err(e) => {
                eprintln!("warning: dependency `{alias}`: {e}");
                continue;
            }
        };
        for src_dir in &dep_config.src {
            let dir = dep_path.join(src_dir);
            if dir.is_dir() {
                collect_expo_files(&dir, &dir, Some(&dep_config.name), out);
            }
        }
    }
}

/// Parses every input file and extracts doc-renderable items into a
/// [`expo_doc::DocProject`]. Files with parse errors are reported and skipped.
fn extract_doc_project(
    inputs: &[(String, String)],
    project_name: &str,
    color: bool,
) -> expo_doc::DocProject {
    let mut project = expo_doc::DocProject {
        constants: Vec::new(),
        enums: Vec::new(),
        functions: Vec::new(),
        items: Vec::new(),
        name: project_name.to_string(),
        protocols: Vec::new(),
        structs: Vec::new(),
    };

    for (path, _file_fqn) in inputs {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let parse_result = expo_parser::parse(&source, ParseMode::File);
        if !parse_result.errors.is_empty() {
            render_diagnostics(path, &source, &parse_result.errors, color);
            continue;
        }

        expo_doc::extract_items(&parse_result.ast, &mut project);
    }

    expo_doc::finalize_project(&mut project);
    project
}

/// Renders each item in `project` as HTML and writes it under `out_path`,
/// plus a top-level `index.html`.
fn write_doc_files(project: &expo_doc::DocProject, out_path: &Path) {
    for c in &project.constants {
        let html = expo_doc::render_constant(c, project);
        write_doc_file(&out_path.join(format!("{}.html", c.name)), &html);
    }
    for e in &project.enums {
        let html = expo_doc::render_enum(e, project);
        write_doc_file(&out_path.join(format!("{}.html", e.name)), &html);
    }
    for f in &project.functions {
        let html = expo_doc::render_function(f, project);
        write_doc_file(&out_path.join(format!("{}.html", f.name)), &html);
    }
    for p in &project.protocols {
        let html = expo_doc::render_protocol(p, project);
        write_doc_file(&out_path.join(format!("{}.html", p.name)), &html);
    }
    for s in &project.structs {
        let html = expo_doc::render_struct(s, project);
        write_doc_file(&out_path.join(format!("{}.html", s.name)), &html);
    }

    let index_html = expo_doc::render_index(project);
    write_doc_file(&out_path.join("index.html"), &index_html);
}

fn write_doc_file(path: &Path, content: &str) {
    if let Err(e) = fs::write(path, content) {
        eprintln!("error writing {}: {e}", path.display());
        process::exit(1);
    }
    println!("  {}", path.display());
}

/// Recursively collects `.expo` files from a directory, building file FQNs
/// from relative paths (e.g. `foo/bar.expo` becomes file FQN `foo.bar`).
/// When `prefix` is `Some`, each FQN is prefixed with `{prefix}.` (e.g.
/// `src/lexer.expo` with prefix `myproject` becomes `myproject.lexer`).
fn collect_expo_files(
    dir: &Path,
    root: &Path,
    prefix: Option<&str>,
    out: &mut Vec<(String, String)>,
) {
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
            collect_expo_files(&path, root, prefix, out);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "expo") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .with_extension("")
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join(".");
        let file_fqn = match prefix {
            Some(p) => format!("{p}.{relative}"),
            None => relative,
        };
        if let Some(s) = path.to_str() {
            out.push((s.to_string(), file_fqn));
        }
    }
}

/// `expo eval <file>` -- runs the file through the IR interpreter and
/// prints the entry function's result. The interpreter's coverage
/// matches what the IR lowerer produces without `IRInstruction::Stub`;
/// programs that exceed that coverage report a precise interpreter
/// error rather than silently falling through to codegen.
pub fn cmd_eval(file: String, entry: Option<String>) {
    let path = Path::new(&file);
    match expo_shell::eval_file(path, entry.as_deref()) {
        Ok(Some(value)) => println!("{value}"),
        Ok(None) => {}
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// `expo test` -- discovers `@test` functions, compiles a test harness, and runs it.
///
/// Requires an `expo.toml` in the current directory.
pub fn cmd_test(color: bool) {
    let (config, cwd) = load_project_or_exit(&[
        "error: no expo.toml found",
        "Usage: expo test (run from a directory containing expo.toml)",
    ]);

    pipeline::test_project(&config, &cwd, color);
}

/// `expo format [files...] [--check] [--write]` -- formats Expo source files.
///
/// With no arguments, looks for `expo.toml` and formats all `.expo` files in
/// the project's `src` and `test` directories. Directory arguments are walked
/// recursively for `.expo` files.
pub fn cmd_format(files: Vec<String>, check: bool, write: bool, color: bool) {
    let resolved = if files.is_empty() {
        let (config, cwd) = load_project_or_exit(&[
            "error: no files specified and no expo.toml found",
            "Usage: expo format [files...] [--check] [--write]",
            "  or:  create an expo.toml in the current directory",
        ]);

        let roots: Vec<PathBuf> = config
            .src
            .iter()
            .chain(config.test.iter())
            .map(|s| cwd.join(s))
            .collect();

        let mut paths = Vec::new();
        for root in &roots {
            if root.is_dir() {
                paths.extend(resolve::collect_expo_files_recursive(root));
            }
        }
        paths.sort();
        paths
            .into_iter()
            .filter_map(|p| p.to_str().map(String::from))
            .collect::<Vec<_>>()
    } else {
        let mut paths = Vec::new();
        for input in &files {
            let p = Path::new(input);
            if p.is_dir() {
                let found = resolve::collect_expo_files_recursive(p);
                for f in found {
                    if let Some(s) = f.to_str() {
                        paths.push(s.to_string());
                    }
                }
            } else {
                paths.push(input.clone());
            }
        }
        paths.sort();
        paths
    };

    let mut has_diff = false;
    for path in &resolved {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let result = expo_fmt::format(&source);

        let formatted = match result {
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
        } else if write || resolved.len() > 1 {
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

/// `expo new <name>` -- scaffolds a new Expo project.
///
/// Creates a directory with `expo.toml` and `src/main.expo`.
pub fn cmd_new(name: String) {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        eprintln!("error: project name must contain only ASCII letters, digits, and underscores");
        process::exit(1);
    }

    let project_dir = Path::new(&name);
    if project_dir.exists() {
        eprintln!("error: directory '{name}' already exists");
        process::exit(1);
    }

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create directory: {e}");
        process::exit(1);
    });

    let toml_content = dedent(&format!(
        "
        [project]
        entry = \"main\"
        name = \"{name}\"
        version = \"0.1.0\"
        "
    ));
    fs::write(project_dir.join("expo.toml"), toml_content).unwrap_or_else(|e| {
        eprintln!("error: cannot write expo.toml: {e}");
        process::exit(1);
    });

    let main_content = dedent(
        "
        fn main
          name = \"Expo\"
          IO.puts(\"Hello, #{name}!\")
        end
        ",
    );
    fs::write(src_dir.join("main.expo"), main_content).unwrap_or_else(|e| {
        eprintln!("error: cannot write src/main.expo: {e}");
        process::exit(1);
    });

    println!("created project '{name}'");
}

/// `expo parse <file.expo> [--emit-ast]` -- parses and reports item count or errors.
///
/// With `--emit-ast`, prints the parsed AST to stdout using the compact
/// `expo_ast::format_file` tree (2-space indent, `@L:C-L:C` span
/// suffixes, exhaustive over every AST variant) instead of the
/// item-count line. Annotation slots like `Expr.resolved_type` are
/// `None` here -- no typecheck has run. Diagnostics still go to
/// stderr regardless.
pub fn cmd_parse(files: Vec<String>, color: bool, emit_ast: bool) {
    if files.is_empty() {
        eprintln!("Usage: expo parse <file.expo>");
        process::exit(1);
    }

    for (index, path) in files.iter().enumerate() {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let mut result = expo_parser::parse(&source, ParseMode::File);
        // `expo_parser::parse` is the bare-string entry point that
        // leaves `ast.path` unset. Populate it from the CLI argument
        // so `--emit-ast` surfaces the file identity in the `File`
        // header line.
        result.ast.path = Some(std::path::PathBuf::from(path));

        if !result.errors.is_empty() {
            render_diagnostics(path, &source, &result.errors, color);
            continue;
        }

        if emit_ast {
            if index > 0 {
                println!();
            }
            print!("{}", expo_ast::format_file(&result.ast));
        } else {
            println!("{path}: OK ({} items)", result.ast.items.len());
        }
    }
}

/// `expo lex <file.expo>` -- lexes and prints every token with its position.
pub fn cmd_lex(files: Vec<String>, color: bool) {
    if files.is_empty() {
        eprintln!("Usage: expo lex <file.expo>");
        process::exit(1);
    }

    for path in &files {
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
