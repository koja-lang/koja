//! The commented-sequence builder.
//!
//! Every block interior in Koja is a sequence of children: module items,
//! script nodes, type-body members, statements, arms, and the elements of
//! bracketed constructs. One entry type carries each child's doc and its
//! attached comments, and a small set of layout backends renders the
//! sequence. A comment rule fixed here is fixed at every nesting level.

use std::mem;

use crate::doc::*;
use koja_ast::ast::Comment;

use super::comments::{leading_docs, trailing_doc};

/// One sequence child with its attached comments and layout facts.
pub(super) struct SeqEntry {
    pub(super) doc: Doc,
    /// Last source line, for blank-gap measurement against the next child.
    pub(super) end_line: u32,
    /// Forces a blank line before this child regardless of the source.
    pub(super) force_blank: bool,
    /// Blank lines read better around this child (declarations, `if`,
    /// `match`, ...).
    pub(super) is_block: bool,
    pub(super) leading: Vec<Comment>,
    /// First source line as authored, annotations included.
    pub(super) start_line: u32,
    pub(super) trailing: Vec<Comment>,
}

impl SeqEntry {
    pub(super) fn comment_free(&self) -> bool {
        self.leading.is_empty() && self.trailing.is_empty()
    }
}

/// How a vertical sequence spaces its children.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Spacing {
    /// Blank lines from the source are preserved (a wider gap collapses
    /// to one) and forced around block children.
    Preserve,
    /// No blank lines except where a child forces one.
    Tight,
}

/// Renders children on their own lines, with leading comments above,
/// trailing comments appended, blank lines per [`Spacing`], and the
/// region's dangling comments at the end. The result carries no outer
/// hardlines, so the caller places the sequence.
pub(super) fn vertical(entries: Vec<SeqEntry>, spacing: Spacing, dangling: Vec<Comment>) -> Doc {
    let mut parts = Vec::new();
    let mut prev_end: Option<u32> = None;
    let mut prev_forces = false;

    for entry in entries {
        let first_line = entry
            .leading
            .first()
            .map_or(entry.start_line, |c| c.span.start.line);
        if let Some(prev) = prev_end {
            parts.push(hardline());
            let source_gap = first_line > prev + 1;
            let blank = match spacing {
                Spacing::Preserve => {
                    source_gap || entry.is_block || entry.force_blank || prev_forces
                }
                Spacing::Tight => entry.force_blank || prev_forces,
            };
            if blank {
                parts.push(hardline());
            }
        }
        let (comment_parts, last_comment_line) = leading_docs(&entry.leading);
        parts.extend(comment_parts);
        if let Some(lcl) = last_comment_line
            && entry.start_line > lcl + 1
        {
            parts.push(hardline());
        }
        parts.push(entry.doc);
        if let Some(tc) = trailing_doc(&entry.trailing) {
            parts.push(tc);
        }
        // `force_blank` demands a blank before its own entry only;
        // `is_block` wants space on both sides.
        prev_forces = match spacing {
            Spacing::Preserve => entry.is_block,
            Spacing::Tight => entry.force_blank,
        };
        prev_end = Some(entry.end_line);
    }

    if let Some(first) = dangling.first() {
        if prev_end.is_some() {
            parts.push(hardline());
        }
        if prev_end.is_some_and(|prev| first.span.start.line > prev + 1) {
            parts.push(hardline());
        }
        let (mut comment_parts, _) = leading_docs(&dangling);
        comment_parts.pop();
        parts.extend(comment_parts);
    }

    concat(parts)
}

/// Appends region-final comments after the last element, without the
/// final hardline (the enclosing layout breaks before its delimiter).
fn push_stragglers(body: &mut Vec<Doc>, stragglers: &[Comment]) {
    if stragglers.is_empty() {
        return;
    }
    let (mut docs, _) = leading_docs(stragglers);
    docs.pop();
    body.push(hardline());
    body.extend(docs);
}

/// Broken field-list interior with one field per line, a comma after
/// every field, and comments anchored to their field. Starts with the
/// break after the opening brace. The caller closes with `hardline`
/// and the delimiter.
pub(super) fn field_lines(entries: Vec<SeqEntry>, stragglers: Vec<Comment>) -> Doc {
    let mut body = Vec::new();
    for entry in entries {
        body.push(hardline());
        let (comment_parts, _) = leading_docs(&entry.leading);
        body.extend(comment_parts);
        body.push(entry.doc);
        body.push(text(","));
        if let Some(tc) = trailing_doc(&entry.trailing) {
            body.push(tc);
        }
    }
    push_stragglers(&mut body, &stragglers);
    concat(body)
}

/// Broken element-list interior with comment-aware packing. Comment-free
/// runs fill-pack, a trailing comment ends its packed line, and a leading
/// comment takes its own line before the next run. Comma after every
/// element except the last.
pub(super) fn element_lines(entries: Vec<SeqEntry>, stragglers: Vec<Comment>) -> Doc {
    let last = entries.len() - 1;
    let mut body = vec![hardline()];
    let mut run: Vec<Doc> = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        let elem = if i < last {
            concat(vec![entry.doc, text(",")])
        } else {
            entry.doc
        };
        if !entry.leading.is_empty() {
            if !run.is_empty() {
                body.push(fill(mem::take(&mut run)));
                body.push(hardline());
            }
            // Each leading comment already ends with a hardline, so the
            // next run lands on the following line.
            let (comment_parts, _) = leading_docs(&entry.leading);
            body.extend(comment_parts);
        }
        match trailing_doc(&entry.trailing) {
            Some(tc) => {
                run.push(concat(vec![elem, tc]));
                body.push(fill(mem::take(&mut run)));
                body.push(hardline());
            }
            None => run.push(elem),
        }
    }
    if run.is_empty() {
        body.pop();
    } else {
        body.push(fill(run));
    }
    push_stragglers(&mut body, &stragglers);
    concat(body)
}
