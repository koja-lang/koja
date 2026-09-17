//! Declaration printers, one `*_to_doc` per `Item` variant, in the order
//! [`Printer::item_to_doc`] dispatches them. The shared pieces (header,
//! type body, member entries) sit at the bottom.

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;

use super::Printer;
use super::attach::Slot;
use super::comments::trailing_doc;
use super::seq::{SeqEntry, Spacing, field_lines, vertical};
use super::util::*;

impl Printer {
    pub(super) fn item_to_doc(&mut self, item: &Item) -> Doc {
        match item {
            Item::Alias(a) => alias_to_doc(a),
            Item::Builtin(b) => self.builtin_to_doc(b),
            Item::Constant(c) => self.constant_to_doc(c),
            Item::Enum(e) => self.enum_to_doc(e),
            Item::Extend(e) => self.extend_to_doc(e),
            Item::Function(f) => self.function_to_doc(f, 0),
            Item::Impl(i) => self.impl_to_doc(i),
            Item::Protocol(p) => self.protocol_to_doc(p),
            Item::Struct(s) => self.struct_to_doc(s),
            Item::Test(t) => self.test_to_doc(t),
            Item::TypeAlias(t) => type_alias_to_doc(t),
        }
    }

    /// The struct printer minus fields.
    fn builtin_to_doc(&mut self, b: &BuiltinDecl) -> Doc {
        let mut parts = self.decl_header(
            format!(
                "{}builtin {}",
                visibility_prefix(b.visibility),
                name_texts(&b.path).join(".")
            ),
            &b.type_params,
            &[],
            &b.annotations,
            b.span,
        );
        let mut entries: Vec<SeqEntry> = b
            .functions
            .iter()
            .map(|f| self.member_function_entry(f))
            .collect();
        for test in &b.tests {
            entries.push(self.member_test_entry(test));
        }
        parts.push(self.type_body_to_doc(entries, b.span));
        concat(parts)
    }

    fn constant_to_doc(&mut self, c: &Constant) -> Doc {
        let mut parts = Vec::new();
        push_annotations(&mut parts, &c.annotations);
        parts.push(text(visibility_prefix(c.visibility)));
        parts.push(text("const "));
        parts.push(text(&c.name.text));
        if let Some(type_ann) = &c.type_annotation {
            parts.push(text(": "));
            parts.push(type_expr_to_doc(type_ann));
        }
        parts.push(text(" = "));
        parts.push(self.expr_to_doc(&c.value));
        concat(parts)
    }

    fn enum_to_doc(&mut self, e: &EnumDecl) -> Doc {
        let mut parts = self.decl_header(
            format!(
                "{}enum {}",
                visibility_prefix(e.visibility),
                name_texts(&e.path).join(".")
            ),
            &e.type_params,
            &e.conformances,
            &e.annotations,
            e.span,
        );
        let mut entries =
            self.seq_entries(&e.variants, |v| v.span, |p, v| p.enum_variant_to_doc(v));
        for item in &e.nested {
            entries.push(self.member_nested_entry(item));
        }
        for func in &e.functions {
            entries.push(self.member_function_entry(func));
        }
        for test in &e.tests {
            entries.push(self.member_test_entry(test));
        }
        parts.push(self.type_body_to_doc(entries, e.span));
        concat(parts)
    }

    fn enum_variant_to_doc(&mut self, variant: &EnumVariant) -> Doc {
        match &variant.data {
            EnumVariantData::Unit => text(&variant.name.text),
            EnumVariantData::Tuple(types) => {
                let inner: Vec<Doc> = types.iter().map(type_expr_to_doc).collect();
                concat(vec![
                    text(&variant.name.text),
                    text("("),
                    intersperse(inner, text(", ")),
                    text(")"),
                ])
            }
            EnumVariantData::Struct(fields) => {
                let entries = self.seq_entries(
                    fields,
                    |field| field.span,
                    |p, field| p.struct_field_to_doc(field),
                );
                self.field_list_to_doc(text(&variant.name.text), entries, variant.span)
            }
        }
    }

    fn extend_to_doc(&mut self, block: &ExtendBlock) -> Doc {
        let mut parts = vec![text("extend "), type_expr_to_doc(&block.target)];
        self.push_header_trailing(&mut parts, block.span);
        parts.push(self.impl_member_body_to_doc(&block.members, &block.tests, block.span));
        concat(parts)
    }

