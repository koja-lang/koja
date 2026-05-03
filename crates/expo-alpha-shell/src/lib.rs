//! Interactive REPL for the alpha pipeline.
//!
//! `expo-alpha-shell` owns the alpha-side REPL: it accumulates user input
//! into a [`Session`], re-runs the whole session through
//! `expo-parser → expo-alpha-typecheck → expo-alpha-ir → expo-alpha-ir-eval`
//! on every step, and prints the trailing expression's value (if any).
//!
//! This crate is self-contained: it owns its own pipeline driver and depends
//! directly on the alpha pipeline crates, with no path back through
//! `expo-driver`. Today's only public surface is [`run`] (the REPL entry
//! point); when the shell grows file-input support it will gain an
//! `eval_source`-style helper that the driver's `cmd_eval` can delegate to.
//!
//! Mirrors the v1 [`expo-shell`](https://docs.rs/expo-shell) crate's role
//! relative to `expo eval` / `expo shell` — the alpha namespace is a clean
//! cut from v1, not an evolution of it.
//!
//! POC scope (mirrors `expo-alpha-typecheck` / `expo-alpha-ir`): integer
//! literals, integer arithmetic (`+ - * / %`), parenthesized groups.
//! Anything richer typecheck-errors with a precise diagnostic.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process;

use expo_alpha_ir::lower_program;
use expo_alpha_ir_eval::{Interpreter, Value};
use expo_alpha_typecheck::{CheckFailure, check_program};
use expo_ast::ast::Diagnostic;
use expo_ast::identifier::Identifier;
use expo_ast::token::TokenKind;
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};

/// Synthetic package name for the REPL session. The session re-runs
/// the entire concatenated input history through the alpha pipeline
/// on every step; lowering uses the same name when constructing the
/// entry-point [`Identifier`].
const SESSION_PACKAGE: &str = "REPL";

/// The session resolves its entry function under this name.
/// In script-mode parsing, [`expo_alpha_typecheck::lift_script`] hoists
/// any top-level statements (the REPL's accumulated input, or a
/// statements-only `.expo` file) into a synthesized `fn main`. Files
/// that already declare an explicit `fn main` use that one directly.
const SESSION_ENTRY: &str = "main";

const BANNER: &str = "expo alpha shell -- alpha IR interpreter (POC: integer arithmetic only)\n\
    Type :help for commands, :quit (or Ctrl-D) to exit\n";

const HELP: &str = "Commands:\n  \
    :help    show this message\n  \
    :quit    exit the shell\n  \
    :reset   clear session state and discard the current multiline buffer\n  \
    :state   print how many statement blocks the session is holding\n\
\n\
Notes:\n  \
    - State accumulates across inputs: each new input runs the whole\n    \
      session (today's pipeline is whole-program; incremental support\n    \
      lands later).\n  \
    - POC scope: integer literals, integer arithmetic (+, -, *, /, %),\n    \
      and parenthesized groups. Other constructs typecheck-error.\n";

/// Run the alpha REPL on stdin/stdout until `:quit` or EOF.
///
/// Reads stdin, accumulating each evaluated input into a [`Session`]
/// that re-runs the whole history every step. The trailing expression's
/// value (if any) gets printed; [`Value::Unit`] suppresses the print
/// line. Pipeline errors print `error: …` and roll the session back to
/// its pre-input state.
pub fn run() {
    print!("{BANNER}");
    let _ = io::stdout().flush();
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut session = Session::new();
    let mut buffer = String::new();
    loop {
        if io::stdin().is_terminal() {
            let prompt = if buffer.is_empty() {
                format!("expo({})> ", session.counter())
            } else {
                format!("....({})> ", session.counter())
            };
            print!("{prompt}");
            let _ = io::stdout().flush();
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
                println!("session: {} statement block(s)", session.statement_count());
                continue;
            }
        } else if trimmed == ":reset" {
            buffer.clear();
            continue;
        }
        buffer.push_str(&line);
        if !is_input_complete(&buffer) {
            continue;
        }
        let input = std::mem::take(&mut buffer);
        match session.try_eval(input.trim()) {
            Ok(Some(value)) => {
                println!("{value}");
                session.bump_counter();
            }
            Ok(None) => {
                session.bump_counter();
            }
            Err(error) => eprintln!("error: {error}"),
        }
    }
}

