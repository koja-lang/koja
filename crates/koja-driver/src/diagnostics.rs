//! Diagnostic rendering for compiler errors, warnings, and notes.
//!
//! Two formats ride one entry point. `Pretty` draws a box-drawing
//! source snippet per diagnostic for humans. `Short` prints one
//! `path:line:col: severity: message` line per diagnostic for pipes,
//! editors, and AI agents. Selection order: the `--diagnostics`
//! flag, then the `KOJA_DIAGNOSTICS` env var, then whether stderr
//! is a terminal.

use std::collections::BTreeMap;
use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use koja_ast::ast::{Diagnostic, Severity};

/// Source text keyed by file path, for snippet rendering.
pub type SourceMap = BTreeMap<PathBuf, String>;

/// Hints longer than this render as a trailing `help:` block instead
/// of a label attached to the underline.
const INLINE_HINT_LIMIT: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DiagnosticFormat {
    /// Box-drawing source snippets (default on terminals)
    Pretty,
    /// One line per diagnostic (default when stderr is piped)
    Short,
}

/// Process-wide rendering style, resolved once at startup.
#[derive(Clone, Copy)]
struct RenderStyle {
    color: bool,
    format: DiagnosticFormat,
}

static STYLE: OnceLock<RenderStyle> = OnceLock::new();

/// Resolve and store the process-wide diagnostic style. Called once
/// from `main` after CLI parsing. The short format never colors, so
/// captured output stays clean regardless of `NO_COLOR`.
pub fn init_style(format_flag: Option<DiagnosticFormat>, no_color: bool) {
    let format = format_flag
        .or_else(format_from_env)
        .unwrap_or_else(detect_format);
    let color = format == DiagnosticFormat::Pretty
        && !no_color
        && env::var_os("NO_COLOR").is_none()
        && std::io::stderr().is_terminal();
    let _ = STYLE.set(RenderStyle { color, format });
}

fn format_from_env() -> Option<DiagnosticFormat> {
    match env::var("KOJA_DIAGNOSTICS").ok()?.as_str() {
        "pretty" => Some(DiagnosticFormat::Pretty),
        "short" => Some(DiagnosticFormat::Short),
        _ => None,
    }
}

fn detect_format() -> DiagnosticFormat {
    if std::io::stderr().is_terminal() {
        DiagnosticFormat::Pretty
    } else {
        DiagnosticFormat::Short
    }
}

fn style() -> RenderStyle {
    *STYLE.get_or_init(|| RenderStyle {
        color: false,
        format: detect_format(),
    })
}

/// Render diagnostics in the process style. The returned block has
/// no trailing newline. Pretty rendering degrades gracefully: a
/// missing source keeps the header and location lines, a missing
/// path keeps just the header (and hint).
pub fn render_program_diagnostics(diagnostics: &[Diagnostic], sources: &SourceMap) -> String {
    render_with_style(diagnostics, sources, style())
}

/// Render diagnostics from one bare parse / lex result to stderr,
/// attributing them all to `path`.
pub fn print_file_diagnostics(path: &str, source: &str, diagnostics: &[Diagnostic]) {
    let mut owned = diagnostics.to_vec();
    Diagnostic::stamp_paths(&mut owned, Path::new(path));
    let sources = SourceMap::from([(PathBuf::from(path), source.to_string())]);
    eprintln!("{}", render_program_diagnostics(&owned, &sources));
}

fn render_with_style(
    diagnostics: &[Diagnostic],
    sources: &SourceMap,
    style: RenderStyle,
) -> String {
    let rendered: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| match style.format {
            DiagnosticFormat::Pretty => render_pretty(diagnostic, sources, style.color),
            DiagnosticFormat::Short => render_short(diagnostic),
        })
        .collect();
    let separator = match style.format {
        DiagnosticFormat::Pretty => "\n\n",
        DiagnosticFormat::Short => "\n",
    };
    rendered.join(separator)
}

