//! Rendering helpers for attached comments.
//!
//! Ownership is decided up front by [`super::attach`]. These helpers turn
//! a slot's comments into `Doc` fragments, preserving single blank-line
//! gaps between comment runs via the line numbers on their spans.

use crate::doc::*;
use koja_ast::ast::Comment;

/// Renders own-line comments, each followed by a hardline, with a single
/// blank preserved between runs. Returns the docs and the source line of
/// the last comment, which callers compare against the following code
/// line to preserve a blank after the run.
pub(super) fn leading_docs(comments: &[Comment]) -> (Vec<Doc>, Option<u32>) {
    let mut docs = Vec::new();
    let mut last_line: Option<u32> = None;
    for comment in comments {
        if let Some(ll) = last_line
            && comment.span.start.line > ll + 1
        {
            docs.push(hardline());
        }
        docs.push(comment_doc(&comment.text));
        docs.push(hardline());
        last_line = Some(comment.span.start.line);
    }
    (docs, last_line)
}

/// Renders comments appended to the end of a code line, each prefixed
/// with a space.
pub(super) fn trailing_doc(comments: &[Comment]) -> Option<Doc> {
    if comments.is_empty() {
        return None;
    }
    Some(concat(
        comments
            .iter()
            .map(|c| concat(vec![text(" "), comment_doc(&c.text)]))
            .collect(),
    ))
}

/// Formats a single comment body as a `Doc`, normalizing whitespace.
pub(super) fn comment_doc(body: &str) -> Doc {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        text("#")
    } else {
        text(format!("# {}", trimmed))
    }
}
