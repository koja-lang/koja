//! Diagnostic rendering for compiler errors, warnings, and notes.
//!
//! Two formats ride one entry point. `Pretty` draws a box-drawing
//! source snippet per diagnostic for humans. `Short` prints one
//! `path:line:col: severity: message` line per diagnostic for pipes,
//! editors, and AI agents. Selection order: the `--diagnostics`
//! flag, then the `KOJA_DIAGNOSTICS` env var, then whether stderr
//! is a terminal.

use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use koja_ast::ast::{Diagnostic, Severity};
use koja_ast::span::{FileId, Span};

/// Source files for rendering, indexed by [`FileId`]. A diagnostic
/// whose file id misses the table renders without a location.
pub struct SourceTable {
    files: Vec<(PathBuf, String)>,
    /// When true, every span resolves to the one entry. Covers
    /// bare parses whose spans carry [`FileId::UNKNOWN`].
    single: bool,
}

impl SourceTable {
    pub fn new(files: Vec<(PathBuf, String)>) -> Self {
        Self {
            files,
            single: false,
        }
    }

    /// One-file table that attributes every span to that file.
    pub fn single(path: impl Into<PathBuf>, source: impl Into<String>) -> Self {
        Self {
            files: vec![(path.into(), source.into())],
            single: true,
        }
    }

    fn resolve(&self, file: FileId) -> Option<(&Path, &str)> {
        let index = if self.single { 0 } else { file.0 as usize };
        self.files
            .get(index)
            .map(|(path, source)| (path.as_path(), source.as_str()))
    }
}

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
static PATH_BASE: OnceLock<PathBuf> = OnceLock::new();

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

/// Set the base directory used to shorten diagnostic paths.
pub fn set_path_base(base: Option<&Path>) {
    if let Some(base) = base {
        let _ = PATH_BASE.set(base.to_path_buf());
    }
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

/// The resolved format, for output that follows the diagnostics
/// style without being a diagnostic. `koja test` hands it to the
/// test reporters.
pub fn current_format() -> DiagnosticFormat {
    style().format
}

/// Whether the resolved style colors its output. See
/// [`current_format`].
pub fn color_enabled() -> bool {
    style().color
}

/// Render diagnostics in the process style. The returned block has
/// no trailing newline. Pretty rendering degrades gracefully: an
/// empty source keeps the header and location lines, an unresolved
/// file id keeps just the header (and hint).
pub fn render_program_diagnostics(diagnostics: &[Diagnostic], sources: &SourceTable) -> String {
    render_with_style(diagnostics, sources, style())
}

/// Render diagnostics from one bare parse / lex result to stderr,
/// attributing them all to `path`.
pub fn print_file_diagnostics(path: &str, source: &str, diagnostics: &[Diagnostic]) {
    let sources = SourceTable::single(path, source);
    eprintln!("{}", render_program_diagnostics(diagnostics, &sources));
}

fn render_with_style(
    diagnostics: &[Diagnostic],
    sources: &SourceTable,
    style: RenderStyle,
) -> String {
    let rendered: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| match style.format {
            DiagnosticFormat::Pretty => render_pretty(diagnostic, sources, style.color),
            DiagnosticFormat::Short => render_short(diagnostic, sources),
        })
        .collect();
    let separator = match style.format {
        DiagnosticFormat::Pretty => "\n\n",
        DiagnosticFormat::Short => "\n",
    };
    rendered.join(separator)
}

/// `path:line:col: severity: message (hint: ...) (related at
/// path:line:col)`. Newlines in the message and hint flatten to
/// spaces so one diagnostic is always exactly one line.
fn render_short(diagnostic: &Diagnostic, sources: &SourceTable) -> String {
    let mut line = format!(
        "{}: {}: {}",
        short_location(diagnostic.span, sources),
        severity_name(diagnostic.severity),
        flatten(&diagnostic.message),
    );
    if let Some(hint) = &diagnostic.hint {
        line.push_str(&format!(" (hint: {})", flatten(hint)));
    }
    if let Some(related) = &diagnostic.related {
        line.push_str(&format!(
            " ({} at {})",
            flatten(&related.message),
            short_location(related.span, sources),
        ));
    }
    line
}