    /// `indent_cols` is the column the declaration starts at (0 at the
    /// top level, 2 inside a type body). The rendered signature is
    /// measured at that column to see whether it wraps, in which case a
    /// blank line separates it from the body.
    pub(super) fn function_to_doc(&mut self, f: &Function, indent_cols: u32) -> Doc {
        let mut parts = Vec::new();
        push_annotations(&mut parts, &f.annotations);
        let sig = self.signature_to_doc(
            format!("{}fn {}", visibility_prefix(f.visibility), f.name),
            &f.type_params,
            &f.params,
            f.span,
            f.return_type.as_ref(),
            f.error_type.as_ref(),
        );
        let sig_multiline = signature_wraps(&sig, indent_cols);
        parts.push(sig);
        self.push_header_trailing(&mut parts, f.span);

        if sig_multiline && f.body.is_some() {
            parts.push(hardline());
        }

        if let Some(body) = &f.body {
            let dangling = self.comments.take(f.span, Slot::Dangling);
            parts.push(self.body_to_doc(body, dangling));
            parts.push(hardline());
            parts.push(text("end"));
        }
        concat(parts)
    }

    /// Formats a signature (prefix, type params, parameters, return tail)
    /// with one wrapping shape shared by functions and protocol methods.
    pub(super) fn signature_to_doc(
        &mut self,
        prefix: String,
        type_params: &[TypeParam],
        params: &[Param],
        owner: Span,
        return_type: Option<&TypeExpr>,
        error_type: Option<&TypeExpr>,
    ) -> Doc {
        let entries = self.seq_entries(
            params,
            |p| *param_span(p),
            |printer, p| printer.param_to_doc(p),
        );
        let return_doc = return_signature_doc(return_type, error_type);

        let params_inline = if entries.is_empty() {
            nil()
        } else if self.entries_comment_free(&entries, owner) {
            let params_doc: Vec<Doc> = entries.into_iter().map(|e| e.doc).collect();
            delimited_list("(", ")", params_doc)
        } else {
            // A parameter comment forces the broken signature. The
            // hardlines make `signature_wraps` report multiline, which
            // adds the blank line before the body.
            let stragglers = self.comments.take(owner, Slot::Stragglers);
            broken_list("(", ")", field_lines(entries, stragglers))
        };

        let head = concat(vec![
            text(prefix),
            type_params_doc(type_params),
            params_inline,
        ]);
        match return_doc {
            Some(ret) => group(concat(vec![
                head,
                group(indent(2, concat(vec![line(), ret]))),
            ])),
            None => head,
        }
    }

    fn param_to_doc(&mut self, p: &Param) -> Doc {
        match p {
            Param::Self_ { .. } => text("self"),
            Param::Regular {
                name,
                type_expr,
                default,
                ..
            } => {
                let mut parts = vec![text(&name.text), text(": "), type_expr_to_doc(type_expr)];
                if let Some(d) = default {
                    parts.push(text(" = "));
                    parts.push(self.expr_to_doc(d));
                }
                concat(parts)
            }
        }
    }

    /// Conditional bounds print inline on the matching target arg
    /// (`impl Equality for List<T: Equality>`).
    fn impl_to_doc(&mut self, block: &ImplBlock) -> Doc {
        let mut parts = vec![
            text("impl "),
            type_expr_to_doc(&block.trait_expr),
            text(" for "),
            impl_target_to_doc(&block.target, &block.target_bounds),
        ];
        self.push_header_trailing(&mut parts, block.span);
        parts.push(self.impl_member_body_to_doc(&block.members, &block.tests, block.span));
        concat(parts)
    }

    /// Shared body for `impl` and `extend`.
    fn impl_member_body_to_doc(
        &mut self,
        members: &[ImplMember],
        tests: &[TestDecl],
        owner: Span,
    ) -> Doc {
        let mut entries: Vec<SeqEntry> = members
            .iter()
            .map(|member| match member {
                ImplMember::Function(f) => self.member_function_entry(f),
                ImplMember::TypeAlias(ta) => {
                    let doc = concat(vec![
                        text(format!("type {} = ", ta.name)),
                        type_expr_to_doc(&ta.type_expr),
                    ]);
                    self.entry(ta.span, ta.span.start.line, true, doc)
                }
            })
            .collect();
        for test in tests {
            entries.push(self.member_test_entry(test));
        }
        self.type_body_to_doc(entries, owner)
    }

    fn protocol_to_doc(&mut self, p: &ProtocolDecl) -> Doc {
        let mut parts = self.decl_header(
            format!(
                "{}protocol {}",
                visibility_prefix(p.visibility),
                name_texts(&p.path).join(".")
            ),
            &p.type_params,
            &[],
            &p.annotations,
            p.span,
        );
        let mut entries = Vec::new();
        for method in &p.methods {
            let doc = self.protocol_method_to_doc(method);
            let start_line = lead_line(&method.annotations, method.span);
            entries.push(self.entry(method.span, start_line, true, doc));
        }
        parts.push(self.type_body_to_doc(entries, p.span));
        concat(parts)
    }

