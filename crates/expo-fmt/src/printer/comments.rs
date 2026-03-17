//! Comment cursor for source-faithful comment re-attachment.
//!
//! The formatter must preserve all source comments in their original positions
//! relative to surrounding code. [`CommentCursor`] walks the comment list in
//! source order, yielding comments that belong before, on, or after a given
//! source line.

use crate::doc::*;
use expo_ast::ast::*;

/// A forward-only cursor over a module's source comments.
///
/// Comments are sorted by source position. The cursor tracks how far we've
/// consumed, so each call to `drain_before` / `drain_trailing` advances past
/// already-emitted comments without rescanning from the start.
pub(crate) struct CommentCursor<'a> {
    comments: &'a [Comment],
    pos: usize,
}

impl<'a> CommentCursor<'a> {
    pub(super) fn new(comments: &'a [Comment]) -> Self {
        Self { comments, pos: 0 }
    }

    /// Consumes all comments whose start line is strictly before `line`.
    ///
    /// Returns `(docs, last_line)` where `docs` are the formatted comment
    /// documents (each followed by a hardline) and `last_line` is the source
    /// line of the last comment drained. Callers use `last_line` to detect
    /// blank-line gaps between the comment block and the next statement.
    pub(super) fn drain_before(&mut self, line: u32) -> (Vec<Doc>, Option<u32>) {
        let mut docs = Vec::new();
        let mut last_line: Option<u32> = None;
        while self.pos < self.comments.len() && self.comments[self.pos].span.start.line < line {
            let c = &self.comments[self.pos];
            if let Some(ll) = last_line
                && c.span.start.line > ll + 1
            {
                docs.push(hardline());
            }
            docs.push(comment_doc(&c.text));
            docs.push(hardline());
            last_line = Some(c.span.start.line);
            self.pos += 1;
        }
        (docs, last_line)
    }

    /// Returns the line of the next unconsumed comment if it falls before
    /// `line`, without advancing the cursor.
    pub(super) fn peek_before(&self, line: u32) -> Option<u32> {
        if self.pos < self.comments.len() && self.comments[self.pos].span.start.line < line {
            Some(self.comments[self.pos].span.start.line)
        } else {
            None
        }
    }

    /// Consumes comments sitting on exactly `line` (trailing comments that
    /// appear after code on the same line).
    pub(super) fn drain_trailing(&mut self, line: u32) -> Option<Doc> {
        let mut docs = Vec::new();
        while self.pos < self.comments.len() && self.comments[self.pos].span.start.line == line {
            let c = &self.comments[self.pos];
            docs.push(comment_doc(&c.text));
            self.pos += 1;
        }
        if docs.is_empty() {
            None
        } else {
            Some(concat(
                docs.into_iter()
                    .map(|d| concat(vec![text(" "), d]))
                    .collect(),
            ))
        }
    }

    /// Drains all remaining unconsumed comments (used at the end of a module).
    pub(super) fn drain_rest(&mut self) -> Vec<Doc> {
        let mut docs = Vec::new();
        while self.pos < self.comments.len() {
            let c = &self.comments[self.pos];
            docs.push(comment_doc(&c.text));
            docs.push(hardline());
            self.pos += 1;
        }
        docs
    }
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