/// Accumulating REPL state. Each new input pushes one statement-text
/// block; [`Session::try_eval`] concatenates the entire history into
/// a single source string, parses it in [`ParseMode::Script`], and
/// drives it through the alpha pipeline. `lift_script` hoists the
/// top-level statements into a synthetic `fn main`; the pipeline
/// then resolves the trailing-expression value.
///
/// The pipeline is whole-program today (no incremental typecheck or
/// IR delta), so re-running the whole history is the simplest way to
/// make state "just work" — perf is fine for the first few hundred
/// lines. Future incremental work would split this into a chunk-based
/// representation; today's session is the reference shape.
struct Session {
    counter: u32,
    statements: Vec<String>,
}

impl Session {
    fn new() -> Self {
        Self {
            counter: 1,
            statements: Vec::new(),
        }
    }

    fn bump_counter(&mut self) {
        self.counter += 1;
    }

    fn clear(&mut self) {
        self.counter = 1;
        self.statements.clear();
    }

    fn counter(&self) -> u32 {
        self.counter
    }

    fn statement_count(&self) -> usize {
        self.statements.len()
    }

    /// Evaluate `input` against this session, mutating it on success
    /// (the input gets appended to the statement list) and rolling
    /// back on failure (the session is left exactly as it was before
    /// the call). `Ok(Some(value))` carries the trailing expression's
    /// value; `Ok(None)` covers `Value::Unit` so the REPL can
    /// suppress the trailing print line for void inputs.
    fn try_eval(&mut self, input: &str) -> Result<Option<Value>, String> {
        let snapshot = self.statements.len();
        self.statements.push(input.to_string());
        match self.run() {
            Ok(Value::Unit) => Ok(None),
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                self.statements.truncate(snapshot);
                Err(error)
            }
        }
    }

    /// Synthesize the full session source and drive it through the
    /// alpha pipeline.
    fn run(&self) -> Result<Value, String> {
        let source = self.synthesize();
        let path = PathBuf::from(format!("{SESSION_PACKAGE}.expo"));
        run_pipeline(source, SESSION_PACKAGE, path, SESSION_ENTRY)
    }

    /// Concatenate all statement blocks into the script source the
    /// pipeline will parse. Blocks are joined with newlines so each
    /// input remains its own logical line group; `ParseMode::Script`
    /// + `lift_script` handle the rest.
    fn synthesize(&self) -> String {
        self.statements.join("\n")
    }
}

/// Run one source string end-to-end through the alpha pipeline.
/// Returns the entry function's value on success, or a formatted
/// error string covering parse / typecheck / lower / runtime
/// failures.
///
/// Always parses in [`ParseMode::Script`]: the alpha surface treats
/// top-level statements as first-class. Files that already have an
/// explicit `fn main` parse identically in either mode, so this is
/// strictly a superset.
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

/// True when `source` (the accumulated multiline buffer) is a
/// well-formed-enough Expo fragment to hand to the parser: every
/// block-opener has its `end`, every bracket pair is closed, and no
/// string literal is left dangling. Implemented over the lexer rather
/// than the parser because the lexer is cheap to re-run on every
/// keystroke and gives precise token-level state.
///
/// Conservative on ambiguity: an input that looks complete by token
/// counting but actually fails to parse will still be handed to
/// [`Session::try_eval`] and produce a parse error — the user can
/// retry.
fn is_input_complete(source: &str) -> bool {
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

/// Render a [`CheckFailure`] as the multi-line error string the REPL
/// prints. Sources diagnostics from both the typecheck pass itself
/// and the partial parse output (parse errors live there, not on
/// `failure.diagnostics`).
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
