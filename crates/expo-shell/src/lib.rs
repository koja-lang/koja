//! `expo shell` -- interactive REPL backed by the IR interpreter
//! ([`expo_ir_eval::Interp`]).
//!
//! ## Session model
//!
//! The shell maintains an accumulating [`Session`] across inputs:
//! every type / function / impl declaration goes into the session's
//! item list, and every assignment / expression statement goes into
//! the session's statement list. On each input the whole session is
//! re-synthesized as a single Expo module, re-typechecked, re-lowered,
//! and re-interpreted; the new input's trailing expression (if any)
//! is rendered as the print result. Pure declarations / assignments
//! print nothing (matches Elixir / Python REPL behaviour).
//!
//! Re-execution is the simplest way to make state "just work" without
//! incremental typecheck / codegen / interpreter caches. Performance
//! is fine for the first few hundred input lines; longer sessions can
//! be incrementalized in a future slice (typecheck delta, persistent
//! interpreter `Frame`, etc.).
//!
//! See [`session`] for the [`Session`] struct and the per-input
//! re-execution pipeline.
//!
//! ## Multiline + commands
//!
//! - Multiline input is detected by lexing the accumulated buffer and
//!   continuing while block-opener keywords (`if`, `cond`, `match`,
//!   `fn`, `struct`, ...) outnumber `end`s, brackets are unbalanced,
//!   or a string literal is unterminated.
//! - No project loading (`-S` is accepted and ignored).
//! - No stdlib auto-import; programs that need it (lists, maps,
//!   methods on built-in types) currently fail with an interpreter
//!   error.
//! - Coverage matches what the IR lowerer produces without
//!   `IRInstruction::Stub`: literals, arithmetic, function calls,
//!   `if`/`else`, local bindings, struct + enum construction. Each
//!   upstream lowering lift widens what works in the REPL.
//!
//! Lifecycle:
//!
//! - Banner on entry.
//! - Prompt `expo(N)> ` for a fresh input, `....(N)> ` for a
//!   continuation line; `N` increments per evaluated input.
//! - `:quit` (or EOF / Ctrl-D) exits with status 0.
//! - `:help` prints the command list.
//! - `:reset` discards the current multiline buffer AND the
//!   accumulated session state.
//! - `:state` prints how many items / statements the session holds.

mod session;

pub use session::Session;

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use expo_ast::ast::{File, Item};
use expo_ast::token::TokenKind;
use expo_ir::{Backend, FunctionIdentifier};
use expo_ir_eval::{Interp, Value};

const BANNER: &str = "Expo shell -- interactive REPL backed by the IR interpreter\n\
    Type :help for commands, :quit (or Ctrl-D) to exit\n";

/// Synthetic name [`eval_file`] renames `fn main` to before typecheck,
/// so it survives lowering as a regular Free function the interpreter
/// can dispatch. Reserved -- user code shouldn't shadow it.
const SYNTHETIC_EVAL_ENTRY: &str = "__expo_eval_entry__";

const HELP: &str = "Commands:\n  \
    :help    show this message\n  \
    :quit    exit the shell\n  \
    :reset   clear session state and discard the current multiline buffer\n  \
    :state   print how many items / statements the session is holding\n\
\n\
Notes:\n  \
    - State accumulates across inputs: type / fn / impl declarations\n    \
      and variable assignments persist until `:reset`.\n  \
    - Multiline input is detected automatically; `....(N)> ` is the\n    \
      continuation prompt. Use `:reset` to abandon a multiline buffer.\n  \
    - Only constructs the IR lowerer handles without `Stub` work today\n    \
      (literals, arithmetic, function calls, if/else, local bindings,\n    \
      struct + enum construction).\n";

