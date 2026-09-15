//! Pretty-printer: converts a parsed Koja AST back into formatted source code.
//!
//! The entry point is [`file_to_doc`], which produces a [`Doc`] document tree
//! that the renderer in [`crate::doc`] lays out to a target line width.
//!
//! Comment ownership is decided before printing by the attachment pass in
//! [`attach`], which assigns every comment to a `(Span, Slot)` owner, and
//! the printer consumes slots as it renders. A slot nobody consumes shows up in
//! the end-of-print sweep, which appends the comments at the file's end so
//! nothing is lost and fails debug builds.

mod attach;
mod comments;
mod decl;
mod expr;
mod pattern;
mod seq;
mod stmt;
mod util;

use std::mem;

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;
use koja_ast::token::Token;

use attach::{CommentTable, Slot};
use comments::leading_docs;
use seq::{SeqEntry, Spacing, vertical};
use util::*;

/// Converts a parsed file into a `Doc` tree ready for rendering. `tokens`
/// is the file's token stream, used to locate boundary keywords (`else`,
/// `after`) that carry no span in the AST.
pub fn file_to_doc(file: &File, tokens: &[Token]) -> Doc {
    let mut p = Printer {
        comments: attach::attach(file, tokens),
    };
    let doc = p.print_file(file);
    let rest = p.comments.drain_remaining();
    if rest.is_empty() {
        return doc;
    }
    debug_assert!(
        false,
        "koja-fmt: comments left unconsumed after printing: {rest:?}"
    );
    let (docs, _) = leading_docs(&rest);
    concat(std::iter::once(doc).chain(docs).collect())
}

/// Converts a function header into a `Doc`, with `display_name` in
/// place of the declared name. No comments are consulted, so the
/// caller may hand in a synthesized node with placeholder spans.
pub fn signature_to_doc(function: &Function, display_name: &str) -> Doc {
    Printer::pure().signature_to_doc(
        format!("{}fn {display_name}", visibility_prefix(function.visibility)),
        &function.type_params,
        &function.params,
        function.span,
        function.return_type.as_ref(),
        function.error_type.as_ref(),
    )
}

/// Holds the comment table the printer consumes while rendering. The
/// `*_to_doc` methods live in the sibling modules by topic: `decl`,
/// `stmt`, `expr`, `pattern`.
struct Printer {
    comments: CommentTable,
}

impl Printer {
    /// A printer over an empty comment table, for rendering a node that
    /// is known to hold no comments through the same code path as a
    /// commented one.
    pub(super) fn pure() -> Self {
        Printer {
            comments: CommentTable::default(),
        }
    }

    /// Formats an entire file, with top-level declarations and statements
    /// merged in source order. `.kojs` scripts carry statements in
    /// `file.body`, and `.koja` modules leave it `None`.
    fn print_file(&mut self, file: &File) -> Doc {
        let nodes = top_level_nodes(file);
        let module = file.body.is_none();
        let mut entries = Vec::with_capacity(nodes.len());
        for (i, node) in nodes.iter().enumerate() {
            let key = node.key();
            // In modules, a run of `const`s and a run of `alias`es read
            // as separate groups, so force a blank at the transition.
            let force_blank = module
                && i > 0
                && matches!(node, TopLevel::Item(Item::Constant(_) | Item::Alias(_)))
                && matches!(
                    &nodes[i - 1],
                    TopLevel::Item(Item::Constant(_) | Item::Alias(_))
                )
                && match (&nodes[i - 1], node) {
                    (TopLevel::Item(prev), TopLevel::Item(cur)) => {
                        mem::discriminant(*prev) != mem::discriminant(*cur)
                    }
                    _ => false,
                };
            let doc = match node {
                TopLevel::Item(item) => self.item_to_doc(item),
                TopLevel::Stmt(stmt) => self.statement_to_doc(stmt),
            };
            let mut entry = self.entry(key, node.start_line(), node.is_block(), doc);
            entry.force_blank = force_blank;
            entries.push(entry);
        }

        let dangling = self.comments.take(file.span, Slot::Dangling);
        if entries.is_empty() && dangling.is_empty() {
            return nil();
        }
        concat(vec![
            vertical(entries, Spacing::Preserve, dangling),
            hardline(),
        ])
    }

    /// Pairs a rendered child with the comments attached to `key`.
    /// `block` marks members and block statements, which want blank
    /// lines around them.
    fn entry(&mut self, key: Span, start_line: u32, block: bool, doc: Doc) -> SeqEntry {
        SeqEntry {
            doc,
            end_line: key.end.line,
            force_blank: block,
            is_block: block,
            leading: self.comments.take(key, Slot::Leading),
            start_line,
            trailing: self.comments.take(key, Slot::Trailing),
        }
    }

    /// Builds sequence entries for a uniform child slice, pairing each
    /// child's doc with the comments attached to its span.
    fn seq_entries<T>(
        &mut self,
        items: &[T],
        key_of: impl Fn(&T) -> Span,
        mut to_doc: impl FnMut(&mut Self, &T) -> Doc,
    ) -> Vec<SeqEntry> {
        items
            .iter()
            .map(|item| {
                let key = key_of(item);
                let doc = to_doc(self, item);
                self.entry(key, key.start.line, false, doc)
            })
            .collect()
    }

    /// True when no comment sits inside the construct, neither anchored
    /// to an element nor pending before the closing delimiter.
    fn entries_comment_free(&self, entries: &[SeqEntry], owner: Span) -> bool {
        entries.iter().all(SeqEntry::comment_free) && !self.comments.has(owner, Slot::Stragglers)
    }
}
