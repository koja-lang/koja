//! Expression and arm formatting for the pretty-printer.
//!
//! Contains the large `expr_to_doc` dispatch and all supporting methods that
//! format sub-expression forms (calls, strings, match/cond/receive arms,
//! etc.).

use crate::doc::*;
use koja_ast::ast::*;

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

            ExprKind::Call { callee, args, .. } => {
                concat(vec![self.expr_to_doc(callee), self.call_args_to_doc(args)])
            }

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
                        self.call_args_to_doc(args),
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
                    let items: Vec<Doc> = elements.iter().map(|e| self.expr_to_doc(e)).collect();
                    fill_bracket_list("[", "]", items)
                }
            }

            ExprKind::Map { entries } => {
                if entries.is_empty() {
                    text("[:]")
                } else {
                    let items: Vec<Doc> = entries
                        .iter()
                        .map(|(k, v)| {
                            concat(vec![self.expr_to_doc(k), text(": "), self.expr_to_doc(v)])
                        })
                        .collect();
                    fill_bracket_list("[", "]", items)
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
                let rendered: Vec<Doc> = arms
                    .iter()
                    .enumerate()
                    .map(|(i, arm)| {
                        let body_end = arms
                            .get(i + 1)
                            .map_or(expr.span.end.line, |next| next.span.start.line);
                        self.match_arm_to_doc(arm, any_multiline, body_end)
                    })
                    .collect();
                let header = concat(vec![text("match "), self.expr_to_doc(subject)]);
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
                        self.cond_arm_to_doc(arm, any_multiline, body_end)
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
                        self.match_arm_to_doc(arm, any_multiline, body_end)
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
                if body.len() == 1 {
                    let body_doc = self.statements_to_doc(body, expr.span.end.line);
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
                    self.construction_to_doc(text(path_str), fields)
                }
            }

            ExprKind::BinaryLiteral { segments } => {
                if segments.is_empty() {
                    text("<<>>")
                } else {
                    let seg_docs: Vec<Doc> = segments
                        .iter()
                        .map(|seg| self.binary_segment_to_doc(seg))
                        .collect();
                    fill_bracket_list("<<", ">>", seg_docs)
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
                        self.construction_to_doc(text(prefix), fields)
                    }
                }
            }
        }
    }

    /// Formats a parenthesized argument list for a call or method call.
    pub(super) fn call_args_to_doc(&mut self, args: &[Arg]) -> Doc {
        if args.is_empty() {
            text("()")
        } else if let [arg] = args
            && arg.name.is_none()
            && is_closure_arg(&arg.value)
        {
            // Hug a sole trailing closure instead of exploding the arg list.
            concat(vec![text("("), self.arg_to_doc(arg), text(")")])
        } else {
            let arg_docs: Vec<Doc> = args.iter().map(|a| self.arg_to_doc(a)).collect();
            group(concat(vec![
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
            ]))
        }
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

    /// Formats a `Prefix{field: value, ...}` construction, attaching any
    /// trailing comment on a field's line to that field. A comment forces
    /// the multi-line layout so the fields after it aren't commented out.
    fn construction_to_doc(&mut self, prefix: Doc, fields: &[FieldInit]) -> Doc {
        let entries: Vec<(Doc, Option<Doc>)> = fields
            .iter()
            .map(|fi| {
                let field_doc = self.field_init_to_doc(fi);
                (field_doc, self.comments.drain_trailing(fi.span.end.line))
            })
            .collect();

        if entries.iter().all(|(_, comment)| comment.is_none()) {
            return struct_body(prefix, entries.into_iter().map(|(d, _)| d).collect());
        }

        let mut body = Vec::new();
        for (field_doc, comment) in entries {
            body.push(hardline());
            body.push(field_doc);
            body.push(text(","));
            if let Some(comment) = comment {
                body.push(comment);
            }
        }
        concat(vec![
            prefix,
            text("{"),
            indent(2, concat(body)),
            hardline(),
            text("}"),
        ])
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

    /// Formats a string literal (single-line or multi-line heredoc).
    fn string_to_doc(&mut self, parts: &[StringPart], multiline: bool) -> Doc {
        if multiline {
            let mut doc_parts = vec![text("\"\"\"")];
            for part in parts {
                match part {
                    StringPart::Literal { value, .. } => {
                        let escaped = escape_multiline_literal(value);
                        for (i, l) in escaped.split('\n').enumerate() {
                            if i > 0 {
                                doc_parts.push(hardline());
                            }
                            doc_parts.push(text(l.to_string()));
                        }
                    }
                    StringPart::Interpolation { expr, format, .. } => {
                        doc_parts.push(text("#{"));
                        doc_parts.push(self.expr_to_doc(expr));
                        if let Some(fmt) = format {
                            doc_parts.push(text(format!(":{}", fmt)));
                        }
                        doc_parts.push(text("}"));
                    }
                }
            }
            doc_parts.push(text("\"\"\""));
            concat(doc_parts)
        } else {
            let mut doc_parts = vec![text("\"")];
            for part in parts {
                match part {
                    StringPart::Literal { value, .. } => {
                        doc_parts.push(text(escape_string_literal(value)));
                    }
                    StringPart::Interpolation { expr, format, .. } => {
                        doc_parts.push(text("#{"));
                        doc_parts.push(self.expr_to_doc(expr));
                        if let Some(fmt) = format {
                            doc_parts.push(text(format!(":{}", fmt)));
                        }
                        doc_parts.push(text("}"));
                    }
                }
            }
            doc_parts.push(text("\""));
            concat(doc_parts)
        }
    }

    /// Formats a `match` arm: `pattern [when guard] -> body`.
    pub(super) fn match_arm_to_doc(
        &mut self,
        arm: &MatchArm,
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let mut head = vec![pattern_to_doc(&arm.pattern)];
        if let Some(guard) = &arm.guard {
            head.push(text(" when "));
            head.push(self.expr_to_doc(guard));
        }
        head.push(text(" ->"));
        self.arm_body_to_doc(concat(head), &arm.body, force_break, block_end)
    }

    /// Formats a `cond` arm: `condition -> body`.
    pub(super) fn cond_arm_to_doc(
        &mut self,
        arm: &CondArm,
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let head = concat(vec![self.expr_to_doc(&arm.condition), text(" ->")]);
        self.arm_body_to_doc(head, &arm.body, force_break, block_end)
    }

    /// Formats an `else ->` arm in a `cond` expression.
    pub(super) fn else_arm_to_doc(
        &mut self,
        body: &[Statement],
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let head = text("else ->");
        self.arm_body_to_doc(head, body, force_break, block_end)
    }

    /// Shared formatting for all arm types (match, cond).
    ///
    /// When `force_break` is true (because at least one sibling arm is
    /// multi-line), every arm body is indented on a new line for visual
    /// consistency. Otherwise single-statement arms may stay inline.
    fn arm_body_to_doc(
        &mut self,
        head: Doc,
        body: &[Statement],
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        if body.len() == 1 && !force_break {
            group(concat(vec![
                head,
                indent(2, concat(vec![line(), self.statement_to_doc(&body[0])])),
            ]))
        } else {
            concat(vec![
                head,
                indent(
                    2,
                    concat(vec![hardline(), self.statements_to_doc(body, block_end)]),
                ),
            ])
        }
    }

    /// Formats a method chain of 3+ calls with one-per-line breaking.
    ///
    /// Flattens the left-recursive MethodCall tree into a root expression
    /// and a list of `.method(args)` segments. When the chain fits on one
    /// line it stays inline. Otherwise each call breaks onto its own line
    /// indented 2 from the root.
    fn method_chain_to_doc(&mut self, expr: &Expr) -> Doc {
        let mut calls: Vec<(&str, &[Arg])> = Vec::new();
        let mut current = expr;
        while let ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } = &current.kind
        {
            calls.push((method.as_str(), args.as_slice()));
            current = receiver;
        }
        calls.reverse();

        let root_doc = self.expr_to_doc(current);

        // Glue the first call to a simple root, break it for call-rooted chains.
        let anchor = if is_simple_chain_root(current) {
            let (first_method, first_args) = calls.remove(0);
            concat(vec![
                root_doc,
                text(format!(".{}", first_method)),
                self.call_args_to_doc(first_args),
            ])
        } else {
            root_doc
        };

        let mut chain_parts = Vec::with_capacity(calls.len());
        for (method, args) in calls {
            chain_parts.push(softline());
            chain_parts.push(text(format!(".{}", method)));
            chain_parts.push(self.call_args_to_doc(args));
        }

        group(concat(vec![anchor, indent(2, concat(chain_parts))]))
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