/// `path:line:col: severity: message (hint: ...)`. Newlines in the
/// message and hint flatten to spaces so one diagnostic is always
/// exactly one line.
fn render_short(diagnostic: &Diagnostic) -> String {
    let location = match &diagnostic.path {
        Some(path) => format!(
            "{}:{}:{}",
            display_path(path),
            diagnostic.span.start.line,
            diagnostic.span.start.column,
        ),
        None => "<unknown>".to_string(),
    };
    let mut line = format!(
        "{location}: {}: {}",
        severity_name(diagnostic.severity),
        flatten(&diagnostic.message),
    );
    if let Some(hint) = &diagnostic.hint {
        line.push_str(&format!(" (hint: {})", flatten(hint)));
    }
    line
}

/// Box-drawing snippet layout:
///
/// ```text
/// error: unknown function `Point.orign`
///    ╭─ src/main.koja:14:9
///    │
/// 13 │   fn setup() -> Point
/// 14 │     p = Point.orign()
///    │         ───────┬────
///    │                ╰─ did you mean `origin`?
/// 15 │   end
/// ```
///
/// Long or multiline hints fall back to a `= help:` block after the
/// snippet instead of the inline label.
fn render_pretty(diagnostic: &Diagnostic, sources: &SourceMap, color: bool) -> String {
    let palette = Palette {
        color,
        severity: diagnostic.severity,
    };
    let mut out = format!(
        "{}: {}",
        palette.severity(severity_name(diagnostic.severity)),
        palette.bold(&diagnostic.message),
    );

    let inline_hint = diagnostic
        .hint
        .as_deref()
        .filter(|hint| !hint.contains('\n') && hint.chars().count() <= INLINE_HINT_LIMIT);

    if let Some(path) = &diagnostic.path {
        let source = sources.get(path).map(String::as_str);
        out.push('\n');
        out.push_str(&render_snippet(
            diagnostic,
            path,
            source,
            inline_hint,
            &palette,
        ));
    } else if let Some(hint) = &diagnostic.hint {
        out.push('\n');
        out.push_str(&render_help_block(hint, "", &palette));
    }
    out
}

/// The location line plus, when the source is available, the
/// context/source/underline rows. A hint too long for an inline
/// label lands as a trailing `= help:` block.
fn render_snippet(
    diagnostic: &Diagnostic,
    path: &Path,
    source: Option<&str>,
    inline_hint: Option<&str>,
    palette: &Palette,
) -> String {
    let span = diagnostic.span;
    let line_number = span.start.line as usize;
    let lines: Vec<&str> = source.map(|s| s.lines().collect()).unwrap_or_default();
    // Line numbers are 1-based and `lines` is 0-based.
    let source_line = line_number.checked_sub(1).and_then(|idx| lines.get(idx));
    let context_before = line_number.checked_sub(2).and_then(|idx| lines.get(idx));
    let context_after = lines.get(line_number);

    let last_shown = if context_after.is_some() {
        line_number + 1
    } else {
        line_number
    };
    let width = last_shown.to_string().len();
    let pad = " ".repeat(width);

    let mut rows = vec![format!(
        "{pad} {} {}:{}:{}",
        palette.dim("╭─"),
        display_path(path),
        span.start.line,
        span.start.column,
    )];
    if let Some(source_line) = source_line {
        let gutter = |label: String| format!("{label:>width$} {}", palette.dim("│"));
        rows.push(gutter(String::new()));
        if let Some(text) = context_before {
            rows.push(palette.dim(&format!("{:>width$} │ {text}", line_number - 1)));
        }
        rows.push(format!("{} {source_line}", gutter(line_number.to_string())));
        rows.extend(underline_rows(
            diagnostic,
            source_line,
            inline_hint,
            &gutter,
            palette,
        ));
        if let Some(text) = context_after {
            rows.push(palette.dim(&format!("{:>width$} │ {text}", line_number + 1)));
        }
    }
    if let Some(hint) = &diagnostic.hint
        && inline_hint.is_none()
    {
        rows.push(render_help_block(hint, &pad, palette));
    }
    rows.join("\n")
}