/// Entry point invoked from `expo-driver`'s `Command::Shell` arm.
/// Reads input from stdin, evaluates each completed input through the
/// IR interpreter, and prints the result. Exits when stdin closes or
/// the user types `:quit`.
///
/// `project` is currently accepted-and-ignored; project loading is a
/// future enhancement (see Design B in the roadmap).
pub fn run(project: Option<PathBuf>, _color: bool) {
    if project.is_some() {
        eprintln!("note: -S/--project loading is not yet implemented");
    }
    print!("{BANNER}");
    let _ = io::stdout().flush();
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut session = Session::new();
    let mut buffer = String::new();
    let mut typed_lines: Vec<String> = Vec::new();
    let mut multiline_rewritten = false;
    loop {
        let is_tty = io::stdin().is_terminal() && io::stdout().is_terminal();
        if io::stdin().is_terminal() {
            let prompt = if buffer.is_empty() {
                Some(format!("expo({})> ", session.counter()))
            } else if multiline_rewritten {
                None
            } else {
                Some(format!("....({})> ", session.counter()))
            };
            if let Some(prompt) = prompt {
                print!("{prompt}");
                let _ = io::stdout().flush();
            }
        }
        let mut line = String::new();
        match handle.read_line(&mut line) {
            Ok(0) => {
                if !buffer.is_empty() {
                    eprintln!("\nerror: unterminated input discarded at EOF");
                }
                println!();
                break;
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("error reading input: {error}");
                process::exit(1);
            }
        }
        let trimmed = line.trim();
        if buffer.is_empty() {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == ":quit" {
                break;
            }
            if trimmed == ":help" {
                print!("{HELP}");
                continue;
            }
            if trimmed == ":reset" {
                session.clear();
                continue;
            }
            if trimmed == ":state" {
                println!(
                    "session: {} item block(s), {} statement block(s)",
                    session.item_count(),
                    session.statement_count()
                );
                continue;
            }
        } else if trimmed == ":reset" {
            buffer.clear();
            typed_lines.clear();
            multiline_rewritten = false;
            continue;
        }
        buffer.push_str(&line);
        typed_lines.push(line.trim_end_matches(['\n', '\r']).to_string());
        if !is_input_complete(&buffer) {
            // First time we discover the input is multiline: drop the
            // user's typed line off the prompt row and onto its own
            // bare line so subsequent reads come in without the
            // `....(N)>` continuation prompt cluttering each row.
            if is_tty && !multiline_rewritten && typed_lines.len() == 1 {
                erase_lines(1);
                println!("expo({})>", session.counter());
                println!("{}", typed_lines[0]);
                multiline_rewritten = true;
            }
            continue;
        }
        let input = std::mem::take(&mut buffer);
        let consumed_lines = std::mem::take(&mut typed_lines);
        let was_multiline_rewritten = multiline_rewritten;
        multiline_rewritten = false;
        let result = session.try_eval(input.trim());
        if is_tty
            && result.is_ok()
            && let Some(rendered) = format_for_display(input.trim())
        {
            let lines_on_screen = if was_multiline_rewritten {
                1 + consumed_lines.len()
            } else {
                consumed_lines.len()
            };
            erase_lines(lines_on_screen);
            let counter = session.counter();
            let mut formatted_lines = rendered.lines();
            let first = formatted_lines.next().unwrap_or("");
            if rendered.lines().count() <= 1 {
                println!("expo({counter})> {first}");
            } else {
                println!("expo({counter})>");
                println!("{first}");
                for l in formatted_lines {
                    println!("{l}");
                }
            }
        }
        match result {
            Ok(Some(rendered)) => {
                println!("{rendered}");
                session.bump_counter();
            }
            Ok(None) => {
                session.bump_counter();
            }
            Err(error) => eprintln!("error: {error}"),
        }
    }
}

