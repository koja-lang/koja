//! Interactive REPL for the pipeline.
//!
//! `koja-shell` owns the runtime-side REPL: it accumulates user input
//! into a [`Session`] and, on every step, re-runs the whole session
//! through `koja-parser -> koja-typecheck -> koja-ir -> koja-ir-eval`,
//! printing the trailing expression's value (if any). Each step is
//! lowered as a script (`lower_script` -> `Interpreter::run_script`), so
//! top-level expressions, assignments, and `fn` definitions are all
//! first-class.
//!
//! The driver supplies the baseline source set (stdlib, plus a
//! project's sources when launched inside one) and the package the
//! session evaluates in — see [`run`]. REPL fragments have no file
//! dimension, so the shell is unconditionally script-mode, bypassing
//! the `.koja` / `.kojs` dispatch the other `koja` subcommands use. It
//! depends only on the pipeline crates, never back on `koja-driver`.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;

use koja_ast::ast::{Diagnostic, Expr, ExprKind, Statement};
use koja_ast::token::TokenKind;
use koja_ir::{IRScript, IRType, lower_script};
use koja_ir_eval::{Interpreter, Value};
use koja_parser::{ParseMode, ParsedProgram, SourceFile, parse_program};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program};
use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;

use crate::complete::{CompletionContext, ShellHelper};

mod complete;

/// Default session package for a bare `koja shell` (no project). The
/// session re-runs the entire concatenated input history through the
/// pipeline on every step; the package label flows through into any
/// helper functions the user defines via top-level `fn` items. In a
/// project the driver passes the project's package name instead, so
/// project modules resolve unqualified (`Cli.usage()`) just as they do
/// from the project's own files.
pub const SESSION_PACKAGE: &str = "REPL";

const BANNER: &str = "koja shell -- IR interpreter\n\
    Type :help for commands, :quit (or Ctrl-D) to exit\n";

/// REPL commands, offered by tab completion and matched literally in
/// [`run`]'s dispatch.
const COMMANDS: &[&str] = &[":help", ":quit", ":reset", ":state"];

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
    - Tab completes keywords, types, functions, session variables,\n    \
      `Type.` members, and `value.` methods and fields.\n  \
    - Up-arrow recalls previous inputs (in-memory, per session).\n  \
    - State accumulates across inputs: each new input runs the whole\n    \
      session (today's pipeline is whole-program; incremental support\n    \
      lands later).\n  \
    - In a project, its modules are in scope and resolve unqualified\n    \
      (e.g. `Cli.usage()`); the stdlib prelude is always available.\n";

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
/// the trailing expression's `Debug.format` rendering (if any) gets
/// printed and a [`Value::Unit`] trailing value suppresses the line.
/// Pipeline errors print `error: …` and roll the session back to
/// its pre-input state. Ctrl-C cancels the in-flight input and
/// loops back to a fresh prompt; Ctrl-D / EOF exits cleanly.
///
/// History is kept in memory only — each accepted input (the full
/// multi-line block, where applicable) is added as one entry so
/// up-arrow recalls prior commands within the session, but
/// nothing is persisted to disk.
pub fn run(baseline: Vec<SourceFile>, session_package: String) {
    print!("{BANNER}");
    if !baseline.is_empty() {
        println!("{} source file(s) in scope", baseline.len());
    }
    let _ = io::stdout().flush();

    let mut editor = match Editor::<ShellHelper, DefaultHistory>::new() {
        Ok(editor) => editor,
        Err(err) => {
            eprintln!("error: failed to initialize line editor: {err}");
            process::exit(1);
        }
    };

    let mut session = Session::new(baseline, session_package);
    editor.set_helper(Some(ShellHelper::new(session.initial_completion())));
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
            Ok(outcome) => {
                if let Some(rendered) = outcome.rendered {
                    println!("\x1b[90m{rendered}\x1b[0m");
                }
                session.bump_counter();
                if let Some(helper) = editor.helper_mut() {
                    helper.set_context(outcome.completion);
                }
            }
            Err(error) => eprintln!("error: {error}"),
        }
    }
}

