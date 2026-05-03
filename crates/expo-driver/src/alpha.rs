//! `expo alpha {eval,shell}` subcommand handlers.
//!
//! The `alpha` namespace hosts experimental subcommands that drive the
//! alpha compiler pipeline (`expo-alpha-typecheck → expo-alpha-ir →
//! expo-alpha-ir-eval`). Production users keep using `expo eval` /
//! `expo shell` (the v1 path); `expo alpha *` lets us iterate on the
//! alpha track end-to-end without touching the v1 surface.
//!
//! `cmd_eval` carries its own copy of the pipeline driver since it
//! runs a single source file and has no REPL state to thread. The
//! REPL itself lives in [`expo_alpha_shell`]; `cmd_shell` is just a
//! thin entry point that hands control off to it. When the alpha
//! shell grows file-input support both handlers will collapse into
//! `expo_alpha_shell` and this module will retire alongside the v1
//! `expo-shell` / `expo-ir-eval` crates.
//!
//! POC scope today (mirrors `expo-alpha-typecheck` / `expo-alpha-ir`):
//! integer literals, integer arithmetic (`+ - * / %`), parenthesized
//! groups. Anything richer typecheck-errors with a precise diagnostic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use expo_alpha_ir::lower_program;
use expo_alpha_ir_eval::{Interpreter, Value};
use expo_alpha_typecheck::{CheckFailure, check_program};
use expo_ast::ast::Diagnostic;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};

/// `expo alpha eval <file>` — run a single source file through the
/// alpha pipeline and print the entry function's [`Value`].
///
/// Mirrors `expo eval`'s contract for the print rule: `Value::Unit`
/// suppresses the trailing line so void entries don't print `()` (the
/// driver still exits 0). Any pipeline failure (filesystem, parse,
/// typecheck, lower, runtime) prints `error: <details>` to stderr
/// and exits 1.
pub fn cmd_eval(file: String, entry: Option<String>) {
    let path = Path::new(&file);
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("error: cannot read `{}`: {err}", path.display());
            process::exit(1);
        }
    };
    let package = derive_package(path);
    let entry_name = entry.as_deref().unwrap_or("main");
    match run_pipeline(source, &package, path.to_path_buf(), entry_name) {
        Ok(Value::Unit) => {}
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// `expo alpha shell` — interactive REPL on top of the alpha pipeline.
///
/// Delegates entirely to [`expo_alpha_shell::run`]; the REPL crate
/// owns Session state, multiline detection, command parsing, and its
/// own pipeline driver.
pub fn cmd_shell() {
    expo_alpha_shell::run();
}

/// Run one source file end-to-end through the alpha pipeline. Returns
/// the entry function's value on success, or a formatted error string
/// covering parse / typecheck / lower / runtime failures.
///
/// Parses in [`ParseMode::Script`] so `cmd_eval` accepts both the
/// legacy `fn main` shape and bare-statement script files; the
/// `lift_script` typecheck pass hoists script statements into a
/// synthetic entry point.
fn run_pipeline(
    source: String,
    package: &str,
    path: PathBuf,
    entry: &str,
) -> Result<Value, String> {
    let parsed = parse_program(
        vec![SourceFile {
            package: package.to_string(),
            path,
            source,
        }],
        ParseMode::Script,
    );
    let checked = check_program(parsed).map_err(format_check_failure)?;
    let entry_id = Identifier::new(package, vec![entry.to_string()]);
    let program = lower_program(&checked, entry_id).map_err(|err| err.to_string())?;
    Interpreter::new(program)
        .run()
        .map_err(|err| err.to_string())
}

/// Derive the package name from the source file's stem. Falls back to
/// `App` when the path has no usable stem; user-facing files always
/// have a stem in practice.
fn derive_package(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("App")
        .to_string()
}

/// Render a [`CheckFailure`] as the multi-line error string the
/// driver prints. Sources diagnostics from both the typecheck pass
/// itself and the partial parse output (parse errors live there, not
/// on `failure.diagnostics`).
fn format_check_failure(failure: CheckFailure) -> String {
    let CheckFailure {
        diagnostics,
        partial,
    } = failure;
    let parse_diags = parse_diagnostics(&partial);
    let parse_block = (!parse_diags.is_empty()).then(|| format_block("parse error", &parse_diags));
    let type_block = (!diagnostics.is_empty()).then(|| {
        format_block(
            "type error",
            diagnostics.iter().collect::<Vec<_>>().as_slice(),
        )
    });
    match (parse_block, type_block) {
        (Some(parse), Some(types)) => format!("{parse}\n{types}"),
        (Some(parse), None) => parse,
        (None, Some(types)) => types,
        (None, None) => "check failed with no diagnostics".to_string(),
    }
}

fn parse_diagnostics(parsed: &ParsedProgram) -> Vec<&Diagnostic> {
    parsed
        .files
        .values()
        .flat_map(|file| file.diagnostics.iter())
        .collect()
}

fn format_block(prefix: &str, diagnostics: &[&Diagnostic]) -> String {
    let mut out = format!("{prefix}:");
    for diag in diagnostics {
        out.push_str("\n  ");
        out.push_str(&diag.message);
    }
    out
}
