//! Expression and arm printers. `expr_to_doc` dispatches on the
//! expression kind, and every non-trivial kind has its own method below,
//! followed by the shared list, string, arm, and chain layouts.

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;

use super::Printer;
use super::attach::Slot;
use super::comments::{leading_docs, trailing_doc};
use super::seq::{Group, SeqEntry, element_lines, field_lines, push_stragglers};
use super::util::*;

impl Printer {
    pub(super) fn expr_to_doc(&mut self, expr: &Expr) -> Doc {
        match &expr.kind {
            ExprKind::Assert {
                condition, message, ..
            } => {
                let mut parts = vec![text("assert "), self.expr_to_doc(condition)];
                if let Some(message) = message {
                    parts.push(text(", "));
                    parts.push(self.expr_to_doc(message));
                }
                concat(parts)
            }
            ExprKind::Binary { op, .. } => self.binary_to_doc(expr, op),
            ExprKind::BinaryLiteral { segments } => {
                if segments.is_empty() {
                    text("<<>>")
                } else {
                    let entries = self.seq_entries(
                        segments,
                        |seg| seg.span,
                        |p, seg| p.binary_segment_to_doc(seg),
                    );
                    self.element_list_to_doc("<<", ">>", entries, expr.span)
                }
            }
            ExprKind::Call { callee, args, .. } => concat(vec![
                self.expr_to_doc(callee),
                self.call_args_to_doc(args, expr.span),
            ]),
            ExprKind::Closure {
                params,
                return_type,
                body,
            } => self.closure_to_doc(expr, params, return_type.as_ref(), body),
            ExprKind::Cond { arms, else_body } => {
                self.cond_to_doc(arms, else_body.as_deref(), expr.span)
            }
            ExprKind::EnumConstruction {
                type_path,
                variant,
                data,
            } => {
                let prefix = enum_prefix(type_path, variant);
                match data {
                    EnumConstructionData::Unit => text(prefix),
                    EnumConstructionData::Tuple(exprs) => {
                        let elems: Vec<Doc> = exprs.iter().map(|e| self.expr_to_doc(e)).collect();
                        concat(vec![
                            text(prefix),
                            text("("),
                            intersperse(elems, text(", ")),
                            text(")"),
                        ])
                    }
                    EnumConstructionData::Struct(fields) => {
                        self.construction_to_doc(text(prefix), fields, expr.span)
                    }
                }
            }
            ExprKind::Fail { value } => concat(vec![text("fail "), self.expr_to_doc(value)]),
            ExprKind::FieldAccess { receiver, field } => concat(vec![
                self.expr_to_doc(receiver),
                text("."),
                text(&field.text),
            ]),
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                let mut header = vec![
                    text("for "),
                    self.pattern_to_doc(pattern),
                    text(" in "),
                    self.expr_to_doc(iterable),
                ];
                self.push_header_trailing(&mut header, expr.span);
                concat(vec![concat(header), self.body_end_to_doc(body, expr.span)])
            }
            ExprKind::Group { expr: inner } => {
                concat(vec![text("("), self.expr_to_doc(inner), text(")")])
            }
            ExprKind::Ident { name, .. } => text(name.clone()),
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => self.if_to_doc(condition, then_body, else_body.as_deref(), expr.span),
            ExprKind::List { elements } => {
                if elements.is_empty() {
                    text("[]")
                } else {
                    let entries = self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e));
                    self.element_list_to_doc("[", "]", entries, expr.span)
                }
            }
            ExprKind::Literal { value } => literal_to_doc(value),
            ExprKind::Loop { body } => {
                let mut header = vec![text("loop")];
                self.push_header_trailing(&mut header, expr.span);
                concat(vec![concat(header), self.body_end_to_doc(body, expr.span)])
            }
            ExprKind::Map { entries } => {
                if entries.is_empty() {
                    text("[:]")
                } else {
                    let entry_docs = self.map_entries(entries);
                    self.element_list_to_doc("[", "]", entry_docs, expr.span)
                }
            }
            ExprKind::Match { subject, arms } => self.match_to_doc(subject, arms, expr.span),
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => self.method_call_to_doc(expr, receiver, method, args),
            ExprKind::NamedFunctionReference { path, arity, .. } => {
                text(format!("&{}/{}", path_text(path), arity))
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => self.receive_to_doc(arms, after_timeout.as_deref(), after_body, expr.span),
            // Breaks into the two-line idiom with `rescue` leading
            // the continuation line, mirroring how it parses.
            ExprKind::Rescue {
                subject,
                binder,
                handler,
                ..
            } => {
                let binder_text = binder.clone().unwrap_or_else(|| String::from("_"));
                group(concat(vec![
                    self.expr_to_doc(subject),
                    indent(
                        2,
                        concat(vec![
                            line(),
                            text(format!("rescue {binder_text} -> ")),
                            self.expr_to_doc(handler),
                        ]),
                    ),
                ]))
            }
            ExprKind::Self_ { .. } => text("self"),
            ExprKind::ShortClosure { params, body } => {
                let params_doc = if params.is_empty() {
                    text("()")
                } else {
                    intersperse(
                        params.iter().map(closure_param_to_doc).collect(),
                        text(", "),
                    )
                };
                group(concat(vec![
                    params_doc,
                    text(" -> "),
                    self.expr_to_doc(body),
                ]))
            }
            ExprKind::Spawn { expr: inner } => {
                concat(vec![text("spawn "), self.expr_to_doc(inner)])
            }
            ExprKind::String { parts, multiline } => self.string_to_doc(parts, *multiline),
            ExprKind::StructConstruction { type_path, fields } => {
                let path_str = path_text(type_path);
                if fields.is_empty() {
                    text(format!("{}{{}}", path_str))
                } else {
                    self.construction_to_doc(text(path_str), fields, expr.span)
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                let cond_doc = self.expr_to_doc(condition);
                let then_doc = self.expr_to_doc(then_expr);
                let else_doc = self.expr_to_doc(else_expr);
                group(concat(vec![
                    cond_doc,
                    indent(
                        2,
                        concat(vec![
                            line(),
                            text("? "),
                            then_doc,
                            line(),
                            text(": "),
                            else_doc,
                        ]),
                    ),
                ]))
            }
            ExprKind::Try { expr: inner } => concat(vec![text("try "), self.expr_to_doc(inner)]),
            ExprKind::Tuple { elements } => {
                let entries = self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e));
                self.element_list_to_doc("(", ")", entries, expr.span)
            }
            ExprKind::Unary { op, operand } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
                };
                concat(vec![text(op_str), self.expr_to_doc(operand)])
            }
            ExprKind::While { condition, body } => concat(vec![
                self.condition_header_to_doc("while ", condition, expr.span),
                self.body_end_to_doc(body, expr.span),
            ]),
        }
    }

    /// A same-operator chain packs densely, fill-style, indented two
    /// past where it began. `and` / `or` lead each continuation line
    /// with the operator. Every other operator trails the line, since a
    /// leading one would not parse.
    fn binary_to_doc(&mut self, expr: &Expr, op: &BinOp) -> Doc {
        let op_str = binop_str(op);
        let leading_op = matches!(op, BinOp::And | BinOp::Or);
        let operands: Vec<Doc> = binop_operands(expr, op)
            .into_iter()
            .map(|e| self.expr_to_doc(e))
            .collect();
        let last = operands.len() - 1;
        let items: Vec<Doc> = operands
            .into_iter()
            .enumerate()
            .map(|(i, doc)| {
                if leading_op && i > 0 {
                    concat(vec![text(op_str), text(" "), doc])
                } else if !leading_op && i < last {
                    concat(vec![doc, text(" "), text(op_str)])
                } else {
                    doc
                }
            })
            .collect();
        indent(2, fill(items))
    }

    /// A depth-1 call renders here. A receiver that is itself a call
    /// makes a chain, which has its own layout.
    fn method_call_to_doc(
        &mut self,
        expr: &Expr,
        receiver: &Expr,
        method: &Name,
        args: &[Arg],
    ) -> Doc {
        if matches!(receiver.kind, ExprKind::MethodCall { .. }) {
            return self.method_chain_to_doc(expr, LinkIndent::Hang);
        }
        let call = |p: &mut Self| {
            concat(vec![
                text("."),
                text(&method.text),
                p.call_args_to_doc(args, expr.span),
            ])
        };
        if let Some(receiver_body) = self.collection_literal_body(receiver) {
            // The shared group makes the literal's brackets split first,
            // and the call hugs the closing bracket.
            let call_doc = call(self);
            return group(concat(vec![receiver_body, call_doc]));
        }
        let receiver_doc = self.expr_to_doc(receiver);
        let call_doc = call(self);
        concat(vec![receiver_doc, call_doc])
    }

    fn if_to_doc(
        &mut self,
        condition: &Expr,
        then_body: &[Statement],
        else_body: Option<&[Statement]>,
        owner: Span,
    ) -> Doc {
        let mut parts = vec![self.condition_header_to_doc("if ", condition, owner)];
        let Some(else_body) = else_body else {
            parts.push(self.body_end_to_doc(then_body, owner));
            return concat(parts);
        };
        parts.push(self.body_to_doc(then_body, Vec::new()));
        parts.push(hardline());
        let before_else = self.comments.take(owner, Slot::BeforeElse);
        let (docs, _) = leading_docs(&before_else);
        parts.extend(docs);
        parts.push(text("else"));
        let else_trailing = self.comments.take(owner, Slot::ElseTrailing);
        if let Some(tc) = trailing_doc(&else_trailing) {
            parts.push(tc);
        }
        parts.push(self.body_end_to_doc(else_body, owner));
        concat(parts)
    }

    fn match_to_doc(&mut self, subject: &Expr, arms: &[MatchArm], owner: Span) -> Doc {
        let force = self.arms_force_block(arms);
        let mut header = vec![text("match "), self.expr_to_doc(subject)];
        self.push_header_trailing(&mut header, owner);
        let rendered = self.render_arms(arms, |a| a.span, |p, a| p.match_arm_to_doc(a, force));
        let end_dangling = self.comments.take(owner, Slot::Dangling);
        arms_block(concat(header), rendered, force, vec![], end_dangling)
    }

    fn cond_to_doc(
        &mut self,
        arms: &[CondArm],
        else_body: Option<&[Statement]>,
        owner: Span,
    ) -> Doc {
        let else_force = else_body.is_some_and(|body| {
            arm_is_multiline(body)
                || arm_body_overflows(0, body)
                || self.arm_comments_require_block(owner, body)
                || self.comments.has(owner, Slot::BeforeElse)
        });
        let force = else_force
            || arms.iter().any(|a| {
                arm_is_multiline(&a.body)
                    || expr_or_is_multiline(&a.condition)
                    || arm_body_overflows(expr_text_len(&a.condition), &a.body)
                    || self.arm_comments_require_block(a.span, &a.body)
            });
        let mut header = vec![text("cond")];
        self.push_header_trailing(&mut header, owner);
        let mut rendered = self.render_arms(arms, |a| a.span, |p, a| p.cond_arm_to_doc(a, force));
        if let Some(body) = else_body {
            let leading = self.comments.take(owner, Slot::BeforeElse);
            let head_trailing = self.comments.take(owner, Slot::ElseTrailing);
            let dangling = self.comments.take(owner, Slot::Dangling);
            let doc = self.arm_body_to_doc(
                text("else ->"),
                head_trailing,
                body,
                force,
                dangling,
                Vec::new(),
            );
            rendered.push(with_leading(&leading, doc));
        }
        let end_dangling = self.comments.take(owner, Slot::Dangling);
        arms_block(concat(header), rendered, force, vec![], end_dangling)
    }

    fn receive_to_doc(
        &mut self,
        arms: &[MatchArm],
        after_timeout: Option<&Expr>,
        after_body: &[Statement],
        owner: Span,
    ) -> Doc {
        let force =
            self.arms_force_block(arms) || after_timeout.is_some() || arm_is_multiline(after_body);
        let mut header = vec![text("receive")];
        self.push_header_trailing(&mut header, owner);
        let rendered = self.render_arms(arms, |a| a.span, |p, a| p.match_arm_to_doc(a, force));
        let mut suffix = Vec::new();
        if let Some(timeout) = after_timeout {
            let before_after = self.comments.take(owner, Slot::BeforeAfter);
            suffix.push(hardline());
            if !before_after.is_empty() {
                suffix.push(hardline());
            }
            let (docs, _) = leading_docs(&before_after);
            suffix.extend(docs);
            suffix.push(text("after "));
            suffix.push(self.expr_to_doc(timeout));
            let after_trailing = self.comments.take(timeout.span, Slot::HeaderTrailing);
            if let Some(tc) = trailing_doc(&after_trailing) {
                suffix.push(tc);
            }
            let dangling = self.comments.take(owner, Slot::Dangling);
            suffix.push(self.body_to_doc(after_body, dangling));
        }
        let end_dangling = self.comments.take(owner, Slot::Dangling);
        arms_block(concat(header), rendered, force, suffix, end_dangling)
    }

    fn closure_to_doc(
        &mut self,
        expr: &Expr,
        params: &[ClosureParam],
        return_type: Option<&TypeExpr>,
        body: &[Statement],
    ) -> Doc {
        let params_doc: Vec<Doc> = params.iter().map(closure_param_to_doc).collect();
        let mut sig_parts = vec![text("fn ("), intersperse(params_doc, text(", ")), text(")")];
        if let Some(rt) = return_type {
            sig_parts.push(text(" -> "));
            sig_parts.push(type_expr_to_doc(rt));
        }
        let sig = concat(sig_parts);
        if self.closure_renders_inline(expr) {
            // No interior comments (the gate guarantees it), so the
            // single statement prints directly. An end-line trailing
            // comment stays with the enclosing context and glues after
            // `end`.
            let body_doc = self.statement_to_doc(&body[0]);
            return group(concat(vec![
                sig,
                indent(2, concat(vec![line(), body_doc])),
                line(),
                text("end"),
            ]));
        }
        let mut parts = vec![sig];
        self.push_header_trailing(&mut parts, expr.span);
        parts.push(self.body_end_to_doc(body, expr.span));
        concat(parts)
    }

    /// Whether a closure takes the collapsed single-line layout. A comment
    /// inside the closure span rules it out (inlined, the comment would
    /// swallow `end`).
    pub(super) fn closure_renders_inline(&self, expr: &Expr) -> bool {
        is_inline_closure(expr) && !self.comments.any_within(expr.span)
    }

    /// The indented body and closing `end` of a block expression, with
    /// the block's dangling comments before `end`.
    fn body_end_to_doc(&mut self, body: &[Statement], owner: Span) -> Doc {
        let dangling = self.comments.take(owner, Slot::Dangling);
        concat(vec![
            self.body_to_doc(body, dangling),
            hardline(),
            text("end"),
        ])
    }

    /// Formats an `if` / `unless` / `while` header. Like wrapped
    /// function signatures, a wrapped condition indents two (the
    /// expression doc hangs its own continuations) and a blank line
    /// separates it from the body. `owner` keys the header's trailing
    /// comment.
    fn condition_header_to_doc(&mut self, keyword: &str, condition: &Expr, owner: Span) -> Doc {
        let mut parts = vec![text(keyword), self.expr_to_doc(condition)];
        self.push_header_trailing(&mut parts, owner);
        parts.push(if_break(nil(), hardline()));
        group(concat(parts))
    }

    /// Formats a parenthesized argument list for a call or method call.
    /// `owner` is the call expression's span, which keys the stragglers
    /// before the closing paren.
    pub(super) fn call_args_to_doc(&mut self, args: &[Arg], owner: Span) -> Doc {
        if args.is_empty() {
            let stragglers = self.comments.take(owner, Slot::Stragglers);
            if stragglers.is_empty() {
                return text("()");
            }
            return broken_list("(", ")", field_lines(Vec::new(), stragglers));
        }
        if let [arg] = args
            && arg.name.is_none()
            && (is_closure_arg(&arg.value) || is_heredoc(&arg.value))
            && !self.comments.has(arg.span, Slot::Leading)
            && !self.comments.has(arg.span, Slot::Trailing)
            && !self.comments.has(owner, Slot::Stragglers)
        {
            // Hug a sole trailing closure or heredoc instead of exploding
            // the arg list. A comment anchored to the argument rules the
            // hug out and takes the broken layout below.
            return concat(vec![text("("), self.arg_to_doc(arg), text(")")]);
        }
        let entries = self.seq_entries(args, |a| a.span, |p, a| p.arg_to_doc(a));
        if self.entries_comment_free(&entries, owner) {
            let arg_docs: Vec<Doc> = entries.into_iter().map(|e| e.doc).collect();
            return delimited_list("(", ")", arg_docs);
        }
        let stragglers = self.comments.take(owner, Slot::Stragglers);
        broken_list("(", ")", field_lines(entries, stragglers))
    }

    fn arg_to_doc(&mut self, arg: &Arg) -> Doc {
        match &arg.name {
            Some(name) => concat(vec![
                text(name.clone()),
                text(": "),
                self.expr_to_doc(&arg.value),
            ]),
            None => self.expr_to_doc(&arg.value),
        }
    }

    fn map_entries(&mut self, entries: &[(Expr, Expr)]) -> Vec<SeqEntry> {
        self.seq_entries(
            entries,
            |(k, v)| map_entry_span(k, v),
            |p, (k, v)| concat(vec![p.expr_to_doc(k), text(": "), p.expr_to_doc(v)]),
        )
    }

    /// Formats an element list (list, tuple, map, or binary literal) with
    /// packed layout when comment-free and comment-aware packing otherwise.
    pub(super) fn element_list_to_doc(
        &mut self,
        open: &str,
        close: &str,
        entries: Vec<SeqEntry>,
        owner: Span,
    ) -> Doc {
        let comment_free = self.entries_comment_free(&entries, owner);
        let body = self.element_list_body(open, close, entries, owner, comment_free);
        if comment_free { group(body) } else { body }
    }

    /// The element-list layout without its enclosing group, so a caller
    /// can bind the break decision to a larger group. The commented form
    /// is already hard-broken and needs no group.
    fn element_list_body(
        &mut self,
        open: &str,
        close: &str,
        entries: Vec<SeqEntry>,
        owner: Span,
        comment_free: bool,
    ) -> Doc {
        if comment_free {
            let items = entries.into_iter().map(|e| e.doc).collect();
            return bracket_list_body(open, close, items);
        }
        let stragglers = self.comments.take(owner, Slot::Stragglers);
        broken_list(open, close, element_lines(entries, stragglers))
    }

    /// Builds the bracket layout for a non-empty collection literal
    /// without its own group. `None` for anything else. A method call on
    /// such a receiver binds the literal's break to the call's group, so
    /// the literal splits its brackets before the argument list does.
    fn collection_literal_body(&mut self, expr: &Expr) -> Option<Doc> {
        let (open, close, entries) = match &expr.kind {
            ExprKind::List { elements } if !elements.is_empty() => (
                "[",
                "]",
                self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e)),
            ),
            ExprKind::Map { entries } if !entries.is_empty() => {
                ("[", "]", self.map_entries(entries))
            }
            ExprKind::Tuple { elements } => (
                "(",
                ")",
                self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e)),
            ),
            _ => return None,
        };
        let comment_free = self.entries_comment_free(&entries, expr.span);
        Some(self.element_list_body(open, close, entries, expr.span, comment_free))
    }

    /// Formats a `prefix{field, ...}` field list with struct-literal
    /// layout when comment-free and one field per line otherwise.
    pub(super) fn field_list_to_doc(
        &mut self,
        prefix: Doc,
        entries: Vec<SeqEntry>,
        owner: Span,
    ) -> Doc {
        if self.entries_comment_free(&entries, owner) {
            let docs = entries.into_iter().map(|e| e.doc).collect();
            return struct_body(prefix, docs);
        }
        let stragglers = self.comments.take(owner, Slot::Stragglers);
        concat(vec![
            prefix,
            broken_list("{", "}", field_lines(entries, stragglers)),
        ])
    }

    fn construction_to_doc(&mut self, prefix: Doc, fields: &[FieldInit], owner: Span) -> Doc {
        let entries = self.seq_entries(fields, |fi| fi.span, |p, fi| p.field_init_to_doc(fi));
        self.field_list_to_doc(prefix, entries, owner)
    }

    fn field_init_to_doc(&mut self, fi: &FieldInit) -> Doc {
        concat(vec![
            text(&fi.name.text),
            text(": "),
            self.expr_to_doc(&fi.value),
        ])
    }

    /// One `value::size unit signedness endianness` or `value: Type`
    /// segment of a binary literal or binary pattern.
    pub(super) fn binary_segment_to_doc(&mut self, seg: &BinarySegment) -> Doc {
        let mut parts = vec![self.expr_to_doc(&seg.value)];
        if let Some(size) = &seg.size {
            parts.push(text("::"));
            parts.push(self.expr_to_doc(size));
            if seg.unit == BinaryUnit::Byte {
                parts.push(text(" byte"));
            }
            if let Some(s) = &seg.signedness {
                parts.push(text(match s {
                    BinarySignedness::Signed => " signed",
                    BinarySignedness::Unsigned => " unsigned",
                }));
            }
            if let Some(e) = &seg.endianness {
                parts.push(text(match e {
                    BinaryEndianness::Big => " big",
                    BinaryEndianness::Little => " little",
                }));
            }
        } else if let Some(ta) = &seg.type_ann {
            parts.push(text(": "));
            parts.push(type_expr_to_doc(ta));
        }
        concat(parts)
    }

    /// Formats a string literal. Multiline strings render as a heredoc
    /// block unless the content cannot survive that form (see
    /// [`heredoc_representable`]), in which case they fall back to the
    /// single-line spelling with `\n` escapes.
    fn string_to_doc(&mut self, parts: &[StringPart], multiline: bool) -> Doc {
        if multiline && heredoc_representable(parts) {
            return self.heredoc_to_doc(parts);
        }
        let mut doc_parts = vec![text("\"")];
        for part in parts {
            match part {
                StringPart::Literal { value, .. } => {
                    doc_parts.push(text(escape_string_literal(value)));
                }
                StringPart::Interpolation { expr, format, .. } => {
                    doc_parts.push(self.interpolation_to_doc(expr, format.as_deref()));
                }
            }
        }
        doc_parts.push(text("\""));
        concat(doc_parts)
    }

    /// Formats a multiline string as a heredoc block: the delimiters and
    /// every content line sit at the ambient indent, each behind a
    /// hardline. The closing delimiter's column therefore equals the pad
    /// the hardlines injected, so the parser's closing-column dedent
    /// strips exactly what the printer added and the cooked value
    /// round-trips (including nested relative indentation).
    fn heredoc_to_doc(&mut self, parts: &[StringPart]) -> Doc {
        let mut doc_parts = vec![text("\"\"\""), hardline()];
        for part in parts {
            match part {
                StringPart::Literal { value, .. } => {
                    let escaped = escape_multiline_literal(value);
                    for (i, line) in escaped.split('\n').enumerate() {
                        if i > 0 {
                            doc_parts.push(hardline());
                        }
                        doc_parts.push(text(line.to_string()));
                    }
                }
                StringPart::Interpolation { expr, format, .. } => {
                    doc_parts.push(self.interpolation_to_doc(expr, format.as_deref()));
                }
            }
        }
        doc_parts.push(hardline());
        doc_parts.push(text("\"\"\""));
        concat(doc_parts)
    }

    /// Formats a `#{expr}` or `#{expr:spec}` interpolation segment. The
    /// expression renders flat because a string literal never breaks.
    fn interpolation_to_doc(&mut self, expr: &Expr, format: Option<&str>) -> Doc {
        let mut doc_parts = vec![text("#{"), flatten(self.expr_to_doc(expr))];
        if let Some(spec) = format {
            doc_parts.push(text(format!(":{spec}")));
        }
        doc_parts.push(text("}"));
        concat(doc_parts)
    }

    /// True when any `match` or `receive` arm needs the block layout: a
    /// multi-line body, a head too wide to share a line with its body,
    /// or a comment. The construct applies the same layout to every arm.
    fn arms_force_block(&self, arms: &[MatchArm]) -> bool {
        arms.iter().any(|a| {
            arm_is_multiline(&a.body)
                || pattern_is_multiline(&a.pattern)
                || arm_body_overflows(pattern_rendered_len(&a.pattern), &a.body)
                || self.arm_comments_require_block(a.span, &a.body)
        })
    }

    /// True when attached comments require an arm body to use block
    /// layout.
    fn arm_comments_require_block(&self, owner: Span, body: &[Statement]) -> bool {
        self.comments.has(owner, Slot::Dangling)
            || self.comments.has(owner, Slot::HeaderTrailing)
            || self.comments.has(owner, Slot::Leading)
            || body
                .first()
                .is_some_and(|statement| self.comments.has(stmt_span(statement), Slot::Leading))
    }

    /// Renders each arm with its leading comments above it.
    fn render_arms<A>(
        &mut self,
        arms: &[A],
        key_of: impl Fn(&A) -> Span,
        mut to_doc: impl FnMut(&mut Self, &A) -> Doc,
    ) -> Vec<Doc> {
        arms.iter()
            .map(|arm| {
                let leading = self.comments.take(key_of(arm), Slot::Leading);
                let doc = to_doc(self, arm);
                with_leading(&leading, doc)
            })
            .collect()
    }

    /// `pattern [when guard] -> body`.
    pub(super) fn match_arm_to_doc(&mut self, arm: &MatchArm, force_break: bool) -> Doc {
        let mut head = vec![self.pattern_to_doc(&arm.pattern)];
        if let Some(guard) = &arm.guard {
            head.push(text(" when "));
            head.push(self.expr_to_doc(guard));
        }
        head.push(text(" ->"));
        let head_trailing = self.comments.take(arm.span, Slot::HeaderTrailing);
        let dangling = self.comments.take(arm.span, Slot::Dangling);
        let trailing = self.comments.take(arm.span, Slot::Trailing);
        self.arm_body_to_doc(
            concat(head),
            head_trailing,
            &arm.body,
            force_break,
            dangling,
            trailing,
        )
    }

    /// `condition -> body`.
    pub(super) fn cond_arm_to_doc(&mut self, arm: &CondArm, force_break: bool) -> Doc {
        let head = concat(vec![self.expr_to_doc(&arm.condition), text(" ->")]);
        let head_trailing = self.comments.take(arm.span, Slot::HeaderTrailing);
        let dangling = self.comments.take(arm.span, Slot::Dangling);
        let trailing = self.comments.take(arm.span, Slot::Trailing);
        self.arm_body_to_doc(
            head,
            head_trailing,
            &arm.body,
            force_break,
            dangling,
            trailing,
        )
    }

    /// Shared formatting for all arm types (match, cond, receive).
    ///
    /// When `force_break` is true (because at least one sibling arm is
    /// multi-line), every arm body is indented on a new line for visual
    /// consistency. Otherwise single-statement arms may stay inline.
    ///
    /// `head_trailing` holds the comments on the arm-head line
    /// (`Pattern -> # note`), `dangling` the arm's trailing body
    /// comments, and `arm_trailing` an inline arm's end-of-line comment.
    /// Any comment forces the body onto its own line so it cannot
    /// swallow code or drift past the block.
    fn arm_body_to_doc(
        &mut self,
        head: Doc,
        head_trailing: Vec<Comment>,
        body: &[Statement],
        force_break: bool,
        dangling: Vec<Comment>,
        arm_trailing: Vec<Comment>,
    ) -> Doc {
        let head_tc = trailing_doc(&head_trailing);
        let arm_tc = trailing_doc(&arm_trailing);

        if body.len() == 1 && !force_break && dangling.is_empty() {
            let key = stmt_span(&body[0]);
            let leading = self.comments.take(key, Slot::Leading);
            let stmt_trailing = self.comments.take(key, Slot::Trailing);
            let mut stmt_doc = self.statement_to_doc(&body[0]);
            if let Some(tc) = trailing_doc(&stmt_trailing) {
                stmt_doc = concat(vec![stmt_doc, tc]);
            }
            if head_tc.is_none() && leading.is_empty() {
                let mut doc = group(concat(vec![
                    head,
                    indent(2, concat(vec![line(), stmt_doc])),
                ]));
                if let Some(tc) = arm_tc {
                    doc = concat(vec![doc, tc]);
                }
                return doc;
            }
            let mut parts = vec![head];
            if let Some(tc) = head_tc {
                parts.push(tc);
            }
            let mut body_parts = vec![hardline()];
            let (lead_docs, _) = leading_docs(&leading);
            body_parts.extend(lead_docs);
            body_parts.push(stmt_doc);
            parts.push(indent(2, concat(body_parts)));
            if let Some(tc) = arm_tc {
                parts.push(tc);
            }
            return concat(parts);
        }

        let mut parts = vec![head];
        if let Some(tc) = head_tc {
            parts.push(tc);
        }
        parts.push(indent(
            2,
            concat(vec![hardline(), self.statements_to_doc(body, dangling)]),
        ));
        if let Some(tc) = arm_tc {
            parts.push(tc);
        }
        concat(parts)
    }

    /// Formats a method chain of 2+ calls.
    ///
    /// Splits the left-recursive MethodCall tree into a root expression
    /// and a list of `.method(args)` links. When the chain fits on one
    /// line it stays inline. A chain with a single continuation call lets
    /// the anchor break its own arguments and hugs the trailing call to
    /// the closing paren, matching how a depth-1 call on a call receiver
    /// formats. Longer chains break every call onto its own line, indented
    /// from the root by `link_indent`. A comment between links forces the
    /// broken chain and anchors to its link.
    pub(super) fn method_chain_to_doc(&mut self, expr: &Expr, link_indent: LinkIndent) -> Doc {
        let hang = link_indent.width();
        let (root, links) = chain_links(expr);
        let root_doc = self.expr_to_doc(root);

        let mut entries: Vec<SeqEntry> = Vec::new();
        for link in &links {
            let ExprKind::MethodCall {
                method,
                args,
                receiver,
                ..
            } = &link.kind
            else {
                unreachable!()
            };
            let doc = concat(vec![
                text(format!(".{}", method)),
                self.call_args_to_doc(args, link.span),
            ]);
            // Link comments are keyed by the receiver span. See
            // `Attacher::walk_chain`.
            entries.push(SeqEntry {
                doc,
                end_line: link.span.end.line,
                force_blank: false,
                group: Group::Member,
                is_block: false,
                leading: self.comments.take(receiver.span, Slot::Leading),
                start_line: link.span.start.line,
                trailing: self.comments.take(receiver.span, Slot::Trailing),
            });
        }

        // Glue the first call to a simple root, break it for call-rooted
        // chains. A comment on the first call rules the glue out, since
        // glued, a trailing comment could swallow the next link when the
        // chain collapses.
        let glue_first =
            is_simple_chain_root(root) && entries.first().is_some_and(SeqEntry::comment_free);
        let anchor = if glue_first {
            let first = entries.remove(0);
            concat(vec![root_doc, first.doc])
        } else {
            root_doc
        };

        if entries.iter().all(SeqEntry::comment_free) {
            let docs: Vec<Doc> = entries.into_iter().map(|e| e.doc).collect();
            // With one continuation the anchor breaks its own arguments
            // first and the trailing call hugs the closing paren, only
            // moving to its own line when it still does not fit.
            if let [doc] = &docs[..] {
                let continuation = concat(vec![softline(), doc.clone()]);
                return concat(vec![anchor, group(indent(hang, continuation))]);
            }
            let mut chain_parts = Vec::with_capacity(docs.len() * 2);
            for doc in docs {
                chain_parts.push(softline());
                chain_parts.push(doc);
            }
            return group(concat(vec![anchor, indent(hang, concat(chain_parts))]));
        }

        let mut chain_parts = Vec::new();
        for entry in entries {
            chain_parts.push(hardline());
            let (lead_docs, _) = leading_docs(&entry.leading);
            chain_parts.extend(lead_docs);
            chain_parts.push(entry.doc);
            if let Some(tc) = trailing_doc(&entry.trailing) {
                chain_parts.push(tc);
            }
        }
        concat(vec![anchor, indent(hang, concat(chain_parts))])
    }
}

