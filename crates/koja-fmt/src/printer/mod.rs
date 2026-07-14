//! Pretty-printer: converts a parsed Koja AST back into formatted source code.
//!
//! The entry point is [`file_to_doc`], which produces a [`Doc`] document tree
//! that the renderer in [`crate::doc`] lays out to a target line width.
//!
//! Internally the printer is split into submodules:
//! - [`comments`]: source comment tracking and re-attachment
//! - [`expr`]: expression and match/cond/receive arm formatting
//! - [`util`]: stateless helpers for types, patterns, spans, etc.

mod comments;
mod expr;
mod util;

use std::mem;

use crate::doc::*;
use koja_ast::ast::*;

use comments::CommentCursor;
use util::*;

/// Converts a parsed file into a `Doc` tree ready for rendering.
pub fn file_to_doc(file: &File) -> Doc {
    let mut p = Printer::new(&file.comments);
    p.print_file(file)
}

/// Holds the comment cursor used during formatting to re-attach comments
/// at their original source positions.
pub(super) struct Printer<'a> {
    pub(super) comments: CommentCursor<'a>,
}

/// A single script top-level element, either a declaration or a
/// statement. Used by [`Printer::print_script`] to merge `file.items`
/// and `file.body` back into source order.
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

    fn end_line(&self) -> u32 {
        match self {
            TopLevel::Item(item) => item_span(item).end.line,
            TopLevel::Stmt(stmt) => stmt_end_line(stmt),
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

impl<'a> Printer<'a> {
    fn new(comments: &'a [Comment]) -> Self {
        Self {
            comments: CommentCursor::new(comments),
        }
    }

    /// Formats an entire file. `.kojs` scripts carry top-level
    /// statements in `file.body`. Those interleave with declarations and
    /// need the script-aware renderer. `.koja` modules leave `body` as
    /// `None` and route through the declaration-only module renderer.
    fn print_file(&mut self, file: &File) -> Doc {
        if file.body.is_some() {
            self.print_script(file)
        } else {
            self.print_module(file)
        }
    }

    /// Formats a declaration-only module: items with interleaved comments.
    fn print_module(&mut self, file: &File) -> Doc {
        let mut parts: Vec<Doc> = Vec::new();
        let mut emitted = false;

        let mut i = 0;

        while i < file.items.len() {
            if matches!(&file.items[i], Item::Constant(_) | Item::Alias(_)) {
                if emitted {
                    parts.push(hardline());
                }

                let anchor = mem::discriminant(&file.items[i]);
                let mut prev_end: Option<u32> = None;
                let mut prev_annotated = false;
                while i < file.items.len() && mem::discriminant(&file.items[i]) == anchor {
                    let item = &file.items[i];
                    let annotated = !item_annotations(item).is_empty();
                    let start_line = item_start_line(item);
                    let next_line = self.comments.peek_before(start_line).unwrap_or(start_line);
                    // Annotated declarations always get surrounding blank
                    // lines; otherwise a single blank is preserved from the
                    // source (a wider gap collapses to one).
                    let source_has_blank = prev_end.is_some_and(|prev| next_line > prev + 1);
                    if prev_end.is_some() && (source_has_blank || annotated || prev_annotated) {
                        parts.push(hardline());
                    }
                    let (comment_docs, last_comment_line) = self.comments.drain_before(start_line);
                    for c in comment_docs {
                        parts.push(c);
                    }
                    if let Some(lcl) = last_comment_line
                        && start_line > lcl + 1
                    {
                        parts.push(hardline());
                    }
                    parts.push(self.item_to_doc(item));
                    parts.push(hardline());
                    prev_end = Some(item_span(item).end.line);
                    prev_annotated = annotated;
                    i += 1;
                }

                emitted = true;
            } else {
                let item = &file.items[i];
                let span = item_span(item);
                if emitted {
                    parts.push(hardline());
                }
                let (comment_docs, last_comment_line) = self.comments.drain_before(span.start.line);
                for c in comment_docs {
                    parts.push(c);
                }
                if let Some(lcl) = last_comment_line
                    && span.start.line > lcl + 1
                {
                    parts.push(hardline());
                }
                parts.push(self.item_to_doc(item));
                parts.push(hardline());
                emitted = true;
                i += 1;
            }
        }

        let trailing = self.comments.drain_rest();
        for c in trailing {
            parts.push(c);
        }

        concat(parts)
    }

    /// Formats a script: top-level declarations and statements merged in
    /// source order. The parser splits them into `file.items` and
    /// `file.body`, so we re-interleave by start line to avoid reordering
    /// the user's code. Spacing mirrors [`Self::statements_to_doc`]:
    /// blank lines are preserved from the source and forced around block
    /// constructs (declarations, `if`/`while`/`for`/...).
    fn print_script(&mut self, file: &File) -> Doc {
        let mut nodes: Vec<TopLevel<'_>> = Vec::new();
        for item in &file.items {
            nodes.push(TopLevel::Item(item));
        }
        if let Some(body) = &file.body {
            for stmt in body {
                nodes.push(TopLevel::Stmt(stmt));
            }
        }
        nodes.sort_by_key(TopLevel::start_line);

        let mut parts: Vec<Doc> = Vec::new();
        let mut prev_end: u32 = 0;
        for (i, node) in nodes.iter().enumerate() {
            let start_line = node.start_line();
            let next_line = self.comments.peek_before(start_line).unwrap_or(start_line);
            let (comment_docs, last_comment_line) = self.comments.drain_before(start_line);

            if i > 0 {
                let source_has_blank = next_line > prev_end + 1;
                if source_has_blank || node.is_block() || nodes[i - 1].is_block() {
                    parts.push(hardline());
                }
            }

            for c in comment_docs {
                parts.push(c);
            }
            if let Some(lcl) = last_comment_line
                && start_line > lcl + 1
            {
                parts.push(hardline());
            }

            match node {
                TopLevel::Item(item) => parts.push(self.item_to_doc(item)),
                TopLevel::Stmt(stmt) => parts.push(self.statement_to_doc(stmt)),
            }
            let end_line = node.end_line();
            if let Some(tc) = self.comments.drain_trailing(end_line) {
                parts.push(tc);
            }
            parts.push(hardline());
            prev_end = end_line;
        }

        let trailing = self.comments.drain_rest();
        for c in trailing {
            parts.push(c);
        }

        concat(parts)
    }

    fn item_to_doc(&mut self, item: &Item) -> Doc {
        match item {
            Item::Struct(s) => self.struct_to_doc(s),
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

    /// Formats a `struct` declaration with its fields and trailing comments.
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

        let mut body = Vec::new();
        for field in &s.fields {
            body.push(hardline());
            let (cdocs, _) = self.comments.drain_before(field.span.start.line);
            for c in cdocs {
                body.push(c);
            }
            body.push(self.struct_field_to_doc(field));
        }
        for (i, func) in s.functions.iter().enumerate() {
            if i > 0 || !s.fields.is_empty() {
                body.push(hardline());
            }
            body.push(hardline());
            body.push(self.function_to_doc(func, 2));
        }
        self.push_trailing_body_comments(&mut body, s.span.end.line);
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

    /// Formats a single struct field with optional default value.
    fn struct_field_to_doc(&mut self, field: &StructField) -> Doc {
        let mut d = concat(vec![
            text(&field.name),
            text(": "),
            type_expr_to_doc(&field.type_expr),
        ]);
        if let Some(default) = &field.default {
            d = concat(vec![d, text(" = "), self.expr_to_doc(default)]);
        }
        if let Some(tc) = self.comments.drain_trailing(field.span.end.line) {
            d = concat(vec![d, tc]);
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

        let mut body = Vec::new();
        for variant in &e.variants {
            body.push(hardline());
            let (cdocs, _) = self.comments.drain_before(variant.span.start.line);
            for c in cdocs {
                body.push(c);
            }
            body.push(self.enum_variant_to_doc(variant));
        }
        for (i, func) in e.functions.iter().enumerate() {
            if i > 0 || !e.variants.is_empty() {
                body.push(hardline());
            }
            body.push(hardline());
            body.push(self.function_to_doc(func, 2));
        }
        self.push_trailing_body_comments(&mut body, e.span.end.line);
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
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
                let field_docs: Vec<Doc> =
                    fields.iter().map(|f| self.struct_field_to_doc(f)).collect();
                concat(vec![
                    text(&variant.name),
                    text(" {"),
                    indent(
                        2,
                        concat(vec![
                            hardline(),
                            intersperse(field_docs, concat(vec![text(","), hardline()])),
                            text(","),
                        ]),
                    ),
                    hardline(),
                    text("}"),
                ])
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

        let (comment_docs, _) = self.comments.drain_before(f.span.start.line);
        parts.extend(comment_docs);

        if let Some(doc) = annotations_to_doc(&f.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }

        let sig = self.function_sig_to_doc(f);
        let sig_multiline = signature_wraps(&sig, indent_cols);
        parts.push(sig);

        if sig_multiline && f.body.is_some() {
            parts.push(hardline());
        }

        if let Some(body) = &f.body {
            parts.push(self.body_to_doc(body, f.span.end.line));
            parts.push(hardline());
            parts.push(text("end"));
        }
        concat(parts)
    }

    /// Formats a function signature (visibility, name, type params, params,
    /// return type) with group/indent for line-breaking.
    fn function_sig_to_doc(&mut self, f: &Function) -> Doc {
        let mut prefix = String::from(visibility_prefix(f.visibility));
        prefix.push_str("fn ");
        prefix.push_str(&f.name);

        if !f.type_params.is_empty() {
            prefix.push('<');
            prefix.push_str(&util::format_type_params(&f.type_params));
            prefix.push('>');
        }

        let params_doc: Vec<Doc> = f.params.iter().map(|p| self.param_to_doc(p)).collect();
        let has_return = f.return_type.as_ref().is_some_and(|rt| !is_unit_type(rt));

        let return_doc = if has_return {
            Some(concat(vec![
                text("-> "),
                type_expr_to_doc(f.return_type.as_ref().unwrap()),
            ]))
        } else {
            None
        };

        let params_inline = if params_doc.is_empty() {
            text("")
        } else {
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
        };

        match return_doc {
            Some(ret) => group(concat(vec![
                text(prefix),
                params_inline,
                group(indent(2, concat(vec![line(), ret]))),
            ])),
            None => concat(vec![text(prefix), params_inline]),
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

        let mut body = Vec::new();
        for (i, method) in p.methods.iter().enumerate() {
            if i > 0 {
                body.push(hardline());
            }
            body.push(hardline());
            body.push(self.protocol_method_to_doc(method));
        }
        self.push_trailing_body_comments(&mut body, p.span.end.line);
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

    /// Formats a protocol method (signature only, or with default body).
    fn protocol_method_to_doc(&mut self, m: &ProtocolMethod) -> Doc {
        let mut parts = Vec::new();
        let (comment_docs, _) = self.comments.drain_before(m.span.start.line);
        parts.extend(comment_docs);
        if let Some(doc) = annotations_to_doc(&m.annotations) {
            parts.push(doc);
            parts.push(hardline());
        }

        let mut prefix = String::from("fn ");
        prefix.push_str(&m.name);
        if !m.type_params.is_empty() {
            prefix.push('<');
            prefix.push_str(&util::format_type_params(&m.type_params));
            prefix.push('>');
        }

        let params_doc: Vec<Doc> = m.params.iter().map(|p| self.param_to_doc(p)).collect();
        let has_return = m.return_type.as_ref().is_some_and(|rt| !is_unit_type(rt));

        let return_doc = if has_return {
            Some(concat(vec![
                text("-> "),
                type_expr_to_doc(m.return_type.as_ref().unwrap()),
            ]))
        } else {
            None
        };

        if params_doc.is_empty() {
            parts.push(text(prefix));
        } else {
            parts.push(text(format!("{prefix}(")));
            parts.push(intersperse(params_doc, text(", ")));
            parts.push(text(")"));
        }

        if let Some(ret) = return_doc {
            parts.push(text(" "));
            parts.push(ret);
        }

        if let Some(body) = &m.body {
            parts.push(self.body_to_doc(body, m.span.end.line));
            parts.push(hardline());
            parts.push(text("end"));
        }

        concat(parts)
    }

    /// Formats an `impl Protocol for Type` block.
    fn impl_to_doc(&mut self, block: &ImplBlock) -> Doc {
        concat(vec![
            text("impl "),
            type_expr_to_doc(&block.trait_expr),
            text(" for "),
            type_expr_to_doc(&block.target),
            self.impl_member_body_to_doc(&block.members, block.span.end.line),
        ])
    }

    /// Formats an `extend Type` block.
    fn extend_to_doc(&mut self, block: &ExtendBlock) -> Doc {
        concat(vec![
            text("extend "),
            type_expr_to_doc(&block.target),
            self.impl_member_body_to_doc(&block.members, block.span.end.line),
        ])
    }

    /// Shared body for `impl` and `extend`: indented members + `end`.
    fn impl_member_body_to_doc(&mut self, members: &[ImplMember], end_line: u32) -> Doc {
        let mut body = Vec::new();
        for (i, member) in members.iter().enumerate() {
            if i > 0 {
                body.push(hardline());
            }
            body.push(hardline());
            body.push(self.impl_member_to_doc(member));
        }
        self.push_trailing_body_comments(&mut body, end_line);
        concat(vec![indent(2, concat(body)), hardline(), text("end")])
    }

    /// Drains comments sitting between the last member of a type body and
    /// its `end`, appending them to `body` so they stay inside the block.
    fn push_trailing_body_comments(&mut self, body: &mut Vec<Doc>, end_line: u32) {
        let (mut trailing, _) = self.comments.drain_before(end_line);
        if !trailing.is_empty() {
            // Drop the final hardline. The block's own newline before `end`
            // provides it.
            trailing.pop();
            body.push(hardline());
            body.append(&mut trailing);
        }
    }

    /// Formats a member inside an `impl` block (function or type alias).
    fn impl_member_to_doc(&mut self, member: &ImplMember) -> Doc {
        match member {
            ImplMember::Function(f) => self.function_to_doc(f, 2),
            ImplMember::TypeAlias(ta) => {
                let (comment_docs, _) = self.comments.drain_before(ta.span.start.line);
                let mut parts = comment_docs;
                parts.push(text(format!("type {} = ", ta.name)));
                parts.push(type_expr_to_doc(&ta.type_expr));
                concat(parts)
            }
        }
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

    /// Formats a single statement.
    pub(super) fn statement_to_doc(&mut self, stmt: &Statement) -> Doc {
        match stmt {
            Statement::Expr(expr) => self.expr_to_doc(expr),
            Statement::Assignment {
                target,
                type_annotation,
                value,
                ..
            } => {
                let target_doc = text(target.segments.join("."));
                let lhs = if let Some(te) = type_annotation {
                    concat(vec![target_doc, text(": "), type_expr_to_doc(te)])
                } else {
                    target_doc
                };
                let value_doc = self.expr_to_doc(value);
                if is_inline_closure(value) {
                    // Stay inline when the closure fits, breaking after `=`
                    // (soft line) only when it overflows the line width.
                    group(concat(vec![
                        lhs,
                        text(" ="),
                        indent(2, concat(vec![line(), value_doc])),
                    ]))
                } else if expr_contains_block(value) {
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
                let value_doc = self.expr_to_doc(value);
                if expr_contains_block(value) {
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
            Statement::Return { value, .. } => match value {
                Some(v) => concat(vec![text("return "), self.expr_to_doc(v)]),
                None => text("return"),
            },
            Statement::Break { .. } => text("break"),
        }
    }

    /// Renders a list of statements, draining interleaved comments.
    ///
    /// `block_end` is the line of the closing `end` keyword so we know
    /// the upper bound for comments belonging to this block.
    pub(super) fn statements_to_doc(&mut self, stmts: &[Statement], block_end: u32) -> Doc {
        let mut parts = Vec::new();
        let mut prev_end: u32 = 0;

        for (i, stmt) in stmts.iter().enumerate() {
            let stmt_line = stmt_start_line(stmt);
            let next_line = self.comments.peek_before(stmt_line).unwrap_or(stmt_line);
            let (comment_docs, last_comment_line) = self.comments.drain_before(stmt_line);

            if i > 0 {
                parts.push(hardline());
                let source_has_blank = next_line > prev_end + 1;
                if source_has_blank {
                    parts.push(hardline());
                } else {
                    let prev_is_block = stmt_is_block(&stmts[i - 1]);
                    let curr_is_block = stmt_is_block(stmt);
                    if prev_is_block || curr_is_block {
                        parts.push(hardline());
                    }
                }
            }

            for c in comment_docs {
                parts.push(c);
            }

            if let Some(lcl) = last_comment_line
                && stmt_line > lcl + 1
            {
                parts.push(hardline());
            }

            parts.push(self.statement_to_doc(stmt));
            let end_line = stmt_end_line(stmt);
            if let Some(tc) = self.comments.drain_trailing(end_line) {
                parts.push(tc);
            }
            prev_end = end_line;
        }

        let next_trailing_line = self.comments.peek_before(block_end);
        let (mut trailing, _) = self.comments.drain_before(block_end);
        if !trailing.is_empty() {
            // Drop the final hardline. The block's own newline before `end` provides it.
            trailing.pop();
            parts.push(hardline());
            if let Some(cl) = next_trailing_line
                && cl > prev_end + 1
            {
                parts.push(hardline());
            }
            for c in trailing {
                parts.push(c);
            }
        }

        concat(parts)
    }

    /// Formats an indented body block (the statements between a keyword and `end`).
    pub(super) fn body_to_doc(&mut self, stmts: &[Statement], block_end: u32) -> Doc {
        if stmts.is_empty() {
            let (trailing, _) = self.comments.drain_before(block_end);
            if trailing.is_empty() {
                nil()
            } else {
                let mut parts = Vec::new();
                for c in trailing {
                    parts.push(hardline());
                    parts.push(c);
                }
                indent(2, concat(parts))
            }
        } else {
            indent(
                2,
                concat(vec![hardline(), self.statements_to_doc(stmts, block_end)]),
            )
        }
    }
}
