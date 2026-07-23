//! Frontend / filesystem CLI command implementations.
//!
//! Each `cmd_*` function handles argument parsing for its
//! subcommand and delegates to the relevant standalone crate
//! (`koja_parser`, `koja_lexer`, `koja_fmt`, `koja_doc`) for
//! parse-only or filesystem tooling. Compiler-pipeline commands
//! (`build`, `check`, `run`, `eval`, `shell`, `test`) live next
//! door in [`crate::pipeline`].

use std::path::{Path, PathBuf};
use std::{env, fs, process};

use koja_ast::util::dedent;
use koja_parser::ParseMode;

use crate::diagnostics::render_diagnostics;
use crate::loader::{
    self, ErrorPolicy, LoadOptions, LoadedSource, ProjectLoader, SourceOrigin, walk_source_files,
};
use crate::project::{self, ProjectConfig};
use crate::serve;

/// Returns the process's current directory, or prints an error to
/// stderr and exits non-zero.
fn current_dir_or_exit() -> PathBuf {
    env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: cannot determine current directory: {e}");
        process::exit(1);
    })
}

/// Loads `koja.toml` from the current directory, returning
/// `(config, cwd)`.
///
/// On a missing `koja.toml`, prints each line in `missing_message`
/// to stderr and exits non-zero. On any other error, prints
/// `error: {e}` and exits.
pub(crate) fn load_project_or_exit(missing_message: &[&str]) -> (ProjectConfig, PathBuf) {
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

/// One source file's worth of input for `koja doc`. The
/// `package` is the doc package the file's items will land
/// under, and `label` is the human-readable display path
/// (filesystem path for project + dep inputs, synthetic
/// `<Pkg.module>` marker for stdlib).
struct DocInput {
    kind: koja_doc::PackageKind,
    label: String,
    package: String,
    source: String,
}

/// `koja doc [file.koja ...] [-o output_dir]` -- generates HTML
/// documentation.
///
/// Bundles the project's own sources, every path dependency's
/// sources, and the embedded stdlib package set together so the
/// generated tree is a one-stop browsable reference. Pass
/// `--project-only` to skip stdlib + deps. With no positional
/// arguments, looks for `koja.toml` in the current directory.
pub fn cmd_doc(files: Vec<String>, output: String, project_only: bool, color: bool) {
    if !generate_docs(&files, &output, project_only, color) {
        return;
    }
    let out_path = Path::new(&output);
    println!("docs generated: {}", out_path.display());
}

/// `koja doc serve [-o output_dir] [--port N] [--no-rebuild]` --
/// generate (unless `no_rebuild`) and then host the doc tree on
/// `127.0.0.1`. Exists because the in-page fuzzy search reads
/// `search-index.json` via `fetch()`, which browsers refuse for
/// `file://` URLs. A local HTTP server is the standard workaround.
pub fn cmd_doc_serve(
    files: Vec<String>,
    output: String,
    project_only: bool,
    port: Option<u16>,
    no_rebuild: bool,
    color: bool,
) {
    if !no_rebuild && !generate_docs(&files, &output, project_only, color) {
        return;
    }
    let out_path = Path::new(&output);
    if let Err(e) = serve::run(out_path, port) {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

/// Drive the full discover -> parse -> extract -> render -> write
/// pipeline. Returns `false` when there's nothing to document so
/// the caller can decide whether to short-circuit (the bare
/// generator prints "docs generated", while `serve` would skip starting
/// the server). Fatal errors (output dir creation, file write)
/// `process::exit` from inside.
fn generate_docs(files: &[String], output: &str, project_only: bool, color: bool) -> bool {
    let (inputs, project_package) = discover_doc_inputs(files, project_only);
    if inputs.is_empty() {
        println!("no source files to document");
        return false;
    }

    let project = extract_doc_project(inputs, &project_package, color);
    if project.packages.is_empty() {
        println!("no items to document");
        return false;
    }

    let out_path = Path::new(output);
    if let Err(e) = fs::create_dir_all(out_path) {
        eprintln!("error creating output directory: {e}");
        process::exit(1);
    }

    write_doc_files(&project, out_path);
    true
}

/// Resolves the list of source files `koja doc` will process,
/// returning the inputs plus the project package name (used as
/// the sidebar header and the default-active package). Empty
/// `files` means project mode (walk `src` from `koja.toml` and
/// every dep's `src`). Otherwise treat each entry as a path or a
/// directory of `.koja` files. Stdlib + deps are bundled unless
/// `project_only` is true.
fn discover_doc_inputs(files: &[String], project_only: bool) -> (Vec<DocInput>, String) {
    if files.is_empty() {
        return discover_project_doc_inputs(project_only);
    }
    discover_explicit_doc_inputs(files, project_only)
}

/// Project-mode doc inputs: no files given, so load `src` from the
/// current `koja.toml` (exiting with usage help if there is none),
/// plus every dependency's `src` and the stdlib unless `project_only`.
/// The project package name doubles as the sidebar header.
fn discover_project_doc_inputs(project_only: bool) -> (Vec<DocInput>, String) {
    let (config, cwd) = load_project_or_exit(&[
        "error: no source file specified and no koja.toml found",
        "Usage: koja doc <file.koja ...> [-o output_dir]",
        "  or:  create a koja.toml in the current directory",
    ]);

    let loaded = ProjectLoader::new(&config, &cwd)
        .sources(LoadOptions {
            extensions: &["koja"],
            include_dependencies: !project_only,
            include_stdlib: !project_only,
            include_tests: false,
            on_error: ErrorPolicy::Lenient,
        })
        .unwrap_or_default();
    let inputs = loaded.into_iter().map(DocInput::from).collect();

    (inputs, config.name.clone())
}

/// Explicit-file doc inputs: each entry in `files` is a `.koja` file
/// or a directory of them, all tagged under the synthetic `Docs`
/// package. The stdlib is appended unless `project_only`.
fn discover_explicit_doc_inputs(files: &[String], project_only: bool) -> (Vec<DocInput>, String) {
    let project_package = "Docs".to_string();
    let mut inputs = Vec::new();
    for input in files {
        let p = Path::new(input);
        if p.is_dir() {
            collect_doc_inputs(
                p,
                &project_package,
                koja_doc::PackageKind::Project,
                &mut inputs,
            );
        } else if let Some(text) = read_doc_input(p) {
            inputs.push(DocInput {
                kind: koja_doc::PackageKind::Project,
                label: input.clone(),
                package: project_package.clone(),
                source: text,
            });
        }
    }
    if !project_only {
        inputs.extend(loader::stdlib_sources().into_iter().map(DocInput::from));
    }

    (inputs, project_package)
}

impl From<LoadedSource> for DocInput {
    fn from(source: LoadedSource) -> Self {
        let kind = match source.origin {
            SourceOrigin::Dependency => koja_doc::PackageKind::Dependency,
            SourceOrigin::Project => koja_doc::PackageKind::Project,
            SourceOrigin::Stdlib => koja_doc::PackageKind::Stdlib,
        };
        DocInput {
            kind,
            label: source.path.display().to_string(),
            package: source.package,
            source: source.source,
        }
    }
}

/// Parses every input file and extracts doc-renderable items
/// into a [`koja_doc::DocProject`] under the input's tagged
/// package. Files with parse errors are reported and skipped.
fn extract_doc_project(
    inputs: Vec<DocInput>,
    project_package: &str,
    color: bool,
) -> koja_doc::DocProject {
    let mut project = koja_doc::DocProject::new(project_package);

    for input in inputs {
        let parse_result = koja_parser::parse(&input.source, ParseMode::File);
        if !parse_result.errors.is_empty() {
            render_diagnostics(&input.label, &input.source, &parse_result.errors, color);
            continue;
        }

        koja_doc::extract_items(&parse_result.ast, &mut project, &input.package, input.kind);
    }

    koja_doc::finalize_project(&mut project);
    project
}

/// Renders the project as the subdir-per-package HTML tree.
/// Always emits `index.html` (package roster), `style.css`,
/// `search.js`, and `search-index.json` at the root, then one
/// subdirectory per documented package containing its
/// `index.html` plus a page per item.
fn write_doc_files(project: &koja_doc::DocProject, out_path: &Path) {
    write_doc_file(&out_path.join("style.css"), koja_doc::CSS);
    write_doc_file(&out_path.join("search.js"), koja_doc::SEARCH_JS);
    write_doc_file(
        &out_path.join("search-index.json"),
        &koja_doc::search_index_json(project),
    );
    write_doc_file(
        &out_path.join("index.html"),
        &koja_doc::render_root_index(project),
    );

    for pkg in &project.packages {
        if pkg.items.is_empty() {
            continue;
        }
        let pkg_dir = out_path.join(&pkg.name);
        if let Err(e) = fs::create_dir_all(&pkg_dir) {
            eprintln!(
                "error creating package directory {}: {e}",
                pkg_dir.display()
            );
            process::exit(1);
        }
        write_doc_file(
            &pkg_dir.join("index.html"),
            &koja_doc::render_package_index(pkg, project),
        );
        for c in &pkg.constants {
            let html = koja_doc::render_constant(c, pkg, project);
            write_doc_file(&pkg_dir.join(format!("{}.html", c.name)), &html);
        }
        for e in &pkg.enums {
            let html = koja_doc::render_enum(e, pkg, project);
            write_doc_file(&pkg_dir.join(format!("{}.html", e.name)), &html);
        }
        for f in &pkg.functions {
            let html = koja_doc::render_function(f, pkg, project);
            write_doc_file(&pkg_dir.join(format!("{}.html", f.name)), &html);
        }
        for p in &pkg.protocols {
            let html = koja_doc::render_protocol(p, pkg, project);
            write_doc_file(&pkg_dir.join(format!("{}.html", p.name)), &html);
        }
        for s in &pkg.structs {
            let html = koja_doc::render_struct(s, pkg, project);
            write_doc_file(&pkg_dir.join(format!("{}.html", s.name)), &html);
        }
    }
}

fn write_doc_file(path: &Path, content: &str) {
    if let Err(e) = fs::write(path, content) {
        eprintln!("error writing {}: {e}", path.display());
        process::exit(1);
    }
    println!("  {}", path.display());
}

/// Recursively collect `.koja` files from `dir`, reading each
/// into memory and tagging it with `package` + `kind` for the
/// doc pipeline.
fn collect_doc_inputs(
    dir: &Path,
    package: &str,
    kind: koja_doc::PackageKind,
    out: &mut Vec<DocInput>,
) {
    for path in walk_source_files(dir, &["koja"]) {
        if let Some(text) = read_doc_input(&path) {
            out.push(DocInput {
                kind,
                label: path.display().to_string(),
                package: package.to_string(),
                source: text,
            });
        }
    }
}

fn read_doc_input(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("error reading {}: {e}", path.display());
            None
        }
    }
}

/// `koja format [files...] [--check] [--write]` -- formats Koja
/// source files. With no arguments, looks for `koja.toml` and
/// formats all `.koja` files in the project's `src` and `test`
/// directories. Directory arguments are walked recursively for
/// `.koja` files.
pub fn cmd_format(files: Vec<String>, check: bool, write: bool, color: bool) {
    let resolved = resolve_format_paths(&files);

    let mut has_diff = false;
    let mut has_parse_errors = false;
    for path in &resolved {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                process::exit(1);
            }
        };

        let result = koja_fmt::format(&source, ParseMode::for_path(Path::new(path)));

        let formatted = match result {
            koja_fmt::FormatResult::Ok(s) => s,
            koja_fmt::FormatResult::ParseErrors(errors) => {
                render_diagnostics(path, &source, &errors, color);
                has_parse_errors = true;
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

    if has_parse_errors || (check && has_diff) {
        process::exit(1);
    }
}

/// Resolve which files `koja format` operates on: with no arguments,
/// the project's `src` + `test` trees, otherwise the explicit file and
/// directory arguments. Paths are sorted for deterministic output.
fn resolve_format_paths(files: &[String]) -> Vec<String> {
    if files.is_empty() {
        return project_format_paths();
    }
    explicit_format_paths(files)
}

/// Project-mode format targets: every `.koja`/`.kojs` file under the
/// current `koja.toml`'s `src` and `test` directories (exiting with
/// usage help when there is no project).
fn project_format_paths() -> Vec<String> {
    let (config, cwd) = load_project_or_exit(&[
        "error: no files specified and no koja.toml found",
        "Usage: koja format [files...] [--check] [--write]",
        "  or:  create a koja.toml in the current directory",
    ]);

    let roots = config.src.iter().chain(config.test.iter());
    let mut paths = Vec::new();
    for root in roots {
        let dir = cwd.join(root);
        if dir.is_dir() {
            paths.extend(walk_source_files(&dir, &["koja", "kojs"]));
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| p.to_str().map(String::from))
        .collect()
}

/// Explicit-argument format targets: directories are walked for source
/// files, while plain file arguments pass through verbatim so read
/// errors surface with the user's exact path.
fn explicit_format_paths(files: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    for input in files {
        let p = Path::new(input);
        if p.is_dir() {
            paths.extend(
                walk_source_files(p, &["koja", "kojs"])
                    .iter()
                    .filter_map(|f| f.to_str().map(String::from)),
            );
        } else {
            paths.push(input.clone());
        }
    }
    paths.sort();
    paths
}

/// `koja new <name>` -- scaffolds a new Koja project.
///
/// Creates a directory with `koja.toml` (`entry = "App"`),
/// `.gitignore`, `src/app.koja` (a minimal `Process` entry type plus
/// a `greet` helper), and `test/app_test.koja` (a placeholder
/// `@test` exercising `greet`). `koja build` and `koja test` both
/// succeed against the scaffold from the first command.
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

    let test_dir = project_dir.join("test");
    fs::create_dir_all(&test_dir).unwrap_or_else(|e| {
        eprintln!("error: cannot create directory: {e}");
        process::exit(1);
    });

    // Stamp the scaffolding compiler's minor as the minimum. Pre-1.0
    // minors are breaking, so a fresh project needs at least the
    // compiler that generated it.
    let minimum = env!("CARGO_PKG_VERSION")
        .rsplit_once('.')
        .map_or(env!("CARGO_PKG_VERSION"), |(minor, _)| minor);
    let toml_content = dedent(&format!(
        "
        [project]
        entry = \"App\"
        koja = \"{minimum}\"
        name = \"{name}\"
        version = \"0.1.0\"
        "
    ));
    fs::write(project_dir.join("koja.toml"), toml_content).unwrap_or_else(|e| {
        eprintln!("error: cannot write koja.toml: {e}");
        process::exit(1);
    });

    let gitignore_content = dedent(
        "
        /build
        /deps
        ",
    );
    fs::write(project_dir.join(".gitignore"), gitignore_content).unwrap_or_else(|e| {
        eprintln!("error: cannot write .gitignore: {e}");
        process::exit(1);
    });

    let app_content = dedent(
        "
        alias Process.Step
        alias Process.StopReason

        fn greet(name: String) -> String
          \"Hello, #{name}!\"
        end

        struct App
        end

        impl Process<(), (), ()> for App
          fn start(config: ()) -> Result<Self, StopReason>
            Result.Ok(App{})
          end

          fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
            Step.Continue(self)
          end

          fn run(self) -> StopReason
            IO.puts(greet(\"Koja\"))
            StopReason.Normal
          end
        end
        ",
    );
    fs::write(src_dir.join("app.koja"), app_content).unwrap_or_else(|e| {
        eprintln!("error: cannot write src/app.koja: {e}");
        process::exit(1);
    });

    let app_test_content = dedent(
        "
        struct AppTest
          @test \"greet builds a greeting message\"
          fn test_greet -> Result<Bool, String>
            actual = greet(\"Koja\")
            expected = \"Hello, Koja!\"

            unless actual == expected
              return Result.Err(\"expected `#{expected}`, got `#{actual}`\")
            end

            Result.Ok(true)
          end
        end
        ",
    );
    fs::write(test_dir.join("app_test.koja"), app_test_content).unwrap_or_else(|e| {
        eprintln!("error: cannot write test/app_test.koja: {e}");
        process::exit(1);
    });

    println!("created project '{name}'");
}

/// `koja parse <file.koja> [--emit-ast]` -- parses and reports
/// item count or errors.
///
/// With `--emit-ast`, prints the parsed AST to stdout using the
/// compact `koja_ast::format_file` tree (2-space indent, `@L:C-L:C`
/// span suffixes, exhaustive over every AST variant) instead of
/// the item-count line. Annotation slots like `Expr.resolved_type`
/// are `None` here -- no typecheck has run. Diagnostics still go
/// to stderr regardless.
pub fn cmd_parse(files: Vec<String>, color: bool, emit_ast: bool) {
    if files.is_empty() {
        eprintln!("Usage: koja parse <file.koja>");
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

        let mut result = koja_parser::parse(&source, ParseMode::File);
        // `koja_parser::parse` is the bare-string entry point that
        // leaves `ast.path` unset. Populate it from the CLI argument
        // so `--emit-ast` surfaces the file identity in the `File`
        // header line.
        result.ast.path = Some(PathBuf::from(path));

        if !result.errors.is_empty() {
            render_diagnostics(path, &source, &result.errors, color);
            continue;
        }

        if emit_ast {
            if index > 0 {
                println!();
            }
            print!("{}", koja_ast::format_file(&result.ast));
        } else {
            println!("{path}: OK ({} items)", result.ast.items.len());
        }
    }
}

/// `koja lex <file.koja>` -- lexes and prints every token with its position.
pub fn cmd_lex(files: Vec<String>, color: bool) {
    if files.is_empty() {
        eprintln!("Usage: koja lex <file.koja>");
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

        let result = koja_lexer::lex(&source);

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
