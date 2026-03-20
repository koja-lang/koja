//! Pretty-printer: converts a parsed Expo AST back into formatted source code.
//!
//! The entry point is [`module_to_doc`], which produces a [`Doc`] document tree
//! that the renderer in [`crate::doc`] lays out to a target line width.
//!
//! Internally the printer is split into submodules:
//! - [`comments`]: source comment tracking and re-attachment
//! - [`expr`]: expression and match/cond/receive arm formatting
//! - [`util`]: stateless helpers for types, patterns, spans, etc.

mod comments;
mod expr;
mod util;

use crate::doc::*;
use expo_ast::ast::*;

use comments::CommentCursor;
use util::*;

/// Converts a parsed module into a `Doc` tree ready for rendering.
pub fn module_to_doc(module: &Module) -> Doc {
    let mut p = Printer::new(&module.comments);
    p.print_module(module)
}

/// Holds the comment cursor used during formatting to re-attach comments
/// at their original source positions.
pub(super) struct Printer<'a> {
    pub(super) comments: CommentCursor<'a>,
}

impl<'a> Printer<'a> {
    /// Creates a new printer with a comment cursor over the module's comments.
    fn new(comments: &'a [Comment]) -> Self {
        Self {
            comments: CommentCursor::new(comments),
        }
    }

    /// Formats an entire module: moduledoc, sorted imports, then items with
    /// interleaved comments.
    fn print_module(&mut self, module: &Module) -> Doc {
        let mut parts: Vec<Doc> = Vec::new();
        let mut emitted = false;
        let mut moduledoc_emitted = false;
        let mut after_moduledoc = false;

        let moduledoc_line = module.moduledoc.as_ref().map(|md| md.span.start.line);

        let mut i = 0;

        while i < module.items.len() {
            let next_item_line = match &module.items[i] {
                Item::Import(imp) => imp.span.start.line,
                other => item_span(other).start.line,
            };

            if !moduledoc_emitted
                && let Some(md) = &module.moduledoc
                && moduledoc_line.is_some_and(|ml| ml < next_item_line)
            {
                let (comment_docs, _) = self.comments.drain_before(md.span.start.line);
                for c in comment_docs {
                    parts.push(c);
                }
                if emitted {
                    parts.push(hardline());
                }
                parts.push(annotation_to_doc(md));
                parts.push(hardline());
                emitted = true;
                moduledoc_emitted = true;
                after_moduledoc = true;
            }

            if matches!(&module.items[i], Item::Import(_) | Item::Constant(_)) {
                let is_import = matches!(&module.items[i], Item::Import(_));
                let run_start = i;

                if emitted && (!is_import || run_start > 0 || after_moduledoc) {
                    parts.push(hardline());
                }

                if is_import {
                    let mut imports: Vec<&Import> = Vec::new();
                    while i < module.items.len() {
                        if let Item::Import(imp) = &module.items[i] {
                            imports.push(imp);
                            i += 1;
                        } else {
                            break;
                        }
                    }

                    let block_end = imports.last().unwrap().span.end.line + 1;
                    let _ = self.comments.drain_before(block_end);

                    imports.sort_by_key(|imp| import_sort_key(imp));

                    for imp in &imports {
                        parts.push(import_to_doc(imp));
                        parts.push(hardline());
                    }
                } else {
                    while i < module.items.len() && matches!(&module.items[i], Item::Constant(_)) {
                        let item = &module.items[i];
                        let span = item_span(item);
                        let (comment_docs, _) = self.comments.drain_before(span.start.line);
                        for c in comment_docs {
                            parts.push(c);
                        }
                        parts.push(self.item_to_doc(item));
                        parts.push(hardline());
                        i += 1;
                    }
                }

                emitted = true;
                after_moduledoc = false;
            } else {
                let item = &module.items[i];
                let span = item_span(item);
                let (comment_docs, _) = self.comments.drain_before(span.start.line);
                for c in comment_docs {
                    parts.push(c);
                }
                if emitted {
                    parts.push(hardline());
                }
                parts.push(self.item_to_doc(item));
                parts.push(hardline());
                emitted = true;
                after_moduledoc = false;
                i += 1;
            }
        }

        if !moduledoc_emitted && let Some(md) = &module.moduledoc {
            if emitted {
                parts.push(hardline());
            }
            parts.push(annotation_to_doc(md));
            parts.push(hardline());
        }

        let trailing = self.comments.drain_rest();
        for c in trailing {
            parts.push(c);
        }

        concat(parts)
    }

    /// Dispatches a top-level item to its specific formatter.
    fn item_to_doc(&mut self, item: &Item) -> Doc {
        match item {
            Item::Import(i) => import_to_doc(i),
            Item::Struct(s) => self.struct_to_doc(s),
            Item::Enum(e) => self.enum_to_doc(e),
            Item::Function(f) => self.function_to_doc(f),
            Item::Impl(i) => self.impl_to_doc(i),
            Item::Protocol(p) => self.protocol_to_doc(p),
            Item::Constant(c) => self.constant_to_doc(c),
            Item::Shared(s) => shared_to_doc(s),
            Item::TypeAlias(t) => type_alias_to_doc(t),
        }
    }

