//! Comment-aware pattern formatting.
//!
//! A comment-free pattern renders through the pure
//! [`util::pattern_to_doc`] fast path. A comment inside a container
//! switches that container to the broken layout with the comment
//! anchored to its element, mirroring the collection printers on the
//! expression side.

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::labels::pattern_span;
use koja_ast::span::Span;

use super::Printer;
use super::util;

impl Printer {
    /// Formats a pattern. A container holding a comment takes the broken
    /// layout. Everything else falls back to the pure printer.
    pub(super) fn pattern_to_doc(&mut self, pattern: &Pattern) -> Doc {
        if !self.comments.any_within(pattern_span(pattern)) {
            return util::pattern_to_doc(pattern);
        }
        match pattern {
            Pattern::Binary { segments, span } if !segments.is_empty() => {
                let entries = self.seq_entries(
                    segments,
                    |seg| seg.span,
                    |_, seg| util::binary_segment_pat_to_doc(seg),
                );
                self.element_list_to_doc("<<", ">>", entries, *span)
            }
            Pattern::Constructor {
                name,
                elements,
                span,
            } if !elements.is_empty() => concat(vec![
                text(name.clone()),
                self.pattern_elements_to_doc("(", ")", elements, *span),
            ]),
            Pattern::EnumStruct {
                type_path,
                variant,
                fields,
                span,
            } => {
                let prefix = util::enum_prefix(type_path, variant);
                self.field_pattern_list_to_doc(prefix, fields, *span)
            }
            Pattern::EnumTuple {
                type_path,
                variant,
                elements,
                span,
            } if !elements.is_empty() => concat(vec![
                text(util::enum_prefix(type_path, variant)),
                self.pattern_elements_to_doc("(", ")", elements, *span),
            ]),
            Pattern::List { elements, span } if !elements.is_empty() => {
                self.pattern_elements_to_doc("[", "]", elements, *span)
            }
            Pattern::Or { patterns, .. } => {
                let last = patterns.len() - 1;
                let items: Vec<Doc> = patterns
                    .iter()
                    .enumerate()
                    .map(|(i, alternative)| {
                        let doc = self.pattern_to_doc(alternative);
                        if i < last {
                            concat(vec![doc, text(" |")])
                        } else {
                            doc
                        }
                    })
                    .collect();
                fill(items)
            }
            Pattern::Struct {
                type_path, fields, ..
            } => self.field_pattern_list_to_doc(type_path.join("."), fields, pattern_span(pattern)),
            Pattern::Tuple { elements, span } => {
                self.pattern_elements_to_doc("(", ")", elements, *span)
            }
            // Leaves own no comment slots. A comment inside one (a typed
            // binding's type expression) relocates during attachment.
            _ => util::pattern_to_doc(pattern),
        }
    }

    /// Renders a delimited element-pattern list, recursing through the
    /// comment-aware printer so nested containers anchor their own
    /// comments.
    fn pattern_elements_to_doc(
        &mut self,
        open: &str,
        close: &str,
        elements: &[Pattern],
        owner: Span,
    ) -> Doc {
        let entries = self.seq_entries(elements, pattern_span, |printer, p| {
            printer.pattern_to_doc(p)
        });
        self.element_list_to_doc(open, close, entries, owner)
    }

    /// Renders a `Prefix{name: pattern, ...}` field list with comments
    /// anchored to their fields.
    fn field_pattern_list_to_doc(
        &mut self,
        prefix: String,
        fields: &[FieldPattern],
        owner: Span,
    ) -> Doc {
        let entries = self.seq_entries(
            fields,
            |f| f.span,
            |printer, f| {
                concat(vec![
                    text(&f.name),
                    text(": "),
                    printer.pattern_to_doc(&f.pattern),
                ])
            },
        );
        self.field_list_to_doc(text(prefix), entries, owner)
    }
}