    /// Signature only, or signature with a default body.
    fn protocol_method_to_doc(&mut self, m: &ProtocolMethod) -> Doc {
        let mut parts = Vec::new();
        push_annotations(&mut parts, &m.annotations);
        parts.push(self.signature_to_doc(
            format!("fn {}", m.name),
            &m.type_params,
            &m.params,
            m.span,
            m.return_type.as_ref(),
            m.error_type.as_ref(),
        ));
        self.push_header_trailing(&mut parts, m.span);

        if let Some(body) = &m.body {
            let dangling = self.comments.take(m.span, Slot::Dangling);
            parts.push(self.body_to_doc(body, dangling));
            parts.push(hardline());
            parts.push(text("end"));
        }
        concat(parts)
    }

    fn struct_to_doc(&mut self, s: &StructDecl) -> Doc {
        let mut parts = self.decl_header(
            format!(
                "{}struct {}",
                visibility_prefix(s.visibility),
                name_texts(&s.path).join(".")
            ),
            &s.type_params,
            &s.conformances,
            &s.annotations,
            s.span,
        );
        let mut entries = self.seq_entries(&s.fields, |f| f.span, |p, f| p.struct_field_to_doc(f));
        for item in &s.nested {
            entries.push(self.member_nested_entry(item));
        }
        for func in &s.functions {
            entries.push(self.member_function_entry(func));
        }
        for test in &s.tests {
            entries.push(self.member_test_entry(test));
        }
        parts.push(self.type_body_to_doc(entries, s.span));
        concat(parts)
    }

    /// The field itself (`name: Type [= default]`), no comment handling.
    /// The joining comma of enum struct variants must land between the
    /// field and its trailing comment.
    fn struct_field_to_doc(&mut self, field: &StructField) -> Doc {
        let mut d = concat(vec![
            text(&field.name.text),
            text(": "),
            type_expr_to_doc(&field.type_expr),
        ]);
        if let Some(default) = &field.default {
            d = concat(vec![d, text(" = "), self.expr_to_doc(default)]);
        }
        d
    }

    fn test_to_doc(&mut self, t: &TestDecl) -> Doc {
        let mut parts = vec![text(format!(
            "test \"{}\"",
            escape_string_literal(&t.description)
        ))];
        self.push_header_trailing(&mut parts, t.span);
        let dangling = self.comments.take(t.span, Slot::Dangling);
        parts.push(self.body_to_doc(&t.body, dangling));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

    /// The header parts shared by `struct`, `builtin`, `enum`, and
    /// `protocol`: annotations, `header<T>` where `header` is the
    /// `{vis}{keyword} {name}` text, the conformance list, and the
    /// header line's trailing comment.
    fn decl_header(
        &mut self,
        mut header: String,
        type_params: &[TypeParam],
        conformances: &[TypeExpr],
        annotations: &[Annotation],
        owner: Span,
    ) -> Vec<Doc> {
        let mut parts = Vec::new();
        push_annotations(&mut parts, annotations);
        if !type_params.is_empty() {
            header.push('<');
            header.push_str(&format_type_params(type_params));
            header.push('>');
        }
        parts.push(text(header));
        if !conformances.is_empty() {
            parts.push(conformance_header_doc(conformances));
        }
        self.push_header_trailing(&mut parts, owner);
        parts
    }

    /// Appends the header-line trailing comment, if any.
    pub(super) fn push_header_trailing(&mut self, parts: &mut Vec<Doc>, owner: Span) {
        if let Some(tc) = trailing_doc(&self.comments.take(owner, Slot::HeaderTrailing)) {
            parts.push(tc);
        }
    }

    /// Renders a type body (members between the header and `end`),
    /// indented, with the block's dangling comments before `end`.
    /// Members print in source order. The AST keeps fields, nested
    /// types, functions, and `test` blocks in separate lists, so the
    /// entries arrive grouped by kind and sort back by line here.
    fn type_body_to_doc(&mut self, mut entries: Vec<SeqEntry>, owner: Span) -> Doc {
        entries.sort_by_key(|entry| entry.start_line);
        let dangling = self.comments.take(owner, Slot::Dangling);
        let body = if entries.is_empty() && dangling.is_empty() {
            nil()
        } else {
            indent(
                2,
                concat(vec![
                    hardline(),
                    vertical(entries, Spacing::Tight, dangling),
                ]),
            )
        };
        concat(vec![body, hardline(), text("end")])
    }

    fn member_function_entry(&mut self, func: &Function) -> SeqEntry {
        let doc = self.function_to_doc(func, 2);
        self.entry(
            func.span,
            lead_line(&func.annotations, func.span),
            true,
            doc,
        )
    }

    fn member_nested_entry(&mut self, item: &Item) -> SeqEntry {
        let doc = self.item_to_doc(item);
        self.entry(*item_span(item), item_start_line(item), true, doc)
    }

    fn member_test_entry(&mut self, t: &TestDecl) -> SeqEntry {
        let doc = self.test_to_doc(t);
        self.entry(t.span, t.span.start.line, true, doc)
    }
}
