//! Eager comment attachment.
//!
//! Before printing, [`attach`] walks the AST in source-offset order with a
//! forward cursor over the comment list and assigns every comment to an
//! owner slot in a [`CommentTable`]. The printer then consumes slots by
//! `(Span, Slot)` key, in any order. Ownership is decided here, once. A
//! comment on a construct's last line trails that construct, and any
//! other comment belongs to the innermost enclosing block, leading the
//! next sibling inside it or dangling before the block's terminator.
//!
//! The `else` and `after` keywords have no spans in the AST, so the pass
//! locates them in a token stream lexed from the same source.
//!
//! This file holds the table, the cursor primitives, and the generic
//! sequence walkers. `items` walks declarations and `exprs` walks
//! statements, expressions, arms, and patterns.

mod exprs;
mod items;

use std::collections::HashMap;

use koja_ast::ast::*;
use koja_ast::span::Span;
use koja_ast::token::{Token, TokenKind};

use super::util::{lead_offset, stmt_span};

/// Where a comment sits relative to its owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Slot {
    /// Above the `after` keyword of a `receive`.
    BeforeAfter,
    /// Above the `else` keyword of an `if` or `cond`.
    BeforeElse,
    /// Between the last child and the block's `end`.
    Dangling,
    /// On the `else` keyword's line, after it.
    ElseTrailing,
    /// On a block header or arm-head line, after the code.
    HeaderTrailing,
    /// Above the node, on their own lines.
    Leading,
    /// Between the last element and a closing delimiter.
    Stragglers,
    /// On the node's last line, after the code.
    Trailing,
}

/// Comments grouped by owner, plus a sorted offset index for containment
/// queries.
#[derive(Default)]
pub(super) struct CommentTable {
    offsets: Vec<u32>,
    slots: HashMap<(Span, Slot), Vec<Comment>>,
}

impl CommentTable {
    /// Removes and returns the comments in a slot.
    pub(super) fn take(&mut self, key: Span, slot: Slot) -> Vec<Comment> {
        self.slots.remove(&(key, slot)).unwrap_or_default()
    }

    /// True when the slot holds at least one comment.
    pub(super) fn has(&self, key: Span, slot: Slot) -> bool {
        self.slots.contains_key(&(key, slot))
    }

    /// True when any comment starts strictly inside the span.
    pub(super) fn any_within(&self, span: Span) -> bool {
        let from = self.offsets.partition_point(|&o| o <= span.start.offset);
        self.offsets.get(from).is_some_and(|&o| o < span.end.offset)
    }

    /// Removes every unconsumed comment, sorted by source position. A
    /// non-empty result after printing is a printer bug. The caller emits
    /// them at the end of the file so nothing is lost, and debug builds
    /// assert.
    pub(super) fn drain_remaining(&mut self) -> Vec<Comment> {
        let mut rest: Vec<Comment> = self.slots.drain().flat_map(|(_, cs)| cs).collect();
        rest.sort_by_key(|c| c.span.start.offset);
        rest
    }
}

/// Builds the comment table for a parsed file.
pub(super) fn attach(file: &File, tokens: &[Token]) -> CommentTable {
    let mut attacher = Attacher {
        comments: &file.comments,
        pos: 0,
        table: CommentTable {
            offsets: file.comments.iter().map(|c| c.span.start.offset).collect(),
            slots: HashMap::new(),
        },
        tokens,
    };
    attacher.walk_file(file);
    attacher.table
}

struct Attacher<'a> {
    comments: &'a [Comment],
    pos: usize,
    table: CommentTable,
    tokens: &'a [Token],
}

/// A sequence child as the walk sees it, carrying the table key the
/// printer will look up, the offset its leading comments drain to, and
/// its span.
struct ChildInfo {
    key: Span,
    lead_offset: u32,
    span: Span,
}

impl ChildInfo {
    fn of(span: Span) -> Self {
        ChildInfo {
            key: span,
            lead_offset: span.start.offset,
            span,
        }
    }

    /// A child whose leading comments drain to its first annotation,
    /// which sits outside the declaration span.
    fn annotated(span: Span, annotations: &[Annotation]) -> Self {
        ChildInfo {
            key: span,
            lead_offset: lead_offset(annotations, span),
            span,
        }
    }
}

