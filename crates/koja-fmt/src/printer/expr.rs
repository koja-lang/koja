//! Expression and arm formatting for the pretty-printer.
//!
//! Contains the large `expr_to_doc` dispatch and all supporting methods that
//! format sub-expression forms (calls, strings, match/cond/receive arms,
//! etc.).

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::labels::pattern_span;

use super::Printer;
use super::util::*;

impl<'a> Printer<'a> {
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
                self.call_args_to_doc(args, expr.span.end.line),
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
                    concat(vec![
                        self.expr_to_doc(receiver),
                        text("."),
                        text(method.clone()),
                        self.call_args_to_doc(args, expr.span.end.line),
                    ])
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
                    let close_line = expr.span.end.line;
                    let entries = self.commented_entries(
                        elements,
                        close_line,
                        |e| (e.span.start.line, e.span.end.line),
                        |p, e| p.expr_to_doc(e),
                    );
                    self.element_list_to_doc("[", "]", entries, close_line)
                }
            }

            ExprKind::Tuple { elements } => {
                let close_line = expr.span.end.line;
                let entries = self.commented_entries(
                    elements,
                    close_line,
                    |e| (e.span.start.line, e.span.end.line),
                    |p, e| p.expr_to_doc(e),
                );
                self.element_list_to_doc("(", ")", entries, close_line)
            }

            ExprKind::Map { entries } => {
                if entries.is_empty() {
                    text("[:]")
                } else {
                    let close_line = expr.span.end.line;
                    let entry_docs = self.commented_entries(
                        entries,
                        close_line,
                        |(k, v)| (k.span.start.line, v.span.end.line),
                        |p, (k, v)| concat(vec![p.expr_to_doc(k), text(": "), p.expr_to_doc(v)]),
                    );
                    self.element_list_to_doc("[", "]", entry_docs, close_line)
                }
            }

            ExprKind::String { parts, multiline } => self.string_to_doc(parts, *multiline),

            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let mut parts = vec![
                    self.condition_header_to_doc("if ", condition),
                    self.body_to_doc(then_body, expr.span.end.line),
                ];
                if let Some(eb) = else_body {
                    parts.push(hardline());
                    parts.push(text("else"));
                    parts.push(self.body_to_doc(eb, expr.span.end.line));
                }
                parts.push(hardline());
                parts.push(text("end"));
                concat(parts)
            }

            ExprKind::Unless { condition, body } => concat(vec![
                self.condition_header_to_doc("unless ", condition),
                self.body_to_doc(body, expr.span.end.line),
                hardline(),
                text("end"),
            ]),

            ExprKind::Match { subject, arms } => {
                let any_multiline = arms.iter().any(|a| {
                    arm_is_multiline(&a.body)
                        || pattern_is_multiline(&a.pattern)
                        || arm_body_overflows(pattern_rendered_len(&a.pattern), &a.body)
                });
                let header = concat(vec![text("match "), self.expr_to_doc(subject)]);
                let rendered: Vec<Doc> = arms
                    .iter()
                    .enumerate()
                    .map(|(i, arm)| {
                        let body_end = arms
                            .get(i + 1)
                            .map_or(expr.span.end.line, |next| next.span.start.line);
                        self.with_leading_comments(arm.span.start.line, |p| {
                            p.match_arm_to_doc(arm, any_multiline, body_end)
                        })
                    })
                    .collect();
                arms_block(header, rendered, any_multiline, vec![])
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
                let mut rendered: Vec<Doc> = arms
                    .iter()
                    .enumerate()
                    .map(|(i, arm)| {
                        let body_end = arms
                            .get(i + 1)
                            .map_or(expr.span.end.line, |next| next.span.start.line);
                        self.with_leading_comments(arm.span.start.line, |p| {
                            p.cond_arm_to_doc(arm, any_multiline, body_end)
                        })
                    })
                    .collect();
                if let Some(body) = else_body {
                    rendered.push(self.else_arm_to_doc(body, any_multiline, expr.span.end.line));
                }
                arms_block(text("cond"), rendered, any_multiline, vec![])
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
                let rendered: Vec<Doc> = arms
                    .iter()
                    .enumerate()
                    .map(|(i, arm)| {
                        let body_end = arms
                            .get(i + 1)
                            .map_or(expr.span.end.line, |next| next.span.start.line);
                        self.with_leading_comments(arm.span.start.line, |p| {
                            p.match_arm_to_doc(arm, any_multiline, body_end)
                        })
                    })
                    .collect();
                let mut suffix = Vec::new();
                if let Some(timeout) = after_timeout {
                    suffix.push(hardline());
                    suffix.push(text("after "));
                    suffix.push(self.expr_to_doc(timeout));
                    suffix.push(self.body_to_doc(after_body, expr.span.end.line));
                }
                arms_block(text("receive"), rendered, any_multiline, suffix)
            }

            ExprKind::For {
                pattern,
                iterable,
                body,
            } => concat(vec![
                text("for "),
                pattern_to_doc(pattern),
                text(" in "),
                self.expr_to_doc(iterable),
                self.body_to_doc(body, expr.span.end.line),
                hardline(),
                text("end"),
            ]),

            ExprKind::Loop { body } => concat(vec![
                text("loop"),
                self.body_to_doc(body, expr.span.end.line),
                hardline(),
                text("end"),
            ]),

            ExprKind::While { condition, body } => concat(vec![
                self.condition_header_to_doc("while ", condition),
                self.body_to_doc(body, expr.span.end.line),
                hardline(),
                text("end"),
            ]),

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
                    // No interior comments (the gate guarantees it), so skip
                    // the draining of statements_to_doc. An end-line trailing
                    // comment stays pending and glues after `end`.
                    let body_doc = self.statement_to_doc(&body[0]);
                    group(concat(vec![
                        sig,
                        indent(2, concat(vec![line(), body_doc])),
                        line(),
                        text("end"),
                    ]))
                } else {
                    concat(vec![
                        sig,
                        self.body_to_doc(body, expr.span.end.line),
                        hardline(),
                        text("end"),
                    ])
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
                    self.construction_to_doc(text(path_str), fields, expr.span.end.line)
                }
            }

            ExprKind::BinaryLiteral { segments } => {
                if segments.is_empty() {
                    text("<<>>")
                } else {
                    let close_line = expr.span.end.line;
                    let entries = self.commented_entries(
                        segments,
                        close_line,
                        |seg| (seg.span.start.line, seg.span.end.line),
                        |p, seg| p.binary_segment_to_doc(seg),
                    );
                    self.element_list_to_doc("<<", ">>", entries, close_line)
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
                        self.construction_to_doc(text(prefix), fields, expr.span.end.line)
                    }
                }
            }
        }
    }

    /// Formats a parenthesized argument list for a call or method call.
    /// `close_line` is the line of the closing paren.
    pub(super) fn call_args_to_doc(&mut self, args: &[Arg], close_line: u32) -> Doc {
        if args.is_empty() {
            return text("()");
        }
        if let [arg] = args
            && arg.name.is_none()
            && (is_closure_arg(&arg.value) || is_heredoc(&arg.value))
            && self.comments.peek_before(arg.span.start.line).is_none()
        {
            // Hug a sole trailing closure or heredoc instead of exploding
            // the arg list. A pending comment before the argument rules
            // the hug out and takes the broken layout below.
            return concat(vec![text("("), self.arg_to_doc(arg), text(")")]);
        }
        let entries = self.commented_entries(
            args,
            close_line,
            |a| (a.span.start.line, a.span.end.line),
            |p, a| p.arg_to_doc(a),
        );
        if self.entries_comment_free(&entries, close_line) {
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
        let (stragglers, _) = self.comments.drain_before(close_line);
        concat(vec![
            text("("),
            indent(2, commented_field_lines(entries, stragglers)),
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

    /// Drains the comments anchored to each element of a bracketed
    /// construct. A trailing comment belongs to the last element on its
    /// line, and never to an element ending on the closing delimiter's
    /// line (a comment there follows the close, not the element).
    pub(super) fn commented_entries<T>(
        &mut self,
        items: &[T],
        close_line: u32,
        lines_of: impl Fn(&T) -> (u32, u32),
        mut to_doc: impl FnMut(&mut Self, &T) -> Doc,
    ) -> Vec<CommentedEntry> {
        items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let (start_line, end_line) = lines_of(item);
                let (leading, _) = self.comments.drain_before(start_line);
                let doc = to_doc(self, item);
                let last_on_line = items
                    .get(i + 1)
                    .is_none_or(|next| lines_of(next).0 > end_line);
                let trailing = if last_on_line && end_line < close_line {
                    self.comments.drain_trailing(end_line)
                } else {
                    None
                };
                CommentedEntry {
                    leading,
                    doc,
                    trailing,
                }
            })
            .collect()
    }

    /// True when no comment sits inside the construct, neither anchored to
    /// an element nor pending before the closing delimiter line.
    pub(super) fn entries_comment_free(&self, entries: &[CommentedEntry], close_line: u32) -> bool {
        entries.iter().all(CommentedEntry::comment_free)
            && self.comments.peek_before(close_line).is_none()
    }

    /// Formats an element list (list, tuple, map, or binary literal) with
    /// packed layout when comment-free and comment-aware packing otherwise.
    fn element_list_to_doc(
        &mut self,
        open: &str,
        close: &str,
        entries: Vec<CommentedEntry>,
        close_line: u32,
    ) -> Doc {
        if self.entries_comment_free(&entries, close_line) {
            let items = entries.into_iter().map(|e| e.doc).collect();
            return fill_bracket_list(open, close, items);
        }
        let (stragglers, _) = self.comments.drain_before(close_line);
        concat(vec![
            text(open),
            indent(2, commented_element_lines(entries, stragglers)),
            hardline(),
            text(close),
        ])
    }

    /// Formats a `prefix{field, ...}` field list with struct-literal
    /// layout when comment-free and one field per line otherwise.
    pub(super) fn field_list_to_doc(
        &mut self,
        prefix: Doc,
        entries: Vec<CommentedEntry>,
        close_line: u32,
    ) -> Doc {
        if self.entries_comment_free(&entries, close_line) {
            let docs = entries.into_iter().map(|e| e.doc).collect();
            return struct_body(prefix, docs);
        }
        let (stragglers, _) = self.comments.drain_before(close_line);
        concat(vec![
            prefix,
            text("{"),
            indent(2, commented_field_lines(entries, stragglers)),
            hardline(),
            text("}"),
        ])
    }

    /// Formats a `Prefix{field: value, ...}` construction with comments
    /// anchored to their fields.
    fn construction_to_doc(&mut self, prefix: Doc, fields: &[FieldInit], close_line: u32) -> Doc {
        let entries = self.commented_entries(
            fields,
            close_line,
            |fi| (fi.span.start.line, fi.span.end.line),
            |p, fi| p.field_init_to_doc(fi),
        );
        self.field_list_to_doc(prefix, entries, close_line)
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
    /// separates it from the body.
    fn condition_header_to_doc(&mut self, keyword: &str, condition: &Expr) -> Doc {
        group(concat(vec![
            text(keyword),
            self.expr_to_doc(condition),
            if_break(nil(), hardline()),
        ]))
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

    /// Formats a `#{expr}` or `#{expr:spec}` interpolation segment.
    fn interpolation_to_doc(&mut self, expr: &Expr, format: Option<&str>) -> Doc {
        let mut doc_parts = vec![text("#{"), self.expr_to_doc(expr)];
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
        is_inline_closure(expr) && self.comments.peek_before(expr.span.end.line).is_none()
    }

    /// Formats a `match` arm: `pattern [when guard] -> body`.
    pub(super) fn match_arm_to_doc(
        &mut self,
        arm: &MatchArm,
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let head_line = arm
            .guard
            .as_ref()
            .map_or(pattern_span(&arm.pattern).end.line, |g| g.span.end.line);
        let mut head = vec![pattern_to_doc(&arm.pattern)];
        if let Some(guard) = &arm.guard {
            head.push(text(" when "));
            head.push(self.expr_to_doc(guard));
        }
        head.push(text(" ->"));
        self.arm_body_to_doc(
            concat(head),
            Some(head_line),
            &arm.body,
            force_break,
            block_end,
        )
    }

    /// Formats a `cond` arm: `condition -> body`.
    pub(super) fn cond_arm_to_doc(
        &mut self,
        arm: &CondArm,
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let head_line = arm.condition.span.end.line;
        let head = concat(vec![self.expr_to_doc(&arm.condition), text(" ->")]);
        self.arm_body_to_doc(head, Some(head_line), &arm.body, force_break, block_end)
    }

    /// Formats an `else ->` arm in a `cond` expression.
    pub(super) fn else_arm_to_doc(
        &mut self,
        body: &[Statement],
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let head = text("else ->");
        self.arm_body_to_doc(head, None, body, force_break, block_end)
    }

    /// Shared formatting for all arm types (match, cond).
    ///
    /// When `force_break` is true (because at least one sibling arm is
    /// multi-line), every arm body is indented on a new line for visual
    /// consistency. Otherwise single-statement arms may stay inline.
    ///
    /// `head_line` anchors a trailing comment on the arm-head line
    /// (`Pattern -> # note`). A comment on the head or above the body
    /// statement forces the body onto its own line so the comment
    /// cannot swallow it or drift past the block.
    fn arm_body_to_doc(
        &mut self,
        head: Doc,
        head_line: Option<u32>,
        body: &[Statement],
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        // When the body starts on the head line, the per-statement
        // trailing drain below owns that line's comments.
        let head_trailing = head_line
            .filter(|line| body.first().map(stmt_start_line) != Some(*line))
            .and_then(|line| self.comments.drain_trailing(line));

        if body.len() == 1 && !force_break {
            let (leading, _) = self.comments.drain_before(stmt_start_line(&body[0]));
            let mut stmt_doc = self.statement_to_doc(&body[0]);
            if let Some(tc) = self.comments.drain_trailing(stmt_end_line(&body[0])) {
                stmt_doc = concat(vec![stmt_doc, tc]);
            }
            if head_trailing.is_none() && leading.is_empty() {
                return group(concat(vec![
                    head,
                    indent(2, concat(vec![line(), stmt_doc])),
                ]));
            }
            let mut parts = vec![head];
            if let Some(tc) = head_trailing {
                parts.push(tc);
            }
            let mut body_parts = vec![hardline()];
            body_parts.extend(leading);
            body_parts.push(stmt_doc);
            parts.push(indent(2, concat(body_parts)));
            return concat(parts);
        }

        let mut parts = vec![head];
        if let Some(tc) = head_trailing {
            parts.push(tc);
        }
        parts.push(indent(
            2,
            concat(vec![hardline(), self.statements_to_doc(body, block_end)]),
        ));
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
        struct Link<'a> {
            args: &'a [Arg],
            end_line: u32,
            /// Line the link's leading comments drain through. The AST has
            /// no span for the method name, so the first argument's line
            /// stands in (the link's end line for empty parens).
            lead_line: u32,
            method: &'a str,
        }

        let mut links: Vec<Link<'_>> = Vec::new();
        let mut current = expr;
        while let ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } = &current.kind
        {
            let end_line = current.span.end.line;
            let lead_line = args.first().map_or(end_line, |arg| arg.span.start.line);
            links.push(Link {
                args,
                end_line,
                lead_line,
                method,
            });
            current = receiver;
        }
        links.reverse();

        let root_doc = self.expr_to_doc(current);
        let last_end_line = expr.span.end.line;

        // Drain comments per link in source order. The final link's
        // trailing comment belongs to the enclosing statement.
        let mut entries: Vec<CommentedEntry> = Vec::new();
        for link in &links {
            let (leading, _) = self.comments.drain_before(link.lead_line);
            let doc = concat(vec![
                text(format!(".{}", link.method)),
                self.call_args_to_doc(link.args, link.end_line),
            ]);
            let trailing = if link.end_line < last_end_line {
                self.comments.drain_trailing(link.end_line)
            } else {
                None
            };
            entries.push(CommentedEntry {
                leading,
                doc,
                trailing,
            });
        }

        // Glue the first call to a simple root, break it for call-rooted
        // chains. A comment on the first call rules the glue out: glued,
        // a trailing comment could swallow the next link when the chain
        // collapses.
        let glue_first = is_simple_chain_root(current)
            && entries.first().is_some_and(CommentedEntry::comment_free);
        let anchor = if glue_first {
            let first = entries.remove(0);
            concat(vec![root_doc, first.doc])
        } else {
            root_doc
        };

        if entries.iter().all(CommentedEntry::comment_free) {
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
            chain_parts.extend(entry.leading);
            chain_parts.push(entry.doc);
            if let Some(tc) = entry.trailing {
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
