//! Expression and arm formatting for the pretty-printer.
//!
//! Contains the large `expr_to_doc` dispatch and all supporting methods that
//! format sub-expression forms (calls, strings, match/cond/receive arms,
//! etc.).

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;

use super::Printer;
use super::attach::Slot;
use super::comments::{leading_docs, trailing_doc};
use super::seq::{SeqEntry, element_lines, field_lines};
use super::util::*;

/// Prepends leading comment docs to a rendered node.
fn with_leading(leading: &[Comment], doc: Doc) -> Doc {
    if leading.is_empty() {
        return doc;
    }
    let (docs, _) = leading_docs(leading);
    concat(docs.into_iter().chain([doc]).collect())
}

impl Printer {
    /// Formats any expression AST node into a `Doc`.
    pub(super) fn expr_to_doc(&mut self, expr: &Expr) -> Doc {
        match &expr.kind {
            ExprKind::Literal { value } => literal_to_doc(value),
            ExprKind::Ident { name, .. } => text(name.clone()),
            ExprKind::Self_ { .. } => text("self"),

            // `and` / `or` chains pack densely with the operator leading
            // each item, so a wrapped chain starts its continuation lines
            // with the operator, indented two past where the chain began.
            ExprKind::Binary {
                op: op @ (BinOp::Or | BinOp::And),
                ..
            } => {
                let op_str = binop_str(op);
                let operands = self.flatten_binop_chain(expr, op);
                let items: Vec<Doc> = operands
                    .into_iter()
                    .enumerate()
                    .map(|(i, doc)| {
                        if i == 0 {
                            doc
                        } else {
                            concat(vec![text(op_str), text(" "), doc])
                        }
                    })
                    .collect();
                indent(2, fill(items))
            }

            // Other binary operators pack the same way but keep the
            // operator trailing (a leading operator would not parse), so
            // a wrapped chain leaves the operator at the end of the line.
            ExprKind::Binary { op, .. } => {
                let op_str = binop_str(op);
                let operands = self.flatten_binop_chain(expr, op);
                let last = operands.len() - 1;
                let items: Vec<Doc> = operands
                    .into_iter()
                    .enumerate()
                    .map(|(i, doc)| {
                        if i == last {
                            doc
                        } else {
                            concat(vec![doc, text(" "), text(op_str)])
                        }
                    })
                    .collect();
                indent(2, fill(items))
            }

            ExprKind::Unary { op, operand } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
                };
                concat(vec![text(op_str), self.expr_to_doc(operand)])
            }

            ExprKind::Group { expr: inner } => {
                concat(vec![text("("), self.expr_to_doc(inner), text(")")])
            }

            ExprKind::Call { callee, args, .. } => concat(vec![
                self.expr_to_doc(callee),
                self.call_args_to_doc(args, expr.span),
            ]),

            ExprKind::MethodCall { .. } => {
                let depth = method_chain_depth(expr);
                if depth >= 2 {
                    self.method_chain_to_doc(expr)
                } else {
                    let ExprKind::MethodCall {
                        receiver,
                        method,
                        args,
                        ..
                    } = &expr.kind
                    else {
                        unreachable!()
                    };
                    if let Some(receiver_body) = self.collection_literal_body(receiver) {
                        // The shared group makes the literal's brackets
                        // split first, and the call hugs the closing
                        // bracket.
                        group(concat(vec![
                            receiver_body,
                            text("."),
                            text(method.clone()),
                            self.call_args_to_doc(args, expr.span),
                        ]))
                    } else {
                        concat(vec![
                            self.expr_to_doc(receiver),
                            text("."),
                            text(method.clone()),
                            self.call_args_to_doc(args, expr.span),
                        ])
                    }
                }
            }

            ExprKind::FieldAccess { receiver, field } => concat(vec![
                self.expr_to_doc(receiver),
                text("."),
                text(field.clone()),
            ]),