impl<'a> Attacher<'a> {
    fn peek(&self) -> Option<&'a Comment> {
        self.comments.get(self.pos)
    }

    /// True when an unconsumed comment starts before `offset`.
    fn pending_before(&self, offset: u32) -> bool {
        self.peek().is_some_and(|c| c.span.start.offset < offset)
    }

    fn push(&mut self, key: Span, slot: Slot, comments: Vec<Comment>) {
        if comments.is_empty() {
            return;
        }
        self.table
            .slots
            .entry((key, slot))
            .or_default()
            .extend(comments);
    }

    /// Consumes comments starting before `offset`.
    fn take_before(&mut self, offset: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c.span.start.offset >= offset {
                break;
            }
            out.push(c.clone());
            self.pos += 1;
        }
        out
    }

    /// Consumes comments before `offset` until a blank line starts a new
    /// comment run.
    fn take_before_without_blank(&mut self, offset: u32, previous_line: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        let mut last_line = previous_line;
        while let Some(comment) = self.peek() {
            if comment.span.start.offset >= offset || comment.span.start.line > last_line + 1 {
                break;
            }
            last_line = comment.span.end.line;
            out.push(comment.clone());
            self.pos += 1;
        }
        out
    }

    /// Consumes comments on exactly `line` that start before `bound`.
    fn take_on_line(&mut self, line: u32, bound: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c.span.start.line != line || c.span.start.offset >= bound {
                break;
            }
            out.push(c.clone());
            self.pos += 1;
        }
        out
    }

    /// The first token of `kind` in the offset range, if any.
    fn find_token(&self, kind: TokenKind, from: u32, to: u32) -> Option<&'a Token> {
        let start = self.tokens.partition_point(|t| t.span.start.offset < from);
        self.tokens[start..]
            .iter()
            .take_while(|t| t.span.start.offset < to)
            .find(|t| t.kind == kind)
    }

    /// Routes the comment after a header line's code to the owner's
    /// `HeaderTrailing`. `first_child` bounds the take.
    fn take_header_trailing(&mut self, owner: Span, header_line: u32, first_child: u32) {
        let trailing = self.take_on_line(header_line, first_child);
        self.push(owner, Slot::HeaderTrailing, trailing);
    }

    /// Walks one sequence child: leading comments, interior, a safety
    /// sweep of unclaimed interior comments into `Leading`, and a
    /// trailing take on the child's last line. `next_offset` is where the
    /// following content starts (the next child, or the region bound).
    fn walk_child(&mut self, child: &ChildInfo, next_offset: u32, walk: impl FnOnce(&mut Self)) {
        let leading = self.take_before(child.lead_offset);
        self.push(child.key, Slot::Leading, leading);
        walk(self);
        // Comments the interior walk left behind relocate above the child
        // rather than leak into a sibling. Or-pattern separators are the
        // known case.
        let strays = self.take_before(child.span.end.offset);
        self.push(child.key, Slot::Leading, strays);
        let trailing = self.take_on_line(content_end_line(child.span), next_offset);
        self.push(child.key, Slot::Trailing, trailing);
    }

    /// Walks a uniform child slice, then routes region-final comments to
    /// `dangling`.
    fn walk_children<T>(
        &mut self,
        items: &[T],
        info: impl Fn(&T) -> ChildInfo,
        mut walk: impl FnMut(&mut Self, &T),
        bound: u32,
        dangling: (Span, Slot),
    ) {
        for (i, item) in items.iter().enumerate() {
            let child = info(item);
            let next_offset = items.get(i + 1).map_or(bound, |n| info(n).lead_offset);
            self.walk_child(&child, next_offset, |s| walk(s, item));
        }
        let rest = self.take_before(bound);
        self.push(dangling.0, dangling.1, rest);
    }

    /// Walks a statement body whose region-final comments dangle before
    /// the owner's terminator.
    fn walk_body(&mut self, stmts: &[Statement], bound: u32, owner: Span) {
        self.walk_children(
            stmts,
            |s| ChildInfo::of(stmt_span(s)),
            |a, s| a.walk_stmt(s),
            bound,
            (owner, Slot::Dangling),
        );
    }

    /// A `header ... body ... end` block: the header line's trailing
    /// comment, then the body.
    fn walk_block(&mut self, owner: Span, header_line: u32, body: &[Statement]) {
        let first_stmt = first_stmt_offset(body, owner.end.offset);
        self.take_header_trailing(owner, header_line, first_stmt);
        self.walk_body(body, owner.end.offset, owner);
    }
}

/// Start offset of the first statement, or `fallback` for an empty body.
fn first_stmt_offset(body: &[Statement], fallback: u32) -> u32 {
    first_offset(body, stmt_span, fallback)
}

/// Start offset of the first item, or `fallback` for an empty slice.
fn first_offset<T>(items: &[T], span_of: impl Fn(&T) -> Span, fallback: u32) -> u32 {
    items
        .first()
        .map_or(fallback, |item| span_of(item).start.offset)
}

/// The last line holding the node's content. Some parser spans end at
/// column 1 of the following line (bodyless signatures, inline arms), and
/// a trailing take keyed on that line would steal the next line's
/// comments.
fn content_end_line(span: Span) -> u32 {
    if span.end.column == 1 && span.end.line > span.start.line {
        span.end.line - 1
    } else {
        span.end.line
    }
}