/// `path:line:col`, or `<unknown>` when the span's file is not in
/// the table.
fn short_location(span: Span, sources: &SourceTable) -> String {
    match sources.resolve(span.file) {
        Some((path, _)) => format!(
            "{}:{}:{}",
            display_path(path),
            span.start.line,
            span.start.column,
        ),
        None => "<unknown>".to_string(),
    }
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
/// snippet instead of the inline label. A related location renders
/// as a second snippet with its message as the label.
///
/// ```text
/// error: `App.greet` is already defined
///    ╭─ src/main.koja:5:4
///    │
///  4 │
///  5 │ fn greet
///    │    ─────
///  6 │   2
///    ╭─ src/main.koja:1:4
///    │
///  1 │ fn greet
///    │    ──┬──
///    │      ╰─ previous function definition
///  2 │   1
/// ```
fn render_pretty(diagnostic: &Diagnostic, sources: &SourceTable, color: bool) -> String {
    let palette = Palette {
        color,
        severity: diagnostic.severity,
    };
    let mut out = format!(
        "{}: {}",
        palette.severity(severity_name(diagnostic.severity)),
        palette.bold(&diagnostic.message),
    );

    let hint = diagnostic.hint.as_deref();
    let inline_hint = hint.filter(|hint| fits_inline(hint));
    let help = hint.filter(|_| inline_hint.is_none());

    if let Some((path, source)) = sources.resolve(diagnostic.span.file) {
        out.push('\n');
        out.push_str(&render_snippet(
            Snippet {
                span: diagnostic.span,
                label: inline_hint,
                help,
            },
            path,
            source,
            &palette,
        ));
    } else if let Some(hint) = hint {
        out.push('\n');
        out.push_str(&render_help_block("help", hint, "", &palette));
    }

    if let Some(related) = &diagnostic.related {
        let label = Some(related.message.as_str()).filter(|message| fits_inline(message));
        let note = label.is_none().then_some(related.message.as_str());
        out.push('\n');
        if let Some((path, source)) = sources.resolve(related.span.file) {
            out.push_str(&render_snippet(
                Snippet {
                    span: related.span,
                    label,
                    help: note,
                },
                path,
                source,
                &palette,
            ));
        } else {
            out.push_str(&render_help_block("note", &related.message, "", &palette));
        }
    }
    out
}

/// Whether `text` can sit on the `╰─` row under an underline.
fn fits_inline(text: &str) -> bool {
    !text.contains('\n') && text.chars().count() <= INLINE_HINT_LIMIT
}

/// One underlined source range with its optional inline label and
/// trailing `= help:` text.
struct Snippet<'a> {
    span: Span,
    label: Option<&'a str>,
    help: Option<&'a str>,
}

/// The location line plus, when the source is available, the
/// context/source/underline rows. `help` lands as a trailing
/// `= help:` block.
fn render_snippet(snippet: Snippet<'_>, path: &Path, source: &str, palette: &Palette) -> String {
    let Snippet { span, label, help } = snippet;
    let source = (!source.is_empty()).then_some(source);
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
        rows.extend(underline_rows(span, source_line, label, &gutter, palette));
        if let Some(text) = context_after {
            rows.push(palette.dim(&format!("{:>width$} │ {text}", line_number + 1)));
        }
    }
    if let Some(help) = help {
        rows.push(render_help_block("help", help, &pad, palette));
    }
    rows.join("\n")
}

/// The `───┬───` row, plus the `╰─ label` row when a label is
/// present. Multi-line spans underline from the start column to the
/// end of the first line. Columns are 1-based and `end` is exclusive,
/// so both convert to 0-based indexes by subtracting one.
fn underline_rows(
    span: Span,
    source_line: &str,
    label: Option<&str>,
    gutter: &impl Fn(String) -> String,
    palette: &Palette,
) -> Vec<String> {
    let start = span.start.column.saturating_sub(1) as usize;
    let end = if span.start.line == span.end.line {
        (span.end.column.saturating_sub(1) as usize).max(start + 1)
    } else {
        source_line.chars().count().max(start + 1)
    };
    let length = end.saturating_sub(start).max(1);
    let indent = " ".repeat(start);

    let Some(label) = label else {
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
            palette.accent(&format!("╰─ {label}")),
        ),
    ]
}

/// `= help:` (or `= note:`) block for text too long for an inline
/// label. `pad` matches the snippet gutter so the `=` aligns with the
/// `│` rows. Continuation lines align under the first.
fn render_help_block(kind: &str, text: &str, pad: &str, palette: &Palette) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let heading = format!("= {kind}:");
    let mut out = format!("{pad} {} {first}", palette.accent(&heading));
    let continuation_indent = " ".repeat(pad.len() + heading.len() + 2);
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