/// Run `input` through the formatter for redisplay. Returns the
/// formatted text without a trailing newline. Module-shape inputs
/// (`struct`, `fn`, `enum`, `impl`, ...) parse and format directly;
/// statement-shape inputs (assignments, expressions, `let`) get
/// wrapped in a synthetic `fn __fmt__` so the formatter can pretty
/// the body, then the wrapper is stripped. Returns `None` when the
/// formatter parse-errors -- callers leave the screen alone in that
/// case.
fn format_for_display(input: &str) -> Option<String> {
    use expo_fmt::{FormatResult, format};

    if let FormatResult::Ok(rendered) = format(input)
        && !rendered.trim().is_empty()
    {
        return Some(rendered.trim_end().to_string());
    }
    let wrapped = format!("fn __fmt__\n{input}\nend\n");
    let FormatResult::Ok(rendered) = format(&wrapped) else {
        return None;
    };
    extract_function_body(&rendered, "__fmt__")
}

/// Pull the body lines out of a formatter-rendered `fn <name>` block,
/// stripping the leading two-space indent the formatter emits. Used
/// to unwrap statement-shape inputs that we wrapped to coax the
/// formatter into pretty-printing them. Returns `None` if the
/// expected shape isn't found.
fn extract_function_body(formatted: &str, name: &str) -> Option<String> {
    let header = format!("fn {name}");
    let mut lines = formatted.lines();
    while let Some(line) = lines.next() {
        if line.trim_start() == header {
            let mut body = Vec::new();
            for body_line in lines.by_ref() {
                if body_line.trim() == "end" {
                    return Some(body.join("\n"));
                }
                body.push(
                    body_line
                        .strip_prefix("  ")
                        .unwrap_or(body_line)
                        .to_string(),
                );
            }
            return None;
        }
    }
    None
}

/// Move up `n` rows in the terminal, erasing each one. Leaves the
/// cursor at column 0 of the topmost erased row, ready for fresh
/// `println!` output to overwrite the previous content. No-op on
/// `n == 0`. Caller is responsible for gating on `is_terminal()`.
fn erase_lines(n: usize) {
    if n == 0 {
        return;
    }
    let mut out = io::stdout().lock();
    for _ in 0..n {
        let _ = write!(out, "\r\x1b[2K\x1b[A");
    }
    let _ = write!(out, "\r\x1b[2K");
    let _ = out.flush();
}

/// Evaluate `path` through the IR interpreter and return the entry
/// function's result. Returns `Ok(None)` for `Unit` returns so callers
/// can suppress the trailing print line for void entries.
///
/// `entry` selects which function the interpreter calls; defaults to
/// `"main"`. The interpreter rejects [`expo_ir::IRFunctionKind::MainEntry`]
/// today, so calling a user `fn main` reports a precise error until
/// `fn main` lowering moves into the IR.
pub fn eval_file(path: &Path, entry: Option<&str>) -> Result<Option<Value>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let mut module = parse_file(&source)?;
    module.path = Some(path.to_path_buf());
    let entry_name = match entry {
        Some(name) => name.to_string(),
        None => rename_main_for_eval(&mut module).unwrap_or_else(|| "main".to_string()),
    };
    let package = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("__eval__")
        .to_string();
    let type_ctx = expo_typecheck::check(&mut module);
    let modules = vec![&module];
    let packages = vec![package.as_str()];
    let program = expo_codegen::lower_files(&modules, &packages, &type_ctx, &package, None)
        .map_err(|diagnostics| format_diagnostics(&diagnostics))?;
    let mut interp =
        Interp::new(Arc::new(program), Arc::new(type_ctx)).map_err(|error| error.to_string())?;
    let value = interp
        .call(&FunctionIdentifier::new(&entry_name), Vec::new())
        .map_err(|error| error.to_string())?;
    Ok(if matches!(value, Value::Unit) {
        None
    } else {
        Some(value)
    })
}

