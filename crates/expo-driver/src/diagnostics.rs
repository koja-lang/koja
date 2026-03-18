//! Diagnostic rendering for compiler errors, warnings, and notes.
//!
//! Formats diagnostics with source context, caret-style underlines, and
//! optional hints, writing to stderr in a style similar to `rustc`.

use expo_ast::ast::{Diagnostic, Severity};

/// Renders a list of diagnostics to stderr with source context.
///
/// Each diagnostic is printed with its severity, message, source location,
/// the offending source line, caret underlines, and an optional hint.
/// When `color` is true, severity labels are ANSI-colored.
pub fn render_diagnostics(filename: &str, source: &str, diagnostics: &[Diagnostic], color: bool) {
    let lines: Vec<&str> = source.lines().collect();
    let max_line = diagnostics
        .iter()
        .map(|d| d.span.start.line as usize)
        .max()
        .unwrap_or(1);
    let gutter_width = max_line.to_string().len();

    for d in diagnostics {
        let severity_label = match (&d.severity, color) {
            (Severity::Error, true) => "\x1b[1;31merror\x1b[0m",
            (Severity::Error, false) => "error",
            (Severity::Warning, true) => "\x1b[1;33mwarning\x1b[0m",
            (Severity::Warning, false) => "warning",
            (Severity::Note, _) => "note",
        };

        eprintln!("{severity_label}: {}", d.message);
        eprintln!(
            "{:>gutter_width$}--> {filename}:{}:{}",
            " ", d.span.start.line, d.span.start.column
        );

        let line_idx = d.span.start.line.saturating_sub(1) as usize;
        if let Some(source_line) = lines.get(line_idx) {
            eprintln!("{:>gutter_width$} |", "");
            eprintln!("{:>gutter_width$} | {source_line}", d.span.start.line);

            let col_start = d.span.start.column.saturating_sub(1) as usize;
            let col_end = if d.span.start.line == d.span.end.line {
                (d.span.end.column as usize).max(col_start + 1)
            } else {
                source_line.len().max(col_start + 1)
            };
            let caret_count = col_end.saturating_sub(col_start).max(1);
            let padding = " ".repeat(col_start);
            let carets = "^".repeat(caret_count);
            eprintln!("{:>gutter_width$} | {padding}{carets}", "");
        }

        if let Some(hint) = &d.hint {
            eprintln!("{:>gutter_width$} |", "");
            eprintln!("{:>gutter_width$} = hint: {hint}", "");
        }

        eprintln!();
    }
}