/// The `───┬───` row, plus the `╰─ hint` row when an inline hint is
/// present. Multi-line spans underline from the start column to the
/// end of the first line.
fn underline_rows(
    diagnostic: &Diagnostic,
    source_line: &str,
    inline_hint: Option<&str>,
    gutter: &impl Fn(String) -> String,
    palette: &Palette,
) -> Vec<String> {
    let span = diagnostic.span;
    let start = span.start.column.saturating_sub(1) as usize;
    let end = if span.start.line == span.end.line {
        (span.end.column as usize).max(start + 1)
    } else {
        source_line.chars().count().max(start + 1)
    };
    let length = end.saturating_sub(start).max(1);
    let indent = " ".repeat(start);

    let Some(hint) = inline_hint else {
        let underline = "─".repeat(length);
        return vec![format!(
            "{} {indent}{}",
            gutter(String::new()),
            palette.accent(&underline)
        )];
    };

    let connector_offset = length / 2;
    let mut underline = "─".repeat(connector_offset);
    underline.push('┬');
    underline.push_str(&"─".repeat(length - connector_offset - 1));
    let label_indent = " ".repeat(start + connector_offset);
    vec![
        format!(
            "{} {indent}{}",
            gutter(String::new()),
            palette.accent(&underline)
        ),
        format!(
            "{} {label_indent}{}",
            gutter(String::new()),
            palette.accent(&format!("╰─ {hint}")),
        ),
    ]
}

/// `= help:` block for hints too long for an inline label. `pad`
/// matches the snippet gutter so the `=` aligns with the `│` rows.
/// Continuation lines align under the first.
fn render_help_block(hint: &str, pad: &str, palette: &Palette) -> String {
    let mut lines = hint.lines();
    let first = lines.next().unwrap_or_default();
    let mut out = format!("{pad} {} {first}", palette.accent("= help:"));
    let continuation_indent = " ".repeat(pad.len() + 9);
    for line in lines {
        out.push_str(&format!("\n{continuation_indent}{line}"));
    }
    out
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Note => "note",
        Severity::Warning => "warning",
    }
}