/// Rename a top-level `fn main` to [`SYNTHETIC_EVAL_ENTRY`] so the
/// interpreter can dispatch it. `fn main` registers as
/// [`expo_ir::IRFunctionKind::MainEntry`] today (the LLVM-only C entry
/// pair); the interpreter has no body to walk for that kind. Renaming
/// before typecheck makes codegen register it as a regular Free
/// function with populated IR blocks. Returns the synthetic name when
/// a `main` was found.
///
/// Skipped when the caller supplied an explicit `--entry`; that
/// function is dispatched by its source name.
fn rename_main_for_eval(module: &mut File) -> Option<String> {
    for item in &mut module.items {
        if let Item::Function(function) = item
            && function.name == "main"
        {
            function.name = SYNTHETIC_EVAL_ENTRY.to_string();
            return Some(SYNTHETIC_EVAL_ENTRY.to_string());
        }
    }
    None
}

/// True when `source` (the accumulated multiline buffer) is a
/// well-formed-enough Expo fragment to hand to the parser: every
/// block-opener has its `end`, every bracket pair is closed, and no
/// string literal is left dangling. Implemented over the lexer rather
/// than the parser because the lexer is cheap to re-run on every
/// keystroke and gives precise token-level state.
///
/// Conservative on ambiguity: an input that looks complete by token
/// counting but actually fails to parse will still be handed to
/// [`Session::try_eval`] and produce a parse error -- the user can
/// retry.
pub fn is_input_complete(source: &str) -> bool {
    let mut block_depth: i32 = 0;
    let mut paren_depth: i32 = 0;
    let mut brace_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut string_depth: i32 = 0;
    let mut interpol_depth: i32 = 0;
    for token in expo_lexer::lex(source).tokens {
        match token.kind {
            TokenKind::Arena
            | TokenKind::Cond
            | TokenKind::Enum
            | TokenKind::Fn
            | TokenKind::For
            | TokenKind::If
            | TokenKind::Impl
            | TokenKind::Loop
            | TokenKind::Match
            | TokenKind::Protocol
            | TokenKind::Receive
            | TokenKind::Struct
            | TokenKind::Unless
            | TokenKind::While => block_depth += 1,
            TokenKind::End => block_depth -= 1,
            TokenKind::LParen => paren_depth += 1,
            TokenKind::RParen => paren_depth -= 1,
            TokenKind::LBrace => brace_depth += 1,
            TokenKind::RBrace => brace_depth -= 1,
            TokenKind::LBracket => bracket_depth += 1,
            TokenKind::RBracket => bracket_depth -= 1,
            TokenKind::StringStart | TokenKind::MultilineStringStart => string_depth += 1,
            TokenKind::StringEnd | TokenKind::MultilineStringEnd => string_depth -= 1,
            TokenKind::InterpolStart => interpol_depth += 1,
            TokenKind::InterpolEnd => interpol_depth -= 1,
            _ => {}
        }
    }
    block_depth <= 0
        && paren_depth <= 0
        && brace_depth <= 0
        && bracket_depth <= 0
        && string_depth <= 0
        && interpol_depth <= 0
}

/// Parse `source` into an [`expo_ast::ast::File`], returning a
/// formatted multi-line error string when the parser produces
/// diagnostics. Shared between [`eval_file`] and the session
/// pipeline.
pub(crate) fn parse_file(source: &str) -> Result<File, String> {
    let parsed = expo_parser::parse(source);
    if !parsed.errors.is_empty() {
        let messages: Vec<String> = parsed
            .errors
            .iter()
            .map(|error| format!("  {}", error.message))
            .collect();
        return Err(format!("parse error:\n{}", messages.join("\n")));
    }
    Ok(parsed.ast)
}

/// Format codegen / typecheck diagnostics into a multi-line error
/// string the REPL prints under an `error: ` prefix. Shared between
/// [`eval_file`] and the session pipeline.
pub(crate) fn format_diagnostics(diagnostics: &[expo_ast::ast::Diagnostic]) -> String {
    let messages: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| format!("  {}", diagnostic.message))
        .collect();
    format!("type / lower errors:\n{}", messages.join("\n"))
}