/// Relativize against the selected project or current directory for
/// readability, falling back to the path as recorded.
fn display_path(path: &Path) -> String {
    PATH_BASE
        .get()
        .cloned()
        .or_else(|| env::current_dir().ok())
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

    use koja_ast::span::{FileId, Position, Span};

    const SOURCE: &str = "fn setup() -> Point\n  p = Point.orign()\nend\n";
    const FILE: &str = "src/main.koja";

    fn position(line: u32, column: u32) -> Position {
        Position {
            offset: 0,
            line,
            column,
        }
    }

    /// `end_column` is exclusive, as the lexer produces it.
    fn span(line: u32, start_column: u32, end_column: u32) -> Span {
        Span::new(
            position(line, start_column),
            position(line, end_column),
            FileId(0),
        )
    }

    fn sources() -> SourceTable {
        SourceTable::new(vec![(PathBuf::from(FILE), SOURCE.to_string())])
    }

    fn no_sources() -> SourceTable {
        SourceTable::new(Vec::new())
    }

    fn render(
        diagnostics: &[Diagnostic],
        sources: &SourceTable,
        format: DiagnosticFormat,
    ) -> String {
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
        let diagnostic = Diagnostic::error_with_hint(
            "unknown function `Point.orign`",
            "did you mean `origin`?",
            span(2, 7, 18),
        );
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
        let diagnostic =
            Diagnostic::error_with_hint("unknown function `Point.orign`", hint, span(2, 7, 18));
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
        let diagnostic = Diagnostic::error(
            "mismatched end",
            Span::new(position(2, 7), position(3, 2), FileId(0)),
        );
        let rendered = render(&[diagnostic], &sources(), DiagnosticFormat::Pretty);
        assert!(
            rendered.contains("  │       ─────────────"),
            "expected the underline to run to end of line, got:\n{rendered}",
        );
    }

    #[test]
    fn pretty_missing_source_keeps_header_and_location() {
        let diagnostic = Diagnostic::error("something failed", span(2, 7, 18));
        let table = SourceTable::new(vec![(PathBuf::from(FILE), String::new())]);
        let expected = "\
error: something failed
  ╭─ src/main.koja:2:7";
        assert_eq!(
            render(&[diagnostic], &table, DiagnosticFormat::Pretty),
            expected
        );
    }

    #[test]
    fn pretty_unresolved_file_keeps_header_and_hint() {
        let diagnostic = Diagnostic::error_with_hint(
            "public signature leaks private type",
            "mark the type public",
            span(2, 7, 18),
        );
        let expected = "\
error: public signature leaks private type
 = help: mark the type public";
        assert_eq!(
            render(&[diagnostic], &no_sources(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn pretty_related_location_renders_a_second_snippet() {
        let diagnostic = Diagnostic::error("`App.setup` is already defined", span(2, 7, 12))
            .with_related("previous function definition", span(1, 4, 9));
        let expected = "\
error: `App.setup` is already defined
  ╭─ src/main.koja:2:7
  │
1 │ fn setup() -> Point
2 │   p = Point.orign()
  │       ─────
3 │ end
  ╭─ src/main.koja:1:4
  │
1 │ fn setup() -> Point
  │    ──┬──
  │      ╰─ previous function definition
2 │   p = Point.orign()";
        assert_eq!(
            render(&[diagnostic], &sources(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn pretty_related_without_source_falls_back_to_note() {
        let diagnostic =
            Diagnostic::error("boom", span(2, 7, 12)).with_related("declared here", span(1, 4, 9));
        let expected = "\
error: boom
 = note: declared here";
        assert_eq!(
            render(&[diagnostic], &no_sources(), DiagnosticFormat::Pretty),
            expected,
        );
    }

    #[test]
    fn short_appends_related_location() {
        let diagnostic = Diagnostic::error("`App.setup` is already defined", span(2, 7, 12))
            .with_related("previous function definition", span(1, 4, 9));
        assert_eq!(
            render(&[diagnostic], &sources(), DiagnosticFormat::Short),
            "src/main.koja:2:7: error: `App.setup` is already defined \
             (previous function definition at src/main.koja:1:4)",
        );
    }

    #[test]
    fn short_renders_one_line_with_flattened_message_and_hint() {
        let diagnostic = Diagnostic::error_with_hint(
            "unknown function\n`Point.orign`",
            "did you mean `origin`?",
            span(2, 7, 18),
        );
        assert_eq!(
            render(&[diagnostic], &sources(), DiagnosticFormat::Short),
            "src/main.koja:2:7: error: unknown function `Point.orign` (hint: did you mean `origin`?)",
        );
    }

    #[test]
    fn short_unresolved_file_uses_unknown_location() {
        let diagnostic = Diagnostic::warning("private type leaked", span(1, 1, 2));
        assert_eq!(
            render(&[diagnostic], &no_sources(), DiagnosticFormat::Short),
            "<unknown>: warning: private type leaked",
        );
    }

    #[test]
    fn single_table_attributes_unknown_spans() {
        let diagnostic = Diagnostic::error(
            "boom",
            Span::new(position(2, 7), position(2, 9), FileId::UNKNOWN),
        );
        let table = SourceTable::single(FILE, SOURCE);
        assert_eq!(
            render(&[diagnostic], &table, DiagnosticFormat::Short),
            "src/main.koja:2:7: error: boom",
        );
    }

    #[test]
    fn blocks_separate_by_format() {
        let diagnostics = vec![
            Diagnostic::warning("first", span(1, 1, 3)),
            Diagnostic::warning("second", span(2, 3, 4)),
        ];
        let short = render(&diagnostics, &no_sources(), DiagnosticFormat::Short);
        assert_eq!(short.lines().count(), 2);
        let pretty = render(&diagnostics, &no_sources(), DiagnosticFormat::Pretty);
        assert!(
            pretty.contains("\n\n"),
            "pretty blocks separate with a blank line"
        );
    }
}