/// Collapse all whitespace runs (including newlines) to single
/// spaces.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Relativize against the current directory for readability, falling
/// back to the path as recorded.
fn display_path(path: &Path) -> String {
    env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// ANSI styling for one diagnostic: the severity picks the accent
/// color, `color: false` passes text through untouched.
struct Palette {
    color: bool,
    severity: Severity,
}

impl Palette {
    fn severity(&self, text: &str) -> String {
        self.wrap(text, self.severity_code(true))
    }

    fn accent(&self, text: &str) -> String {
        self.wrap(text, self.severity_code(false))
    }

    fn bold(&self, text: &str) -> String {
        self.wrap(text, "\x1b[1m")
    }

    fn dim(&self, text: &str) -> String {
        self.wrap(text, "\x1b[2m")
    }

    fn wrap(&self, text: &str, code: &str) -> String {
        if self.color {
            format!("{code}{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn severity_code(&self, bold: bool) -> &'static str {
        match (self.severity, bold) {
            (Severity::Error, true) => "\x1b[1;31m",
            (Severity::Error, false) => "\x1b[31m",
            (Severity::Note, true) => "\x1b[1;36m",
            (Severity::Note, false) => "\x1b[36m",
            (Severity::Warning, true) => "\x1b[1;33m",
            (Severity::Warning, false) => "\x1b[33m",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use koja_ast::span::{Position, Span};

    const SOURCE: &str = "fn setup() -> Point\n  p = Point.orign()\nend\n";
    const FILE: &str = "src/main.koja";

    fn position(line: u32, column: u32) -> Position {
        Position {
            offset: 0,
            line,
            column,
        }
    }

    fn span(line: u32, start_column: u32, end_column: u32) -> Span {
        Span::new(position(line, start_column), position(line, end_column))
    }

    fn located(mut diagnostic: Diagnostic) -> Diagnostic {
        diagnostic.path = Some(PathBuf::from(FILE));
        diagnostic
    }

    fn sources() -> SourceMap {
        SourceMap::from([(PathBuf::from(FILE), SOURCE.to_string())])
    }

    fn render(diagnostics: &[Diagnostic], sources: &SourceMap, format: DiagnosticFormat) -> String {
        render_with_style(
            diagnostics,
            sources,
            RenderStyle {
                color: false,
                format,
            },
        )
    }

    #[test]
    fn pretty_renders_snippet_with_inline_hint() {
        let diagnostic = located(Diagnostic::error_with_hint(
            "unknown function `Point.orign`",
            "did you mean `origin`?",
            span(2, 7, 17),
        ));
        let expected = "\
error: unknown function `Point.orign`
  ╭─ src/main.koja:2:7
  │
1 │ fn setup() -> Point
2 │   p = Point.orign()
  │       ─────┬─────
  │            ╰─ did you mean `origin`?
3 │ end";
        assert_eq!(
            render(&[diagnostic], &sources(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn pretty_long_hint_falls_back_to_help_block() {
        let hint = "describe the replacement, for example a fully qualified path like `Global.New`";
        let diagnostic = located(Diagnostic::error_with_hint(
            "unknown function `Point.orign`",
            hint,
            span(2, 7, 17),
        ));
        let expected = format!(
            "\
error: unknown function `Point.orign`
  ╭─ src/main.koja:2:7
  │
1 │ fn setup() -> Point
2 │   p = Point.orign()
  │       ───────────
3 │ end
  = help: {hint}"
        );
        assert_eq!(
            render(&[diagnostic], &sources(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn pretty_multiline_span_underlines_to_end_of_first_line() {
        let diagnostic = located(Diagnostic::error(
            "mismatched end",
            Span::new(position(2, 7), position(3, 2)),
        ));
        let rendered = render(&[diagnostic], &sources(), DiagnosticFormat::Pretty);
        assert!(
            rendered.contains("  │       ─────────────"),
            "expected the underline to run to end of line, got:\n{rendered}",
        );
    }

    #[test]
    fn pretty_missing_source_keeps_header_and_location() {
        let diagnostic = located(Diagnostic::error("something failed", span(2, 7, 17)));
        let expected = "\
error: something failed
  ╭─ src/main.koja:2:7";
        assert_eq!(
            render(&[diagnostic], &SourceMap::new(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn pretty_pathless_diagnostic_keeps_header_and_hint() {
        let diagnostic = Diagnostic::error_with_hint(
            "public signature leaks private type",
            "mark the type public",
            span(2, 7, 17),
        );
        let expected = "\
error: public signature leaks private type
 = help: mark the type public";
        assert_eq!(
            render(&[diagnostic], &SourceMap::new(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn short_renders_one_line_with_flattened_message_and_hint() {
        let diagnostic = located(Diagnostic::error_with_hint(
            "unknown function\n`Point.orign`",
            "did you mean `origin`?",
            span(2, 7, 17),
        ));
        assert_eq!(
            render(&[diagnostic], &sources(), DiagnosticFormat::Short),
            "src/main.koja:2:7: error: unknown function `Point.orign` (hint: did you mean `origin`?)",
        );
    }

    #[test]
    fn short_pathless_diagnostic_uses_unknown_location() {
        let diagnostic = Diagnostic::warning("private type leaked", span(1, 1, 2));
        assert_eq!(
            render(&[diagnostic], &SourceMap::new(), DiagnosticFormat::Short),
            "<unknown>: warning: private type leaked",
        );
    }

    #[test]
    fn blocks_separate_by_format() {
        let diagnostics = vec![
            located(Diagnostic::warning("first", span(1, 1, 3))),
            located(Diagnostic::warning("second", span(2, 3, 4))),
        ];
        let short = render(&diagnostics, &SourceMap::new(), DiagnosticFormat::Short);
        assert_eq!(short.lines().count(), 2);
        let pretty = render(&diagnostics, &SourceMap::new(), DiagnosticFormat::Pretty);
        assert!(
            pretty.contains("\n\n"),
            "pretty blocks separate with a blank line"
        );
    }
}
