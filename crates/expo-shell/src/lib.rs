//! Interactive REPL for the pipeline.
//!
//! `expo-shell` owns the runtime-side REPL: it accumulates user input
//! into a [`Session`], re-runs the whole session through
//! `expo-parser → expo-typecheck → expo-ir → expo-ir-eval`
//! on every step, and prints the trailing expression's value (if any).
//! Each fragment is lowered as a script (`lower_script` →
//! `Interpreter::run_script`); top-level expressions and assignments
//! are first-class, and any user-typed `fn` definitions land as
//! helper functions in the script's package fragment.
//!
//! REPL fragments have no file dimension, so the shell bypasses the
//! `.exps` / `.expo` extension dispatch that `expo {build,run,
//! eval,check}` use — every fragment is unconditionally script-mode.
//!
//! This crate is self-contained: it owns its own pipeline driver and depends
//! directly on the pipeline crates, with no path back through
//! `expo-driver`. Today's only public surface is [`run`] (the REPL entry
//! point); when the shell grows file-input support it will gain an
//! `eval_source`-style helper that the driver's `cmd_run --backend=interpreter`
//! path can delegate to.
//!
//! Mirrors the v1 [`expo-shell`](https://docs.rs/expo-shell) crate's role
//! relative to `expo eval` / `expo shell` — the mangled namespace is a clean
//! cut from v1, not an evolution of it.
//!
//! Today's scope mirrors `expo-typecheck` / `expo-ir`:
//! integer literals, integer arithmetic (`+ - * / %`), boolean and
//! comparison operators, and parenthesized groups. Richer constructs
//! typecheck-error with a precise diagnostic.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process;

use expo_ast::ast::Diagnostic;
use expo_ast::token::TokenKind;
use expo_ir::{IRScript, lower_script};
use expo_ir_eval::{Interpreter, Value};
use expo_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};
use expo_typecheck::{CheckFailure, check_program};
use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;

/// Synthetic package name for the REPL session. The session re-runs
/// the entire concatenated input history through the pipeline
/// on every step; the package label flows through into any helper
/// functions the user defines via top-level `fn` items.
const SESSION_PACKAGE: &str = "REPL";

const BANNER: &str = "expo shell -- IR interpreter\n\
    Type :help for commands, :quit (or Ctrl-D) to exit\n";

const HELP: &str = "Commands:\n  \
    :help    show this message\n  \
    :quit    exit the shell\n  \
    :reset   clear session state (or abandon a partial multi-line input)\n  \
    :state   print how many statement blocks the session is holding\n\
\n\
Notes:\n  \
    - Multi-line blocks (struct, enum, fn, ...) rewrite the prompt onto\n    \
      its own line so the block reads as bare code; Ctrl-C or :reset\n    \
      abandons an in-flight multi-line input.\n  \
    - Up-arrow recalls previous inputs (in-memory, per session).\n  \
    - State accumulates across inputs: each new input runs the whole\n    \
      session (today's pipeline is whole-program; incremental support\n    \
      lands later).\n  \
    - Scope today: integer literals, integer arithmetic (+, -, *, /, %),\n    \
      boolean / comparison operators, and parenthesized groups.\n    \
      Other constructs typecheck-error.\n";

/// Outcome of one [`read_input`] call: either a complete buffer
/// ready for the pipeline, the user bailed mid-multiline
/// (`Ctrl-C` / `:reset`), the user hit Ctrl-D on a fresh prompt,
/// or an unrecoverable line-editor error occurred.
enum InputOutcome {
    Buffer(String),
    Cancelled,
    Eof,
    Fatal(String),
}