            ExprKind::List { elements } => {
                if elements.is_empty() {
                    text("[]")
                } else {
                    let entries = self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e));
                    self.element_list_to_doc("[", "]", entries, expr.span)
                }
            }

            ExprKind::Tuple { elements } => {
                let entries = self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e));
                self.element_list_to_doc("(", ")", entries, expr.span)
            }

            ExprKind::Map { entries } => {
                if entries.is_empty() {
                    text("[:]")
                } else {
                    let entry_docs = self.seq_entries(
                        entries,
                        |(k, v)| map_entry_span(k, v),
                        |p, (k, v)| concat(vec![p.expr_to_doc(k), text(": "), p.expr_to_doc(v)]),
                    );
                    self.element_list_to_doc("[", "]", entry_docs, expr.span)
                }
            }

            ExprKind::String { parts, multiline } => self.string_to_doc(parts, *multiline),

            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let dangling = self.comments.take(expr.span, Slot::Dangling);
                let mut parts = vec![self.condition_header_to_doc("if ", condition, expr.span)];
                match else_body {
                    Some(eb) => {
                        parts.push(self.body_to_doc(then_body, Vec::new()));
                        parts.push(hardline());
                        let before_else = self.comments.take(expr.span, Slot::BeforeElse);
                        let (docs, _) = leading_docs(&before_else);
                        parts.extend(docs);
                        parts.push(text("else"));
                        let else_trailing = self.comments.take(expr.span, Slot::ElseTrailing);
                        if let Some(tc) = trailing_doc(&else_trailing) {
                            parts.push(tc);
                        }
                        parts.push(self.body_to_doc(eb, dangling));
                    }
                    None => parts.push(self.body_to_doc(then_body, dangling)),
                }
                parts.push(hardline());
                parts.push(text("end"));
                concat(parts)
            }

            ExprKind::Unless { condition, body } => {
                let dangling = self.comments.take(expr.span, Slot::Dangling);
                concat(vec![
                    self.condition_header_to_doc("unless ", condition, expr.span),
                    self.body_to_doc(body, dangling),
                    hardline(),
                    text("end"),
                ])
            }

            ExprKind::Match { subject, arms } => {
                let any_multiline = arms.iter().any(|a| {
                    arm_is_multiline(&a.body)
                        || pattern_is_multiline(&a.pattern)
                        || arm_body_overflows(pattern_rendered_len(&a.pattern), &a.body)
                });
                let mut header_parts = vec![text("match "), self.expr_to_doc(subject)];
                self.push_expr_header_trailing(&mut header_parts, expr.span);
                let rendered: Vec<Doc> = arms
                    .iter()
                    .map(|arm| {
                        let leading = self.comments.take(arm.span, Slot::Leading);
                        let doc = self.match_arm_to_doc(arm, any_multiline);
                        with_leading(&leading, doc)
                    })
                    .collect();
                let end_dangling = self.comments.take(expr.span, Slot::Dangling);
                arms_block(
                    concat(header_parts),
                    rendered,
                    any_multiline,
                    vec![],
                    end_dangling,
                )
            }

            ExprKind::Cond { arms, else_body } => {
                let else_multiline = else_body
                    .as_ref()
                    .is_some_and(|b| arm_is_multiline(b) || arm_body_overflows(0, b));
                let any_multiline = else_multiline
                    || arms.iter().any(|a| {
                        arm_is_multiline(&a.body)
                            || expr_or_is_multiline(&a.condition)
                            || arm_body_overflows(expr_text_len(&a.condition), &a.body)
                    });
                let mut header_parts = vec![text("cond")];
                self.push_expr_header_trailing(&mut header_parts, expr.span);
                let mut rendered: Vec<Doc> = arms
                    .iter()
                    .map(|arm| {
                        let leading = self.comments.take(arm.span, Slot::Leading);
                        let doc = self.cond_arm_to_doc(arm, any_multiline);
                        with_leading(&leading, doc)
                    })
                    .collect();
                if let Some(body) = else_body {
                    let leading = self.comments.take(expr.span, Slot::BeforeElse);
                    let head_trailing = self.comments.take(expr.span, Slot::ElseTrailing);
                    let dangling = self.comments.take(expr.span, Slot::Dangling);
                    let doc = self.arm_body_to_doc(
                        text("else ->"),
                        head_trailing,
                        body,
                        any_multiline,
                        dangling,
                        Vec::new(),
                    );
                    rendered.push(with_leading(&leading, doc));
                }
                let end_dangling = self.comments.take(expr.span, Slot::Dangling);
                arms_block(
                    concat(header_parts),
                    rendered,
                    any_multiline,
                    vec![],
                    end_dangling,
                )
            }

            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                let any_multiline = arms.iter().any(|a| {
                    arm_is_multiline(&a.body)
                        || pattern_is_multiline(&a.pattern)
                        || arm_body_overflows(pattern_rendered_len(&a.pattern), &a.body)
                }) || arm_is_multiline(after_body);
                let mut header_parts = vec![text("receive")];
                self.push_expr_header_trailing(&mut header_parts, expr.span);
                let rendered: Vec<Doc> = arms
                    .iter()
                    .map(|arm| {
                        let leading = self.comments.take(arm.span, Slot::Leading);
                        let doc = self.match_arm_to_doc(arm, any_multiline);
                        with_leading(&leading, doc)
                    })
                    .collect();
                let mut suffix = Vec::new();
                if let Some(timeout) = after_timeout {
                    suffix.push(hardline());
                    let before_after = self.comments.take(expr.span, Slot::BeforeAfter);
                    let (docs, _) = leading_docs(&before_after);
                    suffix.extend(docs);
                    suffix.push(text("after "));
                    suffix.push(self.expr_to_doc(timeout));
                    let after_trailing = self.comments.take(timeout.span, Slot::HeaderTrailing);
                    if let Some(tc) = trailing_doc(&after_trailing) {
                        suffix.push(tc);
                    }
                    let dangling = self.comments.take(expr.span, Slot::Dangling);
                    suffix.push(self.body_to_doc(after_body, dangling));
                }
                let end_dangling = self.comments.take(expr.span, Slot::Dangling);
                arms_block(
                    concat(header_parts),
                    rendered,
                    any_multiline,
                    suffix,
                    end_dangling,
                )
            }

            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                let pattern_doc = self.pattern_to_doc(pattern);
                let mut header_parts = vec![
                    text("for "),
                    pattern_doc,
                    text(" in "),
                    self.expr_to_doc(iterable),
                ];
                self.push_expr_header_trailing(&mut header_parts, expr.span);
                let dangling = self.comments.take(expr.span, Slot::Dangling);
                concat(vec![
                    concat(header_parts),
                    self.body_to_doc(body, dangling),
                    hardline(),
                    text("end"),
                ])
            }

            ExprKind::Loop { body } => {
                let mut header_parts = vec![text("loop")];
                self.push_expr_header_trailing(&mut header_parts, expr.span);
                let dangling = self.comments.take(expr.span, Slot::Dangling);
                concat(vec![
                    concat(header_parts),
                    self.body_to_doc(body, dangling),
                    hardline(),
                    text("end"),
                ])
            }

            ExprKind::While { condition, body } => {
                let dangling = self.comments.take(expr.span, Slot::Dangling);
                concat(vec![
                    self.condition_header_to_doc("while ", condition, expr.span),
                    self.body_to_doc(body, dangling),
                    hardline(),
                    text("end"),
                ])
            }

            ExprKind::Closure {
                params,
                return_type,
                body,
            } => {
                let params_doc: Vec<Doc> = params.iter().map(closure_param_to_doc).collect();
                let mut sig_parts =
                    vec![text("fn ("), intersperse(params_doc, text(", ")), text(")")];
                if let Some(rt) = return_type {
                    sig_parts.push(text(" -> "));
                    sig_parts.push(type_expr_to_doc(rt));
                }
                let sig = concat(sig_parts);
                if self.closure_renders_inline(expr) {
                    // No interior comments (the gate guarantees it), so
                    // the single statement prints directly. An end-line
                    // trailing comment stays with the enclosing context
                    // and glues after `end`.
                    let body_doc = self.statement_to_doc(&body[0]);
                    group(concat(vec![
                        sig,
                        indent(2, concat(vec![line(), body_doc])),
                        line(),
                        text("end"),
                    ]))
                } else {
                    let mut parts = vec![sig];
                    self.push_expr_header_trailing(&mut parts, expr.span);
                    let dangling = self.comments.take(expr.span, Slot::Dangling);
                    parts.push(self.body_to_doc(body, dangling));
                    parts.push(hardline());
                    parts.push(text("end"));
                    concat(parts)
                }
            }

            ExprKind::ShortClosure { params, body } => {
                let params_doc: Vec<Doc> = params.iter().map(closure_param_to_doc).collect();
                group(concat(vec![
                    intersperse(params_doc, text(", ")),
                    text(" -> "),
                    self.expr_to_doc(body),
                ]))
            }

            ExprKind::Spawn { expr: inner } => {
                concat(vec![text("spawn "), self.expr_to_doc(inner)])
            }

            ExprKind::Try { expr: inner } => concat(vec![text("try "), self.expr_to_doc(inner)]),

            ExprKind::Fail { value } => concat(vec![text("fail "), self.expr_to_doc(value)]),

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

            ExprKind::StructConstruction { type_path, fields } => {
                let path_str = type_path.join(".");
                if fields.is_empty() {
                    text(format!("{}{{}}", path_str))
                } else {
                    self.construction_to_doc(text(path_str), fields, expr.span)
                }
            }

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

            ExprKind::EnumConstruction {
                type_path,
                variant,
                data,
            } => {
                let prefix = if type_path.is_empty() {
                    variant.clone()
                } else {
                    format!("{}.{}", type_path.join("."), variant)
                };
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
        }
    }

    /// Appends the trailing comment attached to a block expression's
    /// header line, if any.
    fn push_expr_header_trailing(&mut self, parts: &mut Vec<Doc>, owner: Span) {
        let trailing = self.comments.take(owner, Slot::HeaderTrailing);
        if let Some(tc) = trailing_doc(&trailing) {
            parts.push(tc);
        }
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
            return concat(vec![
                text("("),
                indent(2, field_lines(Vec::new(), stragglers)),
                hardline(),
                text(")"),
            ]);
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
            return group(concat(vec![
                text("("),
                indent(
                    2,
                    concat(vec![
                        softline(),
                        intersperse(arg_docs, concat(vec![text(","), line()])),
                        trailing_comma(),
                    ]),
                ),
                softline(),
                text(")"),
            ]));
        }
        let stragglers = self.comments.take(owner, Slot::Stragglers);
        concat(vec![
            text("("),
            indent(2, field_lines(entries, stragglers)),
            hardline(),
            text(")"),
        ])
    }

    /// Formats a single call argument, with optional keyword name.
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
        let body = self.element_list_body(open, close, entries, owner);
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
    ) -> Doc {
        if self.entries_comment_free(&entries, owner) {
            let items = entries.into_iter().map(|e| e.doc).collect();
            return bracket_list_body(open, close, items);
        }
        let stragglers = self.comments.take(owner, Slot::Stragglers);
        concat(vec![
            text(open),
            indent(2, element_lines(entries, stragglers)),
            hardline(),
            text(close),
        ])
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
            ExprKind::Map { entries } if !entries.is_empty() => (
                "[",
                "]",
                self.seq_entries(
                    entries,
                    |(k, v)| map_entry_span(k, v),
                    |p, (k, v)| concat(vec![p.expr_to_doc(k), text(": "), p.expr_to_doc(v)]),
                ),
            ),
            ExprKind::Tuple { elements } => (
                "(",
                ")",
                self.seq_entries(elements, |e| e.span, |p, e| p.expr_to_doc(e)),
            ),
            _ => return None,
        };
        Some(self.element_list_body(open, close, entries, expr.span))
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
            text("{"),
            indent(2, field_lines(entries, stragglers)),
            hardline(),
            text("}"),
        ])
    }

    /// Formats a `Prefix{field: value, ...}` construction with comments
    /// anchored to their fields.
    fn construction_to_doc(&mut self, prefix: Doc, fields: &[FieldInit], owner: Span) -> Doc {
        let entries = self.seq_entries(fields, |fi| fi.span, |p, fi| p.field_init_to_doc(fi));
        self.field_list_to_doc(prefix, entries, owner)
    }

    /// Formats a struct field initializer (`name: value`).
    fn field_init_to_doc(&mut self, fi: &FieldInit) -> Doc {
        concat(vec![
            text(&fi.name),
            text(": "),
            self.expr_to_doc(&fi.value),
        ])
    }

    fn binary_segment_to_doc(&mut self, seg: &BinarySegment) -> Doc {
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

    /// Formats an `if` / `unless` / `while` header. Like wrapped
    /// function signatures, a wrapped condition indents two (the
    /// expression doc hangs its own continuations) and a blank line
    /// separates it from the body. `owner` keys the header's trailing
    /// comment.
    fn condition_header_to_doc(&mut self, keyword: &str, condition: &Expr, owner: Span) -> Doc {
        let mut parts = vec![text(keyword), self.expr_to_doc(condition)];
        self.push_expr_header_trailing(&mut parts, owner);
        parts.push(if_break(nil(), hardline()));
        group(concat(parts))
    }

    /// Flattens a chain of same-operator binary expressions into a list of
    /// operand docs for fill-style packing.
    fn flatten_binop_chain(&mut self, expr: &Expr, target_op: &BinOp) -> Vec<Doc> {
        let mut operands = Vec::new();
        self.collect_binop_operands(expr, target_op, &mut operands);
        operands
    }

    fn collect_binop_operands(&mut self, expr: &Expr, target_op: &BinOp, out: &mut Vec<Doc>) {
        if let ExprKind::Binary { op, left, right } = &expr.kind
            && std::mem::discriminant(op) == std::mem::discriminant(target_op)
        {
            self.collect_binop_operands(left, target_op, out);
            self.collect_binop_operands(right, target_op, out);
            return;
        }
        out.push(self.expr_to_doc(expr));
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

    /// Whether a closure takes the collapsed single-line layout. A comment
    /// inside the closure span rules it out (inlined, the comment would
    /// swallow `end`).
    pub(super) fn closure_renders_inline(&self, expr: &Expr) -> bool {
        is_inline_closure(expr) && !self.comments.any_within(expr.span)
    }

    /// Formats a `match` arm: `pattern [when guard] -> body`.
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

    /// Formats a `cond` arm: `condition -> body`.
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
    /// Flattens the left-recursive MethodCall tree into a root expression
    /// and a list of `.method(args)` segments. When the chain fits on one
    /// line it stays inline. A chain with a single continuation call lets
    /// the anchor break its own arguments and hugs the trailing call to
    /// the closing paren, matching how a depth-1 call on a call receiver
    /// formats. Longer chains break every call onto its own line indented
    /// 2 from the root. A comment between links forces the broken chain
    /// and anchors to its link.
    fn method_chain_to_doc(&mut self, expr: &Expr) -> Doc {
        let mut links: Vec<&Expr> = Vec::new();
        let mut current = expr;
        while let ExprKind::MethodCall { receiver, .. } = &current.kind {
            links.push(current);
            current = receiver;
        }
        links.reverse();

        let root_doc = self.expr_to_doc(current);

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
            is_simple_chain_root(current) && entries.first().is_some_and(SeqEntry::comment_free);
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
                return concat(vec![anchor, group(indent(2, continuation))]);
            }
            let mut chain_parts = Vec::with_capacity(docs.len() * 2);
            for doc in docs {
                chain_parts.push(softline());
                chain_parts.push(doc);
            }
            return group(concat(vec![anchor, indent(2, concat(chain_parts))]));
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
        concat(vec![anchor, indent(2, concat(chain_parts))])
    }
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

/// Counts the depth of nested MethodCall nodes on the left spine.
fn method_chain_depth(expr: &Expr) -> usize {
    let mut depth = 0;
    let mut current = expr;
    while let ExprKind::MethodCall { receiver, .. } = &current.kind {
        depth += 1;
        current = receiver;
    }
    depth
}