/// Link indent relative to the chain root. `Flush` is for a chain that
/// already broke after `=`.
#[derive(Clone, Copy)]
pub(super) enum LinkIndent {
    Flush,
    Hang,
}

impl LinkIndent {
    fn width(self) -> u32 {
        match self {
            LinkIndent::Flush => 0,
            LinkIndent::Hang => 2,
        }
    }
}

impl Printer {
    /// True when a comment will force the chain to break, either on a
    /// link (keyed by its receiver span, see `Attacher::walk_chain`) or
    /// anywhere inside the chain's arguments.
    pub(super) fn chain_has_comments(&self, expr: &Expr) -> bool {
        let (_, links) = chain_links(expr);
        links.iter().any(|link| {
            let ExprKind::MethodCall { receiver, .. } = &link.kind else {
                return false;
            };
            self.comments.has(receiver.span, Slot::Leading)
                || self.comments.has(receiver.span, Slot::Trailing)
        }) || self.comments.any_within(expr.span)
    }
}

/// Links that get their own line when the chain breaks. The call glued
/// to a simple root does not count.
pub(super) fn chain_continuations(expr: &Expr) -> usize {
    let (root, links) = chain_links(expr);
    if is_simple_chain_root(root) {
        links.len().saturating_sub(1)
    } else {
        links.len()
    }
}