/// Run the REPL on stdin/stdout until `:quit`, `Ctrl-D`, or an
/// unrecoverable line-editor error.
///
/// Drives a [`rustyline::Editor`] one line at a time via
/// [`read_input`], which rewrites the prompt onto its own row
/// the first time an input proves to be multi-line so the typed
/// block reads as bare code; subsequent continuation lines come
/// in without any prompt prefix. Successful inputs accumulate
/// into a [`Session`] that re-runs the whole history every step;
/// the trailing expression's value (if any) gets printed and a
/// [`Value::Unit`] trailing value suppresses the print line.
/// Pipeline errors print `error: …` and roll the session back to
/// its pre-input state. Ctrl-C cancels the in-flight input and
/// loops back to a fresh prompt; Ctrl-D / EOF exits cleanly.
///
/// History is kept in memory only — each accepted input (the full
/// multi-line block, where applicable) is added as one entry so
/// up-arrow recalls prior commands within the session, but
/// nothing is persisted to disk.
pub fn run() {
    print!("{BANNER}");
    let _ = io::stdout().flush();

    let mut editor = match Editor::<(), DefaultHistory>::new() {
        Ok(editor) => editor,
        Err(err) => {
            eprintln!("error: failed to initialize line editor: {err}");
            process::exit(1);
        }
    };

    let mut session = Session::new();
    loop {
        let input = match read_input(&mut editor, session.counter()) {
            InputOutcome::Buffer(input) => input,
            InputOutcome::Cancelled => continue,
            InputOutcome::Eof => {
                println!();
                break;
            }
            InputOutcome::Fatal(message) => {
                eprintln!("{message}");
                process::exit(1);
            }
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed {
            ":quit" => break,
            ":help" => {
                print!("{HELP}");
                continue;
            }
            ":reset" => {
                session.clear();
                continue;
            }
            ":state" => {
                println!("session: {} statement block(s)", session.statement_count());
                continue;
            }
            _ => {}
        }
        let _ = editor.add_history_entry(input.as_str());
        match session.try_eval(trimmed) {
            Ok(Some(rendered)) => {
                println!("\x1b[90m{rendered}\x1b[0m");
                session.bump_counter();
            }
            Ok(None) => {
                session.bump_counter();
            }
            Err(error) => eprintln!("error: {error}"),
        }
    }
}

/// Read one complete REPL input (possibly multi-line). On the
/// first line, rustyline shows the standard `expo(N)>` prompt
/// with full editing + history. If that line alone isn't a
/// complete fragment (open block, unclosed bracket, dangling
/// string), the prompt + typed line are rewritten via ANSI so
/// the prompt sits on its own row and the typed first line drops
/// onto its own row below; the block then reads as bare code.
/// Subsequent reads happen with an empty prompt until
/// [`is_input_complete`] flips true.
///
/// Returns [`InputOutcome::Cancelled`] if the user hits Ctrl-C
/// (on any line) or types `:reset` mid-multiline, discarding the
/// partial buffer in either case. Ctrl-D / EOF mid-multiline
/// surfaces as a Cancelled outcome with an "unterminated input
/// discarded at EOF" message; Ctrl-D on the first read returns
/// [`InputOutcome::Eof`].
fn read_input(editor: &mut Editor<(), DefaultHistory>, counter: u32) -> InputOutcome {
    let prompt = format!("expo({counter})> ");
    let first = match editor.readline(&prompt) {
        Ok(line) => line,
        Err(ReadlineError::Eof) => return InputOutcome::Eof,
        Err(ReadlineError::Interrupted) => return InputOutcome::Cancelled,
        Err(error) => return InputOutcome::Fatal(format!("error reading input: {error}")),
    };
    if is_input_complete(&first) {
        return InputOutcome::Buffer(first);
    }

    // The first line proves the input is multi-line. Drop the
    // prompt onto its own row, the typed line below it, and read
    // subsequent lines with an empty prompt so the block reads as
    // bare code on the terminal.
    if io::stdout().is_terminal() {
        erase_current_line();
        println!("expo({counter})>");
        println!("{first}");
    }

    let mut buffer = String::with_capacity(first.len() + 16);
    buffer.push_str(&first);
    buffer.push('\n');

    loop {
        let line = match editor.readline("") {
            Ok(line) => line,
            Err(ReadlineError::Eof) => {
                eprintln!("error: unterminated input discarded at EOF");
                return InputOutcome::Cancelled;
            }
            Err(ReadlineError::Interrupted) => return InputOutcome::Cancelled,
            Err(error) => return InputOutcome::Fatal(format!("error reading input: {error}")),
        };
        if line.trim() == ":reset" {
            return InputOutcome::Cancelled;
        }
        buffer.push_str(&line);
        buffer.push('\n');
        if is_input_complete(&buffer) {
            return InputOutcome::Buffer(buffer);
        }
    }
}

/// Cursor up one row, carriage return, erase the entire row.
/// Issued the moment the first read returns an incomplete
/// fragment so the typed content "drops down" onto its own bare
/// line.
fn erase_current_line() {
    print!("\x1b[1A\r\x1b[2K");
    let _ = io::stdout().flush();
}

/// Accumulating REPL state. Each new input pushes one statement-text
/// block; [`Session::try_eval`] concatenates the entire history into
/// a single source string, parses it in [`ParseMode::Script`], and
/// drives it through the pipeline as an [`expo_ir::IRScript`]:
/// `expo_ir::lower_script` produces an implicit-function body
/// for the top-level statements, and `Interpreter::run_script` runs
/// it. Helper `fn` items the user defines land as helpers on
/// `IRScript.packages`.
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
    /// back on pipeline failure (parse / typecheck / lower / runtime
    /// errors leave the session exactly as it was before the call).
    ///
    /// `Ok(Some(rendered))` carries the trailing expression's
    /// `Debug.format` output, falling back to the runtime
    /// [`Display`] when `Debug.format` isn't applicable
    /// (primitives, containers) or when its instance wasn't
    /// monomorphized into the session IR. The statement evaluated
    /// successfully in either case (side effects already landed),
    /// so we don't roll the session back. `Ok(None)` covers
    /// [`Value::Unit`] so the REPL suppresses the trailing print
    /// line for void inputs — including calls to functions like
    /// `IO.puts` whose signature elides `-> T` and which the
    /// interpreter coerces to [`Value::Unit`] at the call boundary.
    fn try_eval(&mut self, input: &str) -> Result<Option<String>, String> {
        let snapshot = self.statements.len();
        self.statements.push(input.to_string());
        match self.run() {
            Ok((_, Value::Unit)) => Ok(None),
            Ok((script, value)) => {
                let rendered = match Interpreter::format_via_debug(&script, value.clone()) {
                    Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                    Ok(None) | Err(_) => value.to_string(),
                };
                Ok(Some(rendered))
            }
            Err(error) => {
                self.statements.truncate(snapshot);
                Err(error)
            }
        }
    }

    /// Synthesize the full session source and drive it through the
    /// pipeline. Returns both the sealed [`IRScript`] and the
    /// trailing value so the caller can dispatch follow-up helpers
    /// (e.g. `Debug.format` auto-print) without re-lowering.
    fn run(&self) -> Result<(IRScript, Value), String> {
        let source = self.synthesize();
        let path = PathBuf::from(format!("{SESSION_PACKAGE}.expo"));
        run_pipeline(source, SESSION_PACKAGE, path)
    }

    /// Concatenate all statement blocks into the script source the
    /// pipeline will parse. Blocks are joined with newlines so each
    /// input remains its own logical line group; `ParseMode::Script`
    /// handles the rest.
    fn synthesize(&self) -> String {
        self.statements.join("\n")
    }
}

