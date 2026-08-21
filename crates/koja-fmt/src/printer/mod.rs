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
//!
//! Internally the printer is split into submodules:
//! - [`attach`]: the comment attachment pass and table
//! - [`comments`]: comment rendering helpers
//! - [`expr`]: expression and match/cond/receive arm formatting
//! - [`pattern`]: comment-aware pattern formatting
//! - [`seq`]: the commented-sequence builder and its layout backends
//! - [`util`]: stateless helpers for types, patterns, spans, etc.

mod attach;
mod comments;
mod expr;
mod pattern;
mod seq;
mod util;

use std::mem;

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;
use koja_ast::token::Token;

use attach::{CommentTable, Slot};
use comments::{leading_docs, trailing_doc};
use seq::{SeqEntry, Spacing, field_lines, vertical};
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

/// Holds the comment table the printer consumes while rendering.
struct Printer {
    comments: CommentTable,
}

/// A single top-level element, either a declaration or a statement. Used
/// by [`Printer::print_file`] to merge `file.items` and `file.body` back
/// into source order for scripts.
enum TopLevel<'a> {
    Item(&'a Item),
    Stmt(&'a Statement),
}

impl TopLevel<'_> {
    fn start_line(&self) -> u32 {
        match self {
            TopLevel::Item(item) => item_start_line(item),
            TopLevel::Stmt(stmt) => stmt_start_line(stmt),
        }
    }

    /// The span that keys this node's comments. For items that is the
    /// declaration span with annotations excluded, matching the
    /// attachment pass.
    fn key(&self) -> Span {
        match self {
            TopLevel::Item(item) => *item_span(item),
            TopLevel::Stmt(stmt) => stmt_span(stmt),
        }
    }

    /// Whether this element forces blank-line separation from its
    /// neighbors. Multi-line declarations, annotated declarations, and
    /// block statements read better with surrounding blank lines, while
    /// bare single-line `const`/`alias` declarations flow with adjacent
    /// statements.
    fn is_block(&self) -> bool {
        match self {
            TopLevel::Item(item @ (Item::Constant(_) | Item::Alias(_) | Item::TypeAlias(_))) => {
                !item_annotations(item).is_empty()
            }
            TopLevel::Item(_) => true,
            TopLevel::Stmt(stmt) => stmt_is_block(stmt),
        }
    }
}

