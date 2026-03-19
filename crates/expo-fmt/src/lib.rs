pub mod doc;
pub mod printer;

use doc::render;
use expo_ast::ast::Diagnostic;

/// The result of formatting a source string.
pub enum FormatResult {
    /// Successfully formatted source code.
    Ok(String),
    /// The source could not be parsed; contains the parse diagnostics.
    ParseErrors(Vec<Diagnostic>),
}

/// Formats Expo source code using the default line width (80 columns).
pub fn format(source: &str) -> FormatResult {
    format_width(source, 80)
}

/// Formats Expo source code, wrapping lines at `width` columns.
pub fn format_width(source: &str, width: u32) -> FormatResult {
    let result = expo_parser::parse(source);
    if !result.errors.is_empty() {
        return FormatResult::ParseErrors(result.errors);
    }

    let doc = printer::module_to_doc(&result.module);
    let rendered = render(&doc, width);
    let mut out: String = rendered
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");

    if !out.ends_with('\n') {
        out.push('\n');
    }

    FormatResult::Ok(out)
}
