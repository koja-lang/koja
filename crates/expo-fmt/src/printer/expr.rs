//! Expression and arm formatting for the pretty-printer.
//!
//! Contains the large `expr_to_doc` dispatch and all supporting methods that
//! format sub-expression forms (calls, strings, match/cond/receive arms,
//! etc.).

use crate::doc::*;
use expo_ast::ast::*;

use super::Printer;
use super::util::*;

impl<'a> Printer<'a> {
    /// Formats any expression AST node into a `Doc`.
    pub(super) fn expr_to_doc(&mut self, expr: &Expr) -> Doc {
        match expr {
            Expr::Literal { value, .. } => literal_to_doc(value),
            Expr::Ident { name, .. } => text(name.clone()),
            Expr::Self_ { .. } => text("self"),

            Expr::Binary {
                op, left, right, ..
            } => {
                let op_str = binop_str(op);
                group(concat(vec![
                    self.expr_to_doc(left),
                    text(" "),
                    text(op_str),
                    line(),
                    self.expr_to_doc(right),
                ]))
            }

            Expr::Unary { op, operand, .. } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
                };
                concat(vec![text(op_str), self.expr_to_doc(operand)])
            }

            Expr::Group { expr: inner, .. } => {
                concat(vec![text("("), self.expr_to_doc(inner), text(")")])
            }

            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                let mut parts = vec![self.expr_to_doc(callee)];
                if let Some(ta) = type_args {
                    parts.push(text("::<"));
                    let type_docs: Vec<Doc> = ta.iter().map(type_expr_to_doc).collect();
                    parts.push(intersperse(type_docs, text(", ")));
                    parts.push(text(">"));
                }
                parts.push(self.call_args_to_doc(args));
                concat(parts)
            }

            Expr::MethodCall {
                receiver,
                method,
                type_args,
                args,
                ..
            } => {
                let mut chain = vec![self.expr_to_doc(receiver)];
                chain.push(text("."));
                chain.push(text(method.clone()));
                if let Some(ta) = type_args {
                    chain.push(text("::<"));
                    let type_docs: Vec<Doc> = ta.iter().map(type_expr_to_doc).collect();
                    chain.push(intersperse(type_docs, text(", ")));
                    chain.push(text(">"));
                }
                chain.push(self.call_args_to_doc(args));
                concat(chain)
            }

            Expr::FieldAccess {
                receiver, field, ..
            } => concat(vec![
                self.expr_to_doc(receiver),
                text("."),
                text(field.clone()),
            ]),

            Expr::List { elements, .. } => {
                if elements.is_empty() {
                    text("[]")
                } else {
                    let items: Vec<Doc> = elements
                        .iter()
                        .enumerate()
                        .map(|(i, e)| {
                            if i < elements.len() - 1 {
                                concat(vec![self.expr_to_doc(e), text(",")])
                            } else {
                                self.expr_to_doc(e)
                            }
                        })
                        .collect();
                    let fill_items: Vec<Doc> = items
                        .into_iter()
                        .enumerate()
                        .map(|(i, d)| if i > 0 { concat(vec![text(" "), d]) } else { d })
                        .collect();
                    group(concat(vec![
                        text("["),
                        indent(2, concat(vec![softline(), fill(fill_items)])),
                        softline(),
                        text("]"),
                    ]))
                }
            }

            Expr::Tuple { elements, .. } => {
                let elems: Vec<Doc> = elements.iter().map(|e| self.expr_to_doc(e)).collect();
                group(concat(vec![
                    text("("),
                    indent(
                        2,
                        concat(vec![
                            softline(),
                            intersperse(elems, concat(vec![text(","), line()])),
                        ]),
                    ),
                    softline(),
                    text(")"),
                ]))
            }

            Expr::String {
                parts, multiline, ..
            } => self.string_to_doc(parts, *multiline),

            Expr::If {
                condition,
                then_body,
                else_body,
                span,
                ..
            } => {
                let mut parts = vec![
                    text("if "),
                    self.expr_to_doc(condition),
                    self.body_to_doc(then_body, span.end.line),
                ];
                if let Some(eb) = else_body {
                    parts.push(hardline());
                    parts.push(text("else"));
                    parts.push(self.body_to_doc(eb, span.end.line));
                }
                parts.push(hardline());
                parts.push(text("end"));
                concat(parts)
            }

            Expr::Unless {
                condition,
                body,
                span,
                ..
            } => concat(vec![
                text("unless "),
                self.expr_to_doc(condition),
                self.body_to_doc(body, span.end.line),
                hardline(),
                text("end"),
            ]),

            Expr::Match {
                subject,
                arms,
                span,
                ..
            } => {
                let any_multiline = arms.iter().any(|a| arm_is_multiline(&a.body));
                let mut parts = vec![text("match "), self.expr_to_doc(subject)];
                let mut arm_docs = Vec::new();
                for arm in arms {
                    arm_docs.push(hardline());
                    arm_docs.push(self.match_arm_to_doc(arm, any_multiline, span.end.line));
                }
                parts.push(indent(2, concat(arm_docs)));
                parts.push(hardline());
                parts.push(text("end"));
                concat(parts)
            }

            Expr::Cond {
                arms,
                else_body,
                span,
                ..
            } => {
                let else_multiline = else_body.as_ref().is_some_and(|b| arm_is_multiline(b));
                let any_multiline =
                    else_multiline || arms.iter().any(|a| arm_is_multiline(&a.body));
                let mut parts = vec![text("cond")];
                let mut arm_docs = Vec::new();
                for arm in arms {
                    arm_docs.push(hardline());
                    arm_docs.push(self.cond_arm_to_doc(arm, any_multiline, span.end.line));
                }
                if let Some(body) = else_body {
                    arm_docs.push(hardline());
                    arm_docs.push(self.else_arm_to_doc(body, any_multiline, span.end.line));
                }
                parts.push(indent(2, concat(arm_docs)));
                parts.push(hardline());
                parts.push(text("end"));
                concat(parts)
            }

            Expr::Receive { arms, span, .. } => {
                let any_multiline = arms.iter().any(|a| arm_is_multiline(&a.body));
                let mut parts = vec![text("receive")];
                let mut arm_docs = Vec::new();
                for arm in arms {
                    arm_docs.push(hardline());
                    arm_docs.push(self.receive_arm_to_doc(arm, any_multiline, span.end.line));
                }
                parts.push(indent(2, concat(arm_docs)));
                parts.push(hardline());
                parts.push(text("end"));
                concat(parts)
            }

            Expr::For {
                pattern,
                iterable,
                body,
                span,
                ..
            } => concat(vec![
                text("for "),
                pattern_to_doc(pattern),
                text(" in "),
                self.expr_to_doc(iterable),
                self.body_to_doc(body, span.end.line),
                hardline(),
                text("end"),
            ]),

            Expr::Loop { body, span, .. } => concat(vec![
                text("loop"),
                self.body_to_doc(body, span.end.line),
                hardline(),
                text("end"),
            ]),

            Expr::While {
                condition,
                body,
                span,
            } => concat(vec![
                text("while "),
                self.expr_to_doc(condition),
                self.body_to_doc(body, span.end.line),
                hardline(),
                text("end"),
            ]),

            Expr::Arena { body, span, .. } => concat(vec![
                text("arena"),
                self.body_to_doc(body, span.end.line),
                hardline(),
                text("end"),
            ]),

            Expr::Closure {
                params,
                return_type,
                body,
                span,
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
                    let body_doc = self.statements_to_doc(body, span.end.line);
                    group(concat(vec![
                        sig,
                        indent(2, concat(vec![line(), body_doc])),
                        line(),
                        text("end"),
                    ]))
                } else {
                    concat(vec![
                        sig,
                        self.body_to_doc(body, span.end.line),
                        hardline(),
                        text("end"),
                    ])
                }
            }

            Expr::ShortClosure { params, body, .. } => {
                let params_doc: Vec<Doc> = params.iter().map(closure_param_to_doc).collect();
                group(concat(vec![
                    intersperse(params_doc, text(", ")),
                    text(" -> "),
                    self.expr_to_doc(body),
                ]))
            }

            Expr::Spawn { expr: inner, .. } => {
                concat(vec![text("spawn("), self.expr_to_doc(inner), text(")")])
            }

            Expr::Await { expr: inner, .. } => {
                concat(vec![text("await "), self.expr_to_doc(inner)])
            }

            Expr::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
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

            Expr::Try { expr: inner, .. } => concat(vec![self.expr_to_doc(inner), text("?")]),

            Expr::StructConstruction {
                type_path, fields, ..
            } => {
                let path_str = type_path.join(".");
                if fields.is_empty() {
                    text(format!("{}{{}}", path_str))
                } else {
                    let field_docs: Vec<Doc> =
                        fields.iter().map(|fi| self.field_init_to_doc(fi)).collect();
                    for fi in fields {
                        self.comments.drain_trailing(fi.span.end.line);
                    }
                    group(concat(vec![
                        text(path_str),
                        text("{"),
                        indent(
                            2,
                            concat(vec![
                                softline(),
                                intersperse(field_docs, concat(vec![text(","), line()])),
                                trailing_comma(),
                            ]),
                        ),
                        softline(),
                        text("}"),
                    ]))
                }
            }

            Expr::EnumConstruction {
                type_path,
                variant,
                data,
                ..
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
                        let field_docs: Vec<Doc> =
                            fields.iter().map(|fi| self.field_init_to_doc(fi)).collect();
                        for fi in fields {
                            self.comments.drain_trailing(fi.span.end.line);
                        }
                        group(concat(vec![
                            text(prefix),
                            text("{"),
                            indent(
                                2,
                                concat(vec![
                                    softline(),
                                    intersperse(field_docs, concat(vec![text(","), line()])),
                                    trailing_comma(),
                                ]),
                            ),
                            softline(),
                            text("}"),
                        ]))
                    }
                }
            }
        }
    }

    /// Formats a parenthesized argument list for a call or method call.
    pub(super) fn call_args_to_doc(&mut self, args: &[Arg]) -> Doc {
        if args.is_empty() {
            text("()")
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

    /// Formats a struct field initializer (`name: value`).
    fn field_init_to_doc(&mut self, fi: &FieldInit) -> Doc {
        concat(vec![
            text(&fi.name),
            text(": "),
            self.expr_to_doc(&fi.value),
        ])
    }

    /// Formats a string literal (single-line or multi-line heredoc).
    fn string_to_doc(&mut self, parts: &[StringPart], multiline: bool) -> Doc {
        if multiline {
            let mut doc_parts = vec![text("\"\"\"")];
            for part in parts {
                match part {
                    StringPart::Literal { value, .. } => {
                        for (i, l) in value.split('\n').enumerate() {
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

    /// Formats a `receive` arm: `pattern = source -> body`.
    pub(super) fn receive_arm_to_doc(
        &mut self,
        arm: &ReceiveArm,
        force_break: bool,
        block_end: u32,
    ) -> Doc {
        let head = concat(vec![
            pattern_to_doc(&arm.pattern),
            text(" = "),
            self.expr_to_doc(&arm.source),
            text(" ->"),
        ]);
        self.arm_body_to_doc(head, &arm.body, force_break, block_end)
    }

    /// Shared formatting for all arm types (match, cond, receive).
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
}
