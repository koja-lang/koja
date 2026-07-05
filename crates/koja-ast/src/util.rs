//! Small string utilities shared across the Koja toolchain.
//!
//! Anything that lives here must be:
//! - purely functional (no IO, no allocation beyond what's returned),
//! - generic over the compiler pipeline (not tied to AST, IR, or any
//!   particular pass),
//! - used from at least two crates (otherwise it belongs next to its
//!   single caller).
//!
//! The bar is deliberately high — `koja_ast` is a leaf crate in the
//! dependency graph, so every export here gets pulled into every
//! downstream crate.

/// Strip the common leading indentation from every line in `s`, after
/// dropping a single leading newline if present.
///
/// Designed for readable multi-line string literals in tests and code
/// generators: write the source indented to match surrounding code,
/// then call `dedent` to normalize it back to column zero.
///
/// Contract:
/// - A leading `\n` (produced by starting the literal on the line
///   *after* the opening quote) is stripped once.
/// - The common leading whitespace is computed across all
///   non-blank lines and removed from every line. Lines shorter than
///   that prefix are trimmed instead of sliced, so blank/whitespace-
///   only lines collapse to the empty string rather than panicking.
/// - Lines are rejoined with `\n`. Per [`str::lines`] semantics one
///   trailing newline is dropped, so `"a\n"` dedents to `"a"`; a
///   closing-quote indent line (literal ending in `\n    `) surfaces
///   as a trailing `"\n"` in the output, which is typically what the
///   caller wants for source fixtures.
///
/// Not intended for dedenting the *contents* of string literals in
/// Koja source (that's [`parser::dedent_multiline_parts`]); this is
/// a tool-side utility for spelling fixtures.
///
/// # Examples
///
/// The trailing indent before the closing quote keeps a final `\n`,
/// which is usually what you want for a source fixture:
///
/// ```
/// use koja_ast::util::dedent;
///
/// let source = dedent("
///     fn main
///       1 + 2
///     end
///     ");
/// assert_eq!(source, "fn main\n  1 + 2\nend\n");
/// ```
pub fn dedent(s: &str) -> String {
    let s = s.strip_prefix('\n').unwrap_or(s);
    let min_indent = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    s.lines()
        .map(|l| {
            if l.len() >= min_indent {
                &l[min_indent..]
            } else {
                l.trim()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_newline_and_common_indent() {
        let got = dedent(
            "
            fn main
              1 + 2
            end
            ",
        );
        assert_eq!(got, "fn main\n  1 + 2\nend\n");
    }

    #[test]
    fn preserves_relative_indentation() {
        let got = dedent(
            "
            outer
              inner
                deeper
            ",
        );
        assert_eq!(got, "outer\n  inner\n    deeper\n");
    }

    #[test]
    fn blank_lines_collapse_and_do_not_skew_min_indent() {
        let got = dedent(
            "
            first

              indented
            ",
        );
        assert_eq!(got, "first\n\n  indented\n");
    }

    #[test]
    fn handles_zero_common_indent_and_drops_single_trailing_newline() {
        // `str::lines` drops a single trailing terminator, so a
        // literal ending in `\n` yields a string without one. That
        // matches the behavior of every legacy `dedent` copy we
        // replaced — callers have been relying on it.
        let got = dedent("fn main\n  1\nend\n");
        assert_eq!(got, "fn main\n  1\nend");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(dedent(""), "");
    }

    #[test]
    fn whitespace_only_input_has_zero_min_indent() {
        // No non-blank lines -> min_indent defaults to 0; whitespace
        // is preserved on each line and one trailing `\n` is dropped.
        assert_eq!(dedent("\n    \n   \n"), "    \n   ");
    }
}