impl Printer {
    /// Formats an entire file, with top-level declarations and statements
    /// merged in source order. `.kojs` scripts carry statements in
    /// `file.body`, and `.koja` modules leave it `None`.
    fn print_file(&mut self, file: &File) -> Doc {
        let mut nodes: Vec<TopLevel<'_>> = file.items.iter().map(TopLevel::Item).collect();
        if let Some(body) = &file.body {
            nodes.extend(body.iter().map(TopLevel::Stmt));
        }
        nodes.sort_by_key(TopLevel::start_line);

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
            entries.push(SeqEntry {
                doc,
                end_line: key.end.line,
                force_blank,
                is_block: node.is_block(),
                leading: self.comments.take(key, Slot::Leading),
                start_line: node.start_line(),
                trailing: self.comments.take(key, Slot::Trailing),
            });
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

    fn item_to_doc(&mut self, item: &Item) -> Doc {
        match item {
            Item::Struct(s) => self.struct_to_doc(s),
            Item::Builtin(b) => self.builtin_to_doc(b),
            Item::Enum(e) => self.enum_to_doc(e),
            Item::Extend(e) => self.extend_to_doc(e),
            Item::Function(f) => self.function_to_doc(f, 0),
            Item::Impl(i) => self.impl_to_doc(i),
            Item::Protocol(p) => self.protocol_to_doc(p),
            Item::Alias(a) => alias_to_doc(a),
            Item::Constant(c) => self.constant_to_doc(c),
            Item::TypeAlias(t) => type_alias_to_doc(t),
        }
    }

    /// Builds the member entry for a function inside a type body.
    fn member_function_entry(&mut self, func: &Function) -> SeqEntry {
        let start_line = func
            .annotations
            .first()
            .map_or(func.span.start.line, |a| a.span.start.line);
        SeqEntry {
            doc: self.function_to_doc(func, 2),
            end_line: func.span.end.line,
            force_blank: true,
            is_block: true,
            leading: self.comments.take(func.span, Slot::Leading),
            start_line,
            trailing: self.comments.take(func.span, Slot::Trailing),
        }
    }

    /// Builds the member entry for a nested type declaration.
    fn member_nested_entry(&mut self, item: &Item) -> SeqEntry {
        let key = *item_span(item);
        SeqEntry {
            doc: self.item_to_doc(item),
            end_line: key.end.line,
            force_blank: true,
            is_block: true,
            leading: self.comments.take(key, Slot::Leading),
            start_line: item_start_line(item),
            trailing: self.comments.take(key, Slot::Trailing),
        }
    }

    /// Renders a type body (members between the header and `end`),
    /// indented, with the block's dangling comments before `end`.
    fn type_body_to_doc(&mut self, entries: Vec<SeqEntry>, owner: Span) -> Doc {
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

    /// Appends the header-line trailing comment, if any.
    fn push_header_trailing(&mut self, parts: &mut Vec<Doc>, owner: Span) {
        if let Some(tc) = trailing_doc(&self.comments.take(owner, Slot::HeaderTrailing)) {
            parts.push(tc);
        }
    }

    /// Formats a `struct` declaration with its fields and members.
    fn struct_to_doc(&mut self, s: &StructDecl) -> Doc {
        let mut parts = Vec::new();
        if let Some(doc) = annotations_to_doc(&s.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }
        let mut header = format!(
            "{}struct {}",
            visibility_prefix(s.visibility),
            s.path.join(".")
        );
        if !s.type_params.is_empty() {
            header.push('<');
            header.push_str(&util::format_type_params(&s.type_params));
            header.push('>');
        }
        parts.push(text(header));
        if !s.conformances.is_empty() {
            parts.push(util::conformance_header_doc(&s.conformances));
        }
        self.push_header_trailing(&mut parts, s.span);

        let mut entries = Vec::new();
        for field in &s.fields {
            entries.push(self.struct_field_entry(field));
        }
        for item in &s.nested {
            entries.push(self.member_nested_entry(item));
        }
        for func in &s.functions {
            entries.push(self.member_function_entry(func));
        }
        parts.push(self.type_body_to_doc(entries, s.span));
        concat(parts)
    }

    /// Formats a `builtin` declaration, the struct printer minus fields.
    fn builtin_to_doc(&mut self, b: &BuiltinDecl) -> Doc {
        let mut parts = Vec::new();
        if let Some(doc) = annotations_to_doc(&b.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }
        let mut header = format!(
            "{}builtin {}",
            visibility_prefix(b.visibility),
            b.path.join(".")
        );
        if !b.type_params.is_empty() {
            header.push('<');
            header.push_str(&util::format_type_params(&b.type_params));
            header.push('>');
        }
        parts.push(text(header));
        self.push_header_trailing(&mut parts, b.span);

        let entries = b
            .functions
            .iter()
            .map(|f| self.member_function_entry(f))
            .collect();
        parts.push(self.type_body_to_doc(entries, b.span));
        concat(parts)
    }

    /// Builds the member entry for a struct field.
    fn struct_field_entry(&mut self, field: &StructField) -> SeqEntry {
        SeqEntry {
            doc: self.struct_field_bare_to_doc(field),
            end_line: field.span.end.line,
            force_blank: false,
            is_block: false,
            leading: self.comments.take(field.span, Slot::Leading),
            start_line: field.span.start.line,
            trailing: self.comments.take(field.span, Slot::Trailing),
        }
    }

    /// The field itself (`name: Type [= default]`), no comment handling.
    /// The joining comma of enum struct variants must land between the
    /// field and its trailing comment.
    fn struct_field_bare_to_doc(&mut self, field: &StructField) -> Doc {
        let mut d = concat(vec![
            text(&field.name),
            text(": "),
            type_expr_to_doc(&field.type_expr),
        ]);
        if let Some(default) = &field.default {
            d = concat(vec![d, text(" = "), self.expr_to_doc(default)]);
        }
        d
    }

    /// Formats an `enum` declaration with its variants.
    fn enum_to_doc(&mut self, e: &EnumDecl) -> Doc {
        let mut parts = Vec::new();
        if let Some(doc) = annotations_to_doc(&e.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }
        let mut header = format!(
            "{}enum {}",
            visibility_prefix(e.visibility),
            e.path.join(".")
        );
        if !e.type_params.is_empty() {
            header.push('<');
            header.push_str(&util::format_type_params(&e.type_params));
            header.push('>');
        }
        parts.push(text(header));
        if !e.conformances.is_empty() {
            parts.push(util::conformance_header_doc(&e.conformances));
        }
        self.push_header_trailing(&mut parts, e.span);

        let mut entries = Vec::new();
        for variant in &e.variants {
            entries.push(SeqEntry {
                doc: self.enum_variant_to_doc(variant),
                end_line: variant.span.end.line,
                force_blank: false,
                is_block: false,
                leading: self.comments.take(variant.span, Slot::Leading),
                start_line: variant.span.start.line,
                trailing: self.comments.take(variant.span, Slot::Trailing),
            });
        }
        for item in &e.nested {
            entries.push(self.member_nested_entry(item));
        }
        for func in &e.functions {
            entries.push(self.member_function_entry(func));
        }
        parts.push(self.type_body_to_doc(entries, e.span));
        concat(parts)
    }

    /// Formats a single enum variant (unit, tuple, or struct form).
    fn enum_variant_to_doc(&mut self, variant: &EnumVariant) -> Doc {
        match &variant.data {
            EnumVariantData::Unit => text(&variant.name),
            EnumVariantData::Tuple(types) => {
                let inner: Vec<Doc> = types.iter().map(type_expr_to_doc).collect();
                concat(vec![
                    text(&variant.name),
                    text("("),
                    intersperse(inner, text(", ")),
                    text(")"),
                ])
            }
            EnumVariantData::Struct(fields) => {
                let entries = self.seq_entries(
                    fields,
                    |field| field.span,
                    |p, field| p.struct_field_bare_to_doc(field),
                );
                self.field_list_to_doc(text(&variant.name), entries, variant.span)
            }
        }
    }

    /// Formats a `fn` declaration (annotation, signature, body, `end`).
    ///
    /// `indent_cols` is the column the declaration starts at (0 at the top
    /// level, 2 inside a type body), used to detect from the rendered
    /// signature whether it wraps across lines, in which case a blank
    /// line separates it from the body.
    fn function_to_doc(&mut self, f: &Function, indent_cols: u32) -> Doc {
        let mut parts = Vec::new();

        if let Some(doc) = annotations_to_doc(&f.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }

        let sig = self.function_sig_to_doc(f);
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

    /// Formats a function signature (visibility, name, type params, params,
    /// return type) with group/indent for line-breaking.
    fn function_sig_to_doc(&mut self, f: &Function) -> Doc {
        self.signature_to_doc(
            format!("{}fn {}", visibility_prefix(f.visibility), f.name),
            &f.type_params,
            &f.params,
            f.span,
            f.return_type.as_ref(),
            f.error_type.as_ref(),
        )
    }

    /// Formats a signature (prefix, type params, parameters, return tail)
    /// with one wrapping shape shared by functions and protocol methods.
    fn signature_to_doc(
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
            group(concat(vec![
                text("("),
                indent(
                    2,
                    concat(vec![
                        softline(),
                        intersperse(params_doc, concat(vec![text(","), line()])),
                        trailing_comma(),
                    ]),
                ),
                softline(),
                text(")"),
            ]))
        } else {
            // A parameter comment forces the broken signature. The
            // hardlines make `signature_wraps` report multiline, which
            // adds the blank line before the body.
            let stragglers = self.comments.take(owner, Slot::Stragglers);
            concat(vec![
                text("("),
                indent(2, field_lines(entries, stragglers)),
                hardline(),
                text(")"),
            ])
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

    /// Formats a function parameter (`self` or `name: Type`).
    fn param_to_doc(&mut self, p: &Param) -> Doc {
        match p {
            Param::Self_ { .. } => text("self"),
            Param::Regular {
                name,
                type_expr,
                default,
                ..
            } => {
                let mut parts = Vec::new();
                parts.push(text(name.clone()));
                parts.push(text(": "));
                parts.push(type_expr_to_doc(type_expr));
                if let Some(d) = default {
                    parts.push(text(" = "));
                    parts.push(self.expr_to_doc(d));
                }
                concat(parts)
            }
        }
    }

    /// Formats a `protocol` declaration with its method signatures.
    fn protocol_to_doc(&mut self, p: &ProtocolDecl) -> Doc {
        let mut parts = Vec::new();
        if let Some(doc) = annotations_to_doc(&p.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }
        let mut header = format!("{}protocol {}", visibility_prefix(p.visibility), p.name);
        if !p.type_params.is_empty() {
            header.push('<');
            header.push_str(&util::format_type_params(&p.type_params));
            header.push('>');
        }
        parts.push(text(header));
        self.push_header_trailing(&mut parts, p.span);

        let mut entries = Vec::new();
        for method in &p.methods {
            let start_line = method
                .annotations
                .first()
                .map_or(method.span.start.line, |a| a.span.start.line);
            entries.push(SeqEntry {
                doc: self.protocol_method_to_doc(method),
                end_line: method.span.end.line,
                force_blank: true,
                is_block: true,
                leading: self.comments.take(method.span, Slot::Leading),
                start_line,
                trailing: self.comments.take(method.span, Slot::Trailing),
            });
        }
        parts.push(self.type_body_to_doc(entries, p.span));
        concat(parts)
    }

    /// Formats a protocol method (signature only, or with default body).
    fn protocol_method_to_doc(&mut self, m: &ProtocolMethod) -> Doc {
        let mut parts = Vec::new();
        if let Some(doc) = annotations_to_doc(&m.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }

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

    /// Formats an `impl Protocol for Type` block. Conditional
    /// bounds print inline on the matching target arg
    /// (`impl Equality for List<T: Equality>`).
    fn impl_to_doc(&mut self, block: &ImplBlock) -> Doc {
        let mut parts = vec![
            text("impl "),
            type_expr_to_doc(&block.trait_expr),
            text(" for "),
            impl_target_to_doc(&block.target, &block.target_bounds),
        ];
        self.push_header_trailing(&mut parts, block.span);
        parts.push(self.impl_member_body_to_doc(&block.members, block.span));
        concat(parts)
    }

    /// Formats an `extend Type` block.
    fn extend_to_doc(&mut self, block: &ExtendBlock) -> Doc {
        let mut parts = vec![text("extend "), type_expr_to_doc(&block.target)];
        self.push_header_trailing(&mut parts, block.span);
        parts.push(self.impl_member_body_to_doc(&block.members, block.span));
        concat(parts)
    }

    /// Shared body for `impl` and `extend`, indented members + `end`.
    fn impl_member_body_to_doc(&mut self, members: &[ImplMember], owner: Span) -> Doc {
        let entries = members
            .iter()
            .map(|member| match member {
                ImplMember::Function(f) => self.member_function_entry(f),
                ImplMember::TypeAlias(ta) => SeqEntry {
                    doc: concat(vec![
                        text(format!("type {} = ", ta.name)),
                        type_expr_to_doc(&ta.type_expr),
                    ]),
                    end_line: ta.span.end.line,
                    force_blank: true,
                    is_block: true,
                    leading: self.comments.take(ta.span, Slot::Leading),
                    start_line: ta.span.start.line,
                    trailing: self.comments.take(ta.span, Slot::Trailing),
                },
            })
            .collect();
        self.type_body_to_doc(entries, owner)
    }

    /// Formats a `const` declaration.
    fn constant_to_doc(&mut self, c: &Constant) -> Doc {
        let mut parts = Vec::new();
        if let Some(doc) = annotations_to_doc(&c.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }
        parts.push(text(visibility_prefix(c.visibility)));
        parts.push(text("const "));
        parts.push(text(&c.name));
        if let Some(type_ann) = &c.type_annotation {
            parts.push(text(": "));
            parts.push(type_expr_to_doc(type_ann));
        }
        parts.push(text(" = "));
        parts.push(self.expr_to_doc(&c.value));
        concat(parts)
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
                SeqEntry {
                    doc: to_doc(self, item),
                    end_line: key.end.line,
                    force_blank: false,
                    is_block: false,
                    leading: self.comments.take(key, Slot::Leading),
                    start_line: key.start.line,
                    trailing: self.comments.take(key, Slot::Trailing),
                }
            })
            .collect()
    }

    /// True when no comment sits inside the construct, neither anchored
    /// to an element nor pending before the closing delimiter.
    fn entries_comment_free(&self, entries: &[SeqEntry], owner: Span) -> bool {
        entries.iter().all(SeqEntry::comment_free) && !self.comments.has(owner, Slot::Stragglers)
    }

    /// True when an assigned value renders as a multi-line block, which
    /// forces the break after `=`. A closure that renders inline does
    /// not count, so `ref = Task.async(fn () -> Int 42 end)` stays glued.
    fn forces_assignment_break(&self, expr: &Expr) -> bool {
        if matches!(expr.kind, ExprKind::Closure { .. }) {
            return !self.closure_renders_inline(expr);
        }
        if is_block_expr(expr) {
            return true;
        }
        match &expr.kind {
            ExprKind::Call { args, .. } => {
                args.iter().any(|a| self.forces_assignment_break(&a.value))
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.forces_assignment_break(receiver)
                    || args.iter().any(|a| self.forces_assignment_break(&a.value))
            }
            ExprKind::Binary { right, .. } => self.forces_assignment_break(right),
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.forces_assignment_break(condition)
                    || self.forces_assignment_break(then_expr)
                    || self.forces_assignment_break(else_expr)
            }
            _ => false,
        }
    }

    /// Formats a single statement.
    pub(super) fn statement_to_doc(&mut self, stmt: &Statement) -> Doc {
        match stmt {
            Statement::Expr(expr) => self.expr_to_doc(expr),
            Statement::Assignment {
                target,
                type_annotation,
                value,
                span,
            } => {
                let target_doc = text(target.segments.join("."));
                let lhs = if let Some(te) = type_annotation {
                    concat(vec![target_doc, text(": "), type_expr_to_doc(te)])
                } else {
                    target_doc
                };
                let inline_closure = self.closure_renders_inline(value);
                // Decided before rendering: rendering consumes the
                // comment table the predicates consult.
                let breaks = self.forces_assignment_break(value);
                let value_doc = self.expr_to_doc(value);
                if inline_closure {
                    // Stay inline when the closure fits, breaking after `=`
                    // (soft line) only when it overflows the line width.
                    group(concat(vec![
                        lhs,
                        text(" ="),
                        indent(2, concat(vec![line(), value_doc])),
                    ]))
                } else if is_heredoc(value) {
                    // Both opener placements are idiomatic. Preserve the
                    // author's choice: glued (`x = """`) when the literal
                    // starts on the assignment's line, otherwise broken
                    // (newline after `=`, block indented like match/cond).
                    if value.span.start.line == span.start.line {
                        concat(vec![lhs, text(" = "), value_doc])
                    } else {
                        concat(vec![
                            lhs,
                            text(" ="),
                            indent(2, concat(vec![hardline(), value_doc])),
                        ])
                    }
                } else if breaks {
                    concat(vec![
                        lhs,
                        text(" ="),
                        indent(2, concat(vec![hardline(), value_doc])),
                    ])
                } else {
                    group(concat(vec![lhs, text(" = "), value_doc]))
                }
            }
            Statement::CompoundAssign {
                target, op, value, ..
            } => {
                let op_str = match op {
                    CompoundOp::Add => "+=",
                    CompoundOp::Div => "/=",
                    CompoundOp::Mul => "*=",
                    CompoundOp::Sub => "-=",
                };
                let breaks = self.forces_assignment_break(value);
                let value_doc = self.expr_to_doc(value);
                if breaks {
                    concat(vec![
                        text(target.segments.join(".")),
                        text(format!(" {}", op_str)),
                        indent(2, concat(vec![hardline(), value_doc])),
                    ])
                } else {
                    concat(vec![
                        text(target.segments.join(".")),
                        text(format!(" {} ", op_str)),
                        value_doc,
                    ])
                }
            }
            Statement::Destructure { pattern, value, .. } => {
                let lhs = self.pattern_to_doc(pattern);
                let breaks = self.forces_assignment_break(value);
                let value_doc = self.expr_to_doc(value);
                if breaks {
                    concat(vec![
                        lhs,
                        text(" ="),
                        indent(2, concat(vec![hardline(), value_doc])),
                    ])
                } else {
                    group(concat(vec![lhs, text(" = "), value_doc]))
                }
            }
            Statement::Return { value, .. } => match value {
                Some(v) => concat(vec![text("return "), self.expr_to_doc(v)]),
                None => text("return"),
            },
            Statement::Break { .. } => text("break"),
        }
    }

    /// Renders a list of statements with their attached comments, ending
    /// with the region's `dangling` comments.
    pub(super) fn statements_to_doc(&mut self, stmts: &[Statement], dangling: Vec<Comment>) -> Doc {
        let entries: Vec<SeqEntry> = stmts
            .iter()
            .map(|stmt| {
                let key = stmt_span(stmt);
                SeqEntry {
                    doc: self.statement_to_doc(stmt),
                    end_line: key.end.line,
                    force_blank: false,
                    is_block: stmt_is_block(stmt),
                    leading: self.comments.take(key, Slot::Leading),
                    start_line: key.start.line,
                    trailing: self.comments.take(key, Slot::Trailing),
                }
            })
            .collect();
        vertical(entries, Spacing::Preserve, dangling)
    }

    /// Formats an indented body block (the statements between a keyword
    /// and `end`). `dangling` holds the comments between the last
    /// statement and the terminator.
    pub(super) fn body_to_doc(&mut self, stmts: &[Statement], dangling: Vec<Comment>) -> Doc {
        if stmts.is_empty() && dangling.is_empty() {
            return nil();
        }
        indent(
            2,
            concat(vec![hardline(), self.statements_to_doc(stmts, dangling)]),
        )
    }
}