/// Read one complete REPL input (possibly multi-line). On the
/// first line, rustyline shows the standard `koja(N)>` prompt
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
fn read_input(editor: &mut Editor<ShellHelper, DefaultHistory>, counter: u32) -> InputOutcome {
    let prompt = format!("koja({counter})> ");
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
        println!("koja({counter})>");
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

/// Accumulating REPL state: each accepted input is one statement-text
/// block. [`Session::try_eval`] concatenates the history into a single
/// script source, runs it through the pipeline, and rolls back on
/// failure.
///
/// Re-running the whole history every step is deliberate: the pipeline
/// is whole-program today (no incremental typecheck or IR delta), so
/// this is the simplest way to make accumulated state work. Fine for
/// the first few hundred lines.
struct Session {
    /// Baseline sources injected by the driver (stdlib plus, in a
    /// project, the project + dependency sources). Prepended to every
    /// pipeline run and preserved across `:reset` — it's the fixed
    /// scope the REPL evaluates against, not session input.
    baseline: Vec<SourceFile>,
    counter: u32,
    /// Package the synthesized session source belongs to. In a project
    /// this is the project's package name, so its modules resolve
    /// unqualified; otherwise [`SESSION_PACKAGE`].
    package: String,
    statements: Vec<String>,
}

impl Session {
    fn new(baseline: Vec<SourceFile>, package: String) -> Self {
        Self {
            baseline,
            counter: 1,
            package,
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
    /// The outcome's `rendered` carries the trailing expression's
    /// `Debug.format` output, the exact bytes `value.print()` would
    /// emit. It is `None` for a [`Value::Unit`] trailing value so the
    /// REPL suppresses the print line for void inputs (a `fn` item,
    /// an assignment, or a call like `IO.puts` whose signature elides
    /// `-> T`). The statement evaluated successfully in either case
    /// (side effects already landed), so we don't roll back.
    fn try_eval(&mut self, input: &str) -> Result<EvalOutcome, String> {
        let snapshot = self.statements.len();
        self.statements.push(input.to_string());
        match self.run() {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.statements.truncate(snapshot);
                Err(error)
            }
        }
    }

    /// Completion context before any input has evaluated: the
    /// baseline checked with an empty fragment. Degrades to an empty
    /// context (keywords and `:commands` only) if the baseline does
    /// not check. The session itself stays usable and reports the
    /// real errors on the first eval.
    fn initial_completion(&self) -> CompletionContext {
        let (sources, path) = self.sources();
        match check_fragment(sources, &path, false) {
            Ok(checked) => CompletionContext::of(&checked, self.package.clone(), &path),
            Err(_) => CompletionContext::empty(self.package.clone()),
        }
    }

    /// Synthesize the full session source and evaluate it.
    fn run(&self) -> Result<EvalOutcome, String> {
        let (sources, path) = self.sources();
        eval_fragment(sources, &path, &self.package)
    }

    /// The baseline plus the synthesized session fragment, and the
    /// fragment's path within that source set.
    fn sources(&self) -> (Vec<SourceFile>, PathBuf) {
        let path = PathBuf::from(format!("{}.koja", self.package));
        let mut sources = self.baseline.clone();
        sources.push(SourceFile {
            package: self.package.clone(),
            path: path.clone(),
            source: self.synthesize(),
        });
        (sources, path)
    }

    /// Concatenate all statement blocks into the script source the
    /// pipeline will parse. Blocks are joined with newlines so each
    /// input remains its own logical line group; `ParseMode::Script`
    /// handles the rest.
    fn synthesize(&self) -> String {
        self.statements.join("\n")
    }
}

/// A successful eval: the REPL's print line (`None` suppresses it
/// for a `Unit` trailing value) plus the completion snapshot taken
/// from the checked session.
struct EvalOutcome {
    completion: CompletionContext,
    rendered: Option<String>,
}

/// Drive the assembled `sources` through the script-mode pipeline and
/// produce an [`EvalOutcome`]: the REPL's print line (`Some(text)`
/// for a value, `None` for a suppressed `Unit` trailing value) plus
/// the completion snapshot taken from the probe pass's
/// [`CheckedProgram`].
///
/// The trailing expression is rendered through its real `Debug.format`
/// instance, the same path `value.print()` takes, so structs show
/// named fields and enums show source-level variant names instead of
/// the runtime [`Display`]'s mangled monomorphization symbols. To get
/// there, the trailing expression `E` is rewritten to `E.format()`
/// before lowering (see [`wrap_trailing_in_format`]): the lowered
/// script then yields the `Debug.format` string directly, and the
/// monomorphizer specializes the instance as a side effect of the
/// call. A post-hoc lookup can't, since the program never formats the
/// value on its own.
///
/// The rewrite only fires for a non-`Unit` trailing value. The probe
/// lower that decides this is side-effect-free, so exactly one
/// [`Interpreter::run_script`] executes per input and a fragment like
/// `GitHub.user("x")` fires its request once. If the wrapped lower
/// fails (a trailing type with no usable `Debug.format`, e.g. a bare
/// function value), the unwrapped body runs and the runtime
/// [`Display`] renders it.
///
/// `sources` is the driver-supplied baseline (stdlib prelude plus, in
/// a project, the project + dependency sources) with the REPL
/// fragment appended last. `fragment_path` identifies that fragment
/// file for the rewrite and the binding walk, and `package` labels
/// the completion snapshot.
fn eval_fragment(
    sources: Vec<SourceFile>,
    fragment_path: &Path,
    package: &str,
) -> Result<EvalOutcome, String> {
    let checked = check_fragment(sources.clone(), fragment_path, false)?;
    let completion = CompletionContext::of(&checked, package.to_string(), fragment_path);
    let probe = lower_checked(&checked)?;
    if probe.return_type == IRType::Unit {
        run_script(&probe)?;
        return Ok(EvalOutcome {
            completion,
            rendered: None,
        });
    }
    let formatted =
        check_fragment(sources, fragment_path, true).and_then(|wrapped| lower_checked(&wrapped));
    let value = match formatted {
        Ok(script) => run_script(&script)?,
        Err(_) => run_script(&probe)?,
    };
    Ok(EvalOutcome {
        completion,
        rendered: Some(render(value)),
    })
}

/// Parse + typecheck `sources` in script mode. [`ParseMode::Script`]
/// is unconditional: the REPL treats top-level statements as
/// first-class, and helper `fn` items land on
/// [`koja_ir::IRScript::packages`] as call targets. When `wrap` is
/// set, the fragment file's trailing expression is rewritten to
/// `<expr>.format()` so the lowered script yields its `Debug.format`
/// string. On failure returns a formatted error string covering
/// parse and typecheck failures.
fn check_fragment(
    sources: Vec<SourceFile>,
    fragment_path: &Path,
    wrap: bool,
) -> Result<CheckedProgram, String> {
    let mut parsed = parse_program(sources, ParseMode::Script);
    if wrap {
        wrap_trailing_in_format(&mut parsed, fragment_path);
    }
    check_program(parsed).map_err(format_check_failure)
}

fn lower_checked(checked: &CheckedProgram) -> Result<IRScript, String> {
    lower_script(checked).map_err(|err| err.to_string())
}

fn run_script(script: &IRScript) -> Result<Value, String> {
    Interpreter::run_script(script).map_err(|err| err.to_string())
}

/// The text the REPL prints for a trailing value. The wrapped lower
/// hands back the `Debug.format` string verbatim; the unwrapped
/// fallback renders the raw value through its runtime [`Display`].
fn render(value: Value) -> String {
    match value {
        Value::String(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        other => other.to_string(),
    }
}

/// Rewrite the fragment file's trailing `Statement::Expr(e)` to
/// `Statement::Expr(e.format())`, mirroring the `.format()` wrap the
/// `Debug` synthesizer splices into interpolations. No-op when the
/// fragment has no body or its trailing statement isn't a bare
/// expression (assignment, `fn` item, empty) — those lower to a `Unit`
/// trailing value the caller handles without rewriting.
fn wrap_trailing_in_format(parsed: &mut ParsedProgram, fragment_path: &Path) {
    let Some(file) = parsed.get_mut(fragment_path) else {
        return;
    };
    let Some(body) = file.ast.body.as_mut() else {
        return;
    };
    let Some(last) = body.pop() else {
        return;
    };
    let Statement::Expr(expr) = last else {
        body.push(last);
        return;
    };
    let span = expr.span;
    body.push(Statement::Expr(Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(expr),
            method: "format".to_string(),
            args: Vec::new(),
            type_args: Vec::new(),
        },
        span,
    )));
}

/// True when `source` (the accumulated multiline buffer) is a
/// well-formed-enough Koja fragment to hand to the parser: every
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
    for token in koja_lexer::lex(source).tokens {
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

/// Shared fixtures for this crate's tests, used by both the eval
/// tests below and the completion tests in [`complete`].
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    use koja_parser::SourceFile;

    /// Stdlib prelude plus a synthetic project package `Demo` so a
    /// REPL fragment can call into project sources just like the
    /// driver wires them up. `Calc` exercises a static method call,
    /// `Point` a struct with fields, and `Color` an enum with unit
    /// variants.
    pub(crate) fn baseline_with_project() -> Vec<SourceFile> {
        let mut baseline = koja_stdlib::autoimport_sources();
        baseline.extend(koja_stdlib::qualified_sources());
        baseline.push(SourceFile {
            package: "Demo".to_string(),
            path: PathBuf::from("demo.koja"),
            source: "struct Calc\n  fn double(x: Int) -> Int\n    x * 2\n  end\nend\n\
                     struct Point\n  x: Int\n  y: Int\nend\n\
                     enum Color\n  Red\n  Green\n  Blue\nend\n"
                .to_string(),
        });
        baseline
    }

    /// The REPL fragment's source set and path for `package`, the
    /// way [`crate::Session::sources`] assembles them.
    pub(crate) fn fragment_sources(
        baseline: &[SourceFile],
        package: &str,
        source: &str,
    ) -> (Vec<SourceFile>, PathBuf) {
        let path = PathBuf::from(format!("{package}.koja"));
        let mut sources = baseline.to_vec();
        sources.push(SourceFile {
            package: package.to_string(),
            path: path.clone(),
            source: source.to_string(),
        });
        (sources, path)
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{baseline_with_project, fragment_sources};
    use super::*;

    /// Append `source` as the REPL fragment for `package` and evaluate
    /// it the way [`Session::run`] does.
    fn eval(
        baseline: &[SourceFile],
        package: &str,
        source: &str,
    ) -> Result<Option<String>, String> {
        let (sources, path) = fragment_sources(baseline, package, source);
        eval_fragment(sources, &path, package).map(|outcome| outcome.rendered)
    }

    #[test]
    fn fragment_calls_into_project_baseline() {
        let baseline = baseline_with_project();
        match eval(&baseline, SESSION_PACKAGE, "Demo.Calc.double(21)") {
            Ok(rendered) => assert_eq!(rendered.as_deref(), Some("42")),
            Err(error) => panic!("expected project call to evaluate, got:\n{error}"),
        }
    }

    #[test]
    fn fragment_in_project_package_resolves_modules_unqualified() {
        let baseline = baseline_with_project();
        match eval(&baseline, "Demo", "Calc.double(21)") {
            Ok(rendered) => assert_eq!(rendered.as_deref(), Some("42")),
            Err(error) => panic!("expected unqualified project call to evaluate, got:\n{error}"),
        }
    }

    #[test]
    fn trailing_struct_renders_named_fields_via_debug_format() {
        let baseline = baseline_with_project();
        match eval(&baseline, "Demo", "Point{x: 1, y: 2}") {
            Ok(rendered) => assert_eq!(rendered.as_deref(), Some("Point{x: 1, y: 2}")),
            Err(error) => panic!("expected struct debug render, got:\n{error}"),
        }
    }

    #[test]
    fn trailing_string_renders_quoted_like_print() {
        let baseline = baseline_with_project();
        match eval(&baseline, SESSION_PACKAGE, "\"hi\"") {
            Ok(rendered) => assert_eq!(rendered.as_deref(), Some("\"hi\"")),
            Err(error) => panic!("expected quoted string render, got:\n{error}"),
        }
    }

    #[test]
    fn trailing_unit_suppresses_print_line() {
        let baseline = baseline_with_project();
        match eval(&baseline, SESSION_PACKAGE, "IO.puts(\"hi\")") {
            Ok(rendered) => assert_eq!(rendered, None),
            Err(error) => panic!("expected unit suppression, got:\n{error}"),
        }
    }
}