/// Run one source string end-to-end through the script-mode
/// pipeline. Returns the sealed [`IRScript`] alongside the trailing
/// value so the caller can dispatch follow-up helpers (e.g.
/// `Debug.format` auto-print) without re-lowering. On failure
/// returns a formatted error string covering parse / typecheck /
/// lower / runtime failures.
///
/// Always parses in [`ParseMode::Script`]: the REPL treats top-level
/// statements as first-class. Helper `fn` items land on
/// [`expo_ir::IRScript::packages`] for [`Interpreter::run_script`]
/// to resolve as call targets.
///
/// Prepends [`expo_stdlib::autoimport_sources`] so REPL input
/// sees the same `Global.*` prelude the driver and tests do —
/// `Duration.from_secs(3).millis()` and `0b1100.band(0b1010)` work
/// at the prompt without any imports.
fn run_pipeline(source: String, package: &str, path: PathBuf) -> Result<(IRScript, Value), String> {
    let mut sources = expo_stdlib::autoimport_sources();
    sources.push(SourceFile {
        package: package.to_string(),
        path,
        source,
    });
    let parsed = parse_program(sources, ParseMode::Script);
    let checked = check_program(parsed).map_err(format_check_failure)?;
    let script = lower_script(&checked).map_err(|err| err.to_string())?;
    let value = Interpreter::run_script(&script).map_err(|err| err.to_string())?;
    Ok((script, value))
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
            TokenKind::Cond
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