/// Prepends leading comment docs to a rendered node.
fn with_leading(leading: &[Comment], doc: Doc) -> Doc {
    if leading.is_empty() {
        return doc;
    }
    let (docs, _) = leading_docs(leading);
    concat(docs.into_iter().chain([doc]).collect())
}

/// Assembles a `keyword ... arms ... end` block: the arms indented with
/// a blank line between them when `spaced`, an optional `suffix` between
/// the arms and `end` (the `after` clause), and the comments between
/// the last arm and `end`.
fn arms_block(
    header: Doc,
    arm_docs: Vec<Doc>,
    spaced: bool,
    suffix: Vec<Doc>,
    end_dangling: Vec<Comment>,
) -> Doc {
    let mut body = Vec::new();
    for (i, doc) in arm_docs.into_iter().enumerate() {
        body.push(hardline());
        if spaced && i > 0 {
            body.push(hardline());
        }
        body.push(doc);
    }
    push_stragglers(&mut body, &end_dangling);
    let mut parts = vec![header, indent(2, concat(body))];
    parts.extend(suffix);
    parts.push(hardline());
    parts.push(text("end"));
    concat(parts)
}

/// True for block or short closures (the hug-eligible argument shapes).
fn is_closure_arg(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Closure { .. } | ExprKind::ShortClosure { .. }
    )
}

/// True when a chain root is a simple receiver whose first call should stay
/// glued (the `StringBuilder.new()...` idiom), vs. a call-rooted pipeline.
fn is_simple_chain_root(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Ident { .. }
            | ExprKind::Self_ { .. }
            | ExprKind::Literal { .. }
            | ExprKind::String { .. }
            | ExprKind::FieldAccess { .. }
            | ExprKind::EnumConstruction {
                data: EnumConstructionData::Unit,
                ..
            }
    )
}
