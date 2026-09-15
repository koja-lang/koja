pub mod doc;
pub mod printer;

use doc::{DEFAULT_WIDTH, render};
use koja_ast::ast::{Diagnostic, Function};
use koja_parser::ParseMode;

/// The result of formatting a source string.
pub enum FormatResult {
    /// Successfully formatted source code.
    Ok(String),
    /// The source could not be parsed. Carries the parse diagnostics.
    ParseErrors(Vec<Diagnostic>),
}

/// Formats Koja source code using the default line width (80 columns).
///
/// `mode` selects the top-level grammar: [`ParseMode::Script`] for
/// `.kojs` scripts (top-level statements), [`ParseMode::File`] for
/// `.koja` modules. Callers typically derive it via
/// [`ParseMode::for_path`].
pub fn format(source: &str, mode: ParseMode) -> FormatResult {
    format_width(source, DEFAULT_WIDTH, mode)
}

/// Formats Koja source code, wrapping lines at `width` columns.
pub fn format_width(source: &str, width: u32, mode: ParseMode) -> FormatResult {
    let result = koja_parser::parse(source, mode);
    if !result.errors.is_empty() {
        return FormatResult::ParseErrors(result.errors);
    }

    // The comment attachment pass locates boundary keywords (`else`,
    // `after`) in the token stream because the AST carries no spans for
    // them.
    let lexed = koja_lexer::lex(source, koja_ast::span::FileId::UNKNOWN);
    let doc = printer::file_to_doc(&result.ast, &lexed.tokens);
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

/// Formats a function header the way [`format`] would print it at
/// column 0, wrapping at `width`. `display_name` replaces the
/// function's own name, so a hover can show `Type.method`. The body,
/// annotations, and comments are left out. Editors call this to show a
/// signature that matches the formatter.
pub fn format_signature(function: &Function, display_name: &str, width: u32) -> String {
    let doc = printer::signature_to_doc(function, display_name);
    render(&doc, width)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}