    /// Formats a `struct` declaration with its fields and trailing comments.
    fn struct_to_doc(&mut self, s: &StructDecl) -> Doc {
        let mut parts = Vec::new();
        if let Some(ann) = &s.annotation {
            parts.push(annotation_to_doc(ann));
            parts.push(hardline());
        }
        let mut header = format!("struct {}", s.name);
        if !s.type_params.is_empty() {
            header.push('<');
            header.push_str(&s.type_params.join(", "));
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
        let (mut trailing, _) = self.comments.drain_before(s.span.end.line);
        if !trailing.is_empty() {
            trailing.pop();
            body.push(hardline());
            for c in trailing {
                body.push(c);
            }
        }
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
        if let Some(ann) = &e.annotation {
            parts.push(annotation_to_doc(ann));
            parts.push(hardline());
        }
        let mut header = format!("enum {}", e.name);
        if !e.type_params.is_empty() {
            header.push('<');
            header.push_str(&e.type_params.join(", "));
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
        let (mut trailing, _) = self.comments.drain_before(e.span.end.line);
        if !trailing.is_empty() {
            trailing.pop();
            body.push(hardline());
            for c in trailing {
                body.push(c);
            }
        }
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
    fn function_to_doc(&mut self, f: &Function) -> Doc {
        let mut parts = Vec::new();

        if let Some(ann) = &f.annotation {
            parts.push(annotation_to_doc(ann));
            parts.push(hardline());
        }

        let sig_multiline = sig_will_break(f);
        parts.push(self.function_sig_to_doc(f));

        if sig_multiline {
            parts.push(hardline());
        }

        parts.push(self.body_to_doc(&f.body, f.span.end.line));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

    /// Formats a function signature (visibility, name, type params, params,
    /// return type) with group/indent for line-breaking.
    fn function_sig_to_doc(&mut self, f: &Function) -> Doc {
        let mut prefix = String::new();
        if f.visibility == Visibility::Private {
            prefix.push_str("priv ");
        }
        prefix.push_str("fn ");
        prefix.push_str(&f.name);

        if !f.type_params.is_empty() {
            prefix.push('<');
            prefix.push_str(&f.type_params.join(", "));
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

    /// Formats a function parameter (`self`, `move self`, or `name: Type`).
    fn param_to_doc(&mut self, p: &Param) -> Doc {
        match p {
            Param::Self_ { mode, .. } => {
                if *mode == PassMode::Move {
                    text("move self")
                } else {
                    text("self")
                }
            }
            Param::Regular {
                mode,
                name,
                type_expr,
                default,
                ..
            } => {
                let mut parts = Vec::new();
                if *mode == PassMode::Move {
                    parts.push(text("move "));
                }
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
        if let Some(ann) = &p.annotation {
            parts.push(annotation_to_doc(ann));
            parts.push(hardline());
        }
        let mut header = format!("protocol {}", p.name);
        if !p.type_params.is_empty() {
            header.push('<');
            header.push_str(&p.type_params.join(", "));
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
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

    /// Formats a protocol method signature (no body).
    fn protocol_method_to_doc(&mut self, m: &ProtocolMethod) -> Doc {
        let mut parts = Vec::new();
        if let Some(ann) = &m.annotation {
            parts.push(annotation_to_doc(ann));
            parts.push(hardline());
        }

        let mut prefix = String::from("fn ");
        prefix.push_str(&m.name);
        if !m.type_params.is_empty() {
            prefix.push('<');
            prefix.push_str(&m.type_params.join(", "));
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

        concat(parts)
    }

    /// Formats an `impl` block (with optional protocol conformance).
    fn impl_to_doc(&mut self, block: &ImplBlock) -> Doc {
        let mut parts = Vec::new();
        parts.push(text("impl "));
        if let Some(trait_expr) = &block.trait_expr {
            parts.push(type_expr_to_doc(trait_expr));
            parts.push(text(" for "));
        }
        parts.push(type_expr_to_doc(&block.target));

        let mut body = Vec::new();
        for (i, member) in block.members.iter().enumerate() {
            if i > 0 {
                body.push(hardline());
            }
            body.push(hardline());
            body.push(self.impl_member_to_doc(member));
        }
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

    /// Formats a member inside an `impl` block (function or type alias).
    fn impl_member_to_doc(&mut self, member: &ImplMember) -> Doc {
        match member {
            ImplMember::Function(f) => self.function_to_doc(f),
            ImplMember::TypeAlias(ta) => concat(vec![
                text(format!("type {} = ", ta.name)),
                type_expr_to_doc(&ta.type_expr),
            ]),
        }
    }

    /// Formats a `const` declaration.
    fn constant_to_doc(&mut self, c: &Constant) -> Doc {
        let mut parts = Vec::new();
        if let Some(ann) = &c.annotation {
            parts.push(annotation_to_doc(ann));
            parts.push(hardline());
        }
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
                let target_doc = match target {
                    AssignTarget::LValue(lv) => text(lv.segments.join(".")),
                    AssignTarget::Pattern(pat) => pattern_to_doc(pat),
                };
                let lhs = if let Some(te) = type_annotation {
                    concat(vec![target_doc, text(": "), type_expr_to_doc(te)])
                } else {
                    target_doc
                };
                let value_doc = self.expr_to_doc(value);
                if expr_contains_block(value) {
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
                    let prev_is_block = is_block_assignment(&stmts[i - 1]);
                    let curr_is_block = is_block_assignment(stmt);
                    if prev_is_block != curr_is_block {
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
            trailing.pop(); // drop the final hardline; the block's own newline before `end` provides it
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
