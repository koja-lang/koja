use crate::doc::*;
use expo_ast::ast::*;

// =========================================================================
// Printer struct -- holds comment cursor for re-attachment
// =========================================================================

pub fn module_to_doc(module: &Module) -> Doc {
    let mut p = Printer::new(&module.comments);
    p.print_module(module)
}

struct Printer<'a> {
    comments: CommentCursor<'a>,
}

impl<'a> Printer<'a> {
    fn new(comments: &'a [Comment]) -> Self {
        Self {
            comments: CommentCursor::new(comments),
        }
    }

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
                let comment_docs = self.comments.drain_before(md.span.start.line);
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

            if let Item::Import(_) = &module.items[i] {
                let run_start = i;
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
                self.comments.drain_before(block_end);

                imports.sort_by_key(|imp| import_sort_key(imp));

                if emitted && (run_start > 0 || after_moduledoc) {
                    parts.push(hardline());
                }

                for imp in &imports {
                    parts.push(import_to_doc(imp));
                    parts.push(hardline());
                }
                emitted = true;
                after_moduledoc = false;
            } else {
                let item = &module.items[i];
                let span = item_span(item);
                let comment_docs = self.comments.drain_before(span.start.line);
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

    // =====================================================================
    // Items
    // =====================================================================

    fn item_to_doc(&mut self, item: &Item) -> Doc {
        match item {
            Item::Import(i) => import_to_doc(i),
            Item::Struct(s) => self.struct_to_doc(s),
            Item::Enum(e) => self.enum_to_doc(e),
            Item::Function(f) => self.function_to_doc(f),
            Item::Impl(i) => self.impl_to_doc(i),
            Item::Constant(c) => self.constant_to_doc(c),
            Item::Shared(s) => shared_to_doc(s),
        }
    }

    // =====================================================================
    // Struct
    // =====================================================================

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
            body.push(self.struct_field_to_doc(field));
        }
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

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

    // =====================================================================
    // Enum
    // =====================================================================

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
            body.push(self.enum_variant_to_doc(variant));
        }
        parts.push(indent(2, concat(body)));
        parts.push(hardline());
        parts.push(text("end"));
        concat(parts)
    }

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

    // =====================================================================
    // Function
    // =====================================================================

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

    fn function_sig_to_doc(&mut self, f: &Function) -> Doc {
        let mut prefix = String::new();
        if f.is_private {
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

    fn param_to_doc(&mut self, p: &Param) -> Doc {
        match p {
            Param::Self_ { .. } => text("self"),
            Param::Regular {
                is_move,
                name,
                type_expr,
                default,
                ..
            } => {
                let mut parts = Vec::new();
                if *is_move {
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

    // =====================================================================
    // Impl
    // =====================================================================

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

    fn impl_member_to_doc(&mut self, member: &ImplMember) -> Doc {
        match member {
            ImplMember::Function(f) => self.function_to_doc(f),
            ImplMember::TypeAlias(ta) => concat(vec![
                text(format!("type {} = ", ta.name)),
                type_expr_to_doc(&ta.type_expr),
            ]),
        }
    }

    // =====================================================================
    // Constant
    // =====================================================================

    fn constant_to_doc(&mut self, c: &Constant) -> Doc {
        concat(vec![text(&c.name), text(" = "), self.expr_to_doc(&c.value)])
    }

    // =====================================================================
    // Expressions
    // =====================================================================

    fn expr_to_doc(&mut self, expr: &Expr) -> Doc {
        match expr {
            Expr::Literal { value, .. } => literal_to_doc(value),
            Expr::Ident { name, .. } => text(name.clone()),
            Expr::Self_ { .. } => text("self"),

            Expr::Binary {
                op, left, right, ..
            } => {
                let op_str = binop_str(op);
                if matches!(op, BinOp::Pipe) {
                    let mut segments = Vec::new();
                    self.collect_pipe_chain(left, &mut segments);
                    self.collect_pipe_chain(right, &mut segments);
                    let mut parts = vec![segments[0].clone()];
                    for seg in &segments[1..] {
                        parts.push(line());
                        parts.push(text("|> "));
                        parts.push(seg.clone());
                    }
                    group(concat(parts))
                } else {
                    group(concat(vec![
                        self.expr_to_doc(left),
                        text(" "),
                        text(op_str),
                        line(),
                        self.expr_to_doc(right),
                    ]))
                }
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
                params, body, span, ..
            } => {
                let params_doc: Vec<Doc> = params.iter().map(closure_param_to_doc).collect();
                let sig = if params.is_empty() {
                    text("fn ")
                } else {
                    concat(vec![
                        text("fn "),
                        intersperse(params_doc, text(", ")),
                        text(" "),
                    ])
                };
                if body.len() == 1 {
                    let body_doc = self.statements_to_doc(body, span.end.line);
                    group(concat(vec![
                        sig,
                        text("->"),
                        indent(2, concat(vec![line(), body_doc])),
                        line(),
                        text("end"),
                    ]))
                } else {
                    concat(vec![
                        sig,
                        text("->"),
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
                    let mut body = Vec::new();
                    for (i, fi) in fields.iter().enumerate() {
                        if i > 0 {
                            body.push(hardline());
                        }
                        body.push(self.field_init_to_doc(fi));
                        body.push(text(","));
                        if let Some(tc) = self.comments.drain_trailing(fi.span.end.line) {
                            body.push(tc);
                        }
                    }
                    concat(vec![
                        text(path_str),
                        text("{"),
                        indent(2, concat(vec![hardline(), concat(body)])),
                        hardline(),
                        text("}"),
                    ])
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
                        let mut body = Vec::new();
                        for (i, fi) in fields.iter().enumerate() {
                            if i > 0 {
                                body.push(hardline());
                            }
                            body.push(self.field_init_to_doc(fi));
                            if i < fields.len() - 1 {
                                body.push(text(","));
                            }
                            if let Some(tc) = self.comments.drain_trailing(fi.span.end.line) {
                                body.push(tc);
                            }
                        }
                        concat(vec![
                            text(prefix),
                            text("{"),
                            indent(2, concat(vec![hardline(), concat(body)])),
                            hardline(),
                            text("}"),
                        ])
                    }
                }
            }
        }
    }

    fn call_args_to_doc(&mut self, args: &[Arg]) -> Doc {
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

    fn field_init_to_doc(&mut self, fi: &FieldInit) -> Doc {
        concat(vec![
            text(&fi.name),
            text(": "),
            self.expr_to_doc(&fi.value),
        ])
    }

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

    // =====================================================================
    // Match / Cond / Receive arms
    // =====================================================================

    fn match_arm_to_doc(&mut self, arm: &MatchArm, force_break: bool, block_end: u32) -> Doc {
        let mut head = vec![pattern_to_doc(&arm.pattern)];
        if let Some(guard) = &arm.guard {
            head.push(text(" when "));
            head.push(self.expr_to_doc(guard));
        }
        head.push(text(" ->"));
        self.arm_body_to_doc(concat(head), &arm.body, force_break, block_end)
    }

    fn cond_arm_to_doc(&mut self, arm: &CondArm, force_break: bool, block_end: u32) -> Doc {
        let head = concat(vec![self.expr_to_doc(&arm.condition), text(" ->")]);
        self.arm_body_to_doc(head, &arm.body, force_break, block_end)
    }

    fn else_arm_to_doc(&mut self, body: &[Statement], force_break: bool, block_end: u32) -> Doc {
        let head = text("else ->");
        self.arm_body_to_doc(head, body, force_break, block_end)
    }

    fn collect_pipe_chain(&mut self, expr: &Expr, segments: &mut Vec<Doc>) {
        if let Expr::Binary {
            op: BinOp::Pipe,
            left,
            right,
            ..
        } = expr
        {
            self.collect_pipe_chain(left, segments);
            self.collect_pipe_chain(right, segments);
        } else {
            segments.push(self.expr_to_doc(expr));
        }
    }

    fn receive_arm_to_doc(&mut self, arm: &ReceiveArm, force_break: bool, block_end: u32) -> Doc {
        let head = concat(vec![
            pattern_to_doc(&arm.pattern),
            text(" = "),
            self.expr_to_doc(&arm.source),
            text(" ->"),
        ]);
        self.arm_body_to_doc(head, &arm.body, force_break, block_end)
    }

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

    // =====================================================================
    // Statements
    // =====================================================================

    fn statement_to_doc(&mut self, stmt: &Statement) -> Doc {
        match stmt {
            Statement::Expr(expr) => self.expr_to_doc(expr),
            Statement::Assignment { target, value, .. } => {
                let target_doc = match target {
                    AssignTarget::LValue(lv) => text(lv.segments.join(".")),
                    AssignTarget::Pattern(pat) => pattern_to_doc(pat),
                };
                let value_doc = self.expr_to_doc(value);
                if expr_contains_block(value) {
                    concat(vec![
                        target_doc,
                        text(" ="),
                        indent(2, concat(vec![hardline(), value_doc])),
                    ])
                } else if matches!(
                    value,
                    Expr::Binary {
                        op: BinOp::Pipe,
                        ..
                    }
                ) {
                    group(concat(vec![
                        target_doc,
                        text(" ="),
                        indent(2, concat(vec![line(), value_doc])),
                    ]))
                } else {
                    group(concat(vec![target_doc, text(" = "), value_doc]))
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

    /// Render a list of statements, draining interleaved comments.
    /// `block_end` is the line of the closing `end` keyword so we know
    /// the upper bound for comments belonging to this block.
    fn statements_to_doc(&mut self, stmts: &[Statement], block_end: u32) -> Doc {
        let mut parts = Vec::new();
        let mut prev_end: u32 = 0;

        for (i, stmt) in stmts.iter().enumerate() {
            let stmt_line = stmt_start_line(stmt);
            let next_line = self.comments.peek_before(stmt_line).unwrap_or(stmt_line);
            let comment_docs = self.comments.drain_before(stmt_line);

            if i > 0 {
                parts.push(hardline());
                if next_line > prev_end + 1 {
                    parts.push(hardline());
                }
            }

            for c in comment_docs {
                parts.push(c);
            }

            parts.push(self.statement_to_doc(stmt));
            let end_line = stmt_end_line(stmt);
            if let Some(tc) = self.comments.drain_trailing(end_line) {
                parts.push(tc);
            }
            prev_end = end_line;
        }

        let trailing = self.comments.drain_before(block_end);
        if !trailing.is_empty() {
            parts.push(hardline());
            for c in trailing {
                parts.push(c);
            }
        }

        concat(parts)
    }

    fn body_to_doc(&mut self, stmts: &[Statement], block_end: u32) -> Doc {
        if stmts.is_empty() {
            // Still drain any comments inside the empty body
            let trailing = self.comments.drain_before(block_end);
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

// =========================================================================
// Comment helpers
// =========================================================================

struct CommentCursor<'a> {
    comments: &'a [Comment],
    pos: usize,
}

impl<'a> CommentCursor<'a> {
    fn new(comments: &'a [Comment]) -> Self {
        Self { comments, pos: 0 }
    }

    fn drain_before(&mut self, line: u32) -> Vec<Doc> {
        let mut docs = Vec::new();
        while self.pos < self.comments.len() && self.comments[self.pos].span.start.line < line {
            let c = &self.comments[self.pos];
            docs.push(comment_doc(&c.text));
            docs.push(hardline());
            self.pos += 1;
        }
        docs
    }

    /// Peek at the line of the next unconsumed comment, if it's before `line`.
    fn peek_before(&self, line: u32) -> Option<u32> {
        if self.pos < self.comments.len() && self.comments[self.pos].span.start.line < line {
            Some(self.comments[self.pos].span.start.line)
        } else {
            None
        }
    }

    /// Drain comments that sit on exactly `line` (trailing comments).
    fn drain_trailing(&mut self, line: u32) -> Option<Doc> {
        let mut docs = Vec::new();
        while self.pos < self.comments.len() && self.comments[self.pos].span.start.line == line {
            let c = &self.comments[self.pos];
            docs.push(comment_doc(&c.text));
            self.pos += 1;
        }
        if docs.is_empty() {
            None
        } else {
            Some(concat(
                docs.into_iter()
                    .map(|d| concat(vec![text(" "), d]))
                    .collect(),
            ))
        }
    }

    fn drain_rest(&mut self) -> Vec<Doc> {
        let mut docs = Vec::new();
        while self.pos < self.comments.len() {
            let c = &self.comments[self.pos];
            docs.push(comment_doc(&c.text));
            docs.push(hardline());
            self.pos += 1;
        }
        docs
    }
}

fn comment_doc(body: &str) -> Doc {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        text("#")
    } else {
        text(format!("# {}", trimmed))
    }
}

// =========================================================================
// Pure helper functions (no comment cursor needed)
// =========================================================================

fn item_span(item: &Item) -> &expo_ast::span::Span {
    match item {
        Item::Constant(c) => &c.span,
        Item::Enum(e) => &e.span,
        Item::Function(f) => &f.span,
        Item::Impl(i) => &i.span,
        Item::Import(i) => &i.span,
        Item::Shared(s) => &s.span,
        Item::Struct(s) => &s.span,
    }
}

fn import_sort_key(imp: &Import) -> String {
    let base = imp.path.join(".");
    match &imp.target {
        ImportTarget::Module => base,
        ImportTarget::Item(name) => format!("{}.{}", base, name),
        ImportTarget::Group(_) => base,
        ImportTarget::Wildcard => format!("{}.*", base),
    }
}

fn import_to_doc(imp: &Import) -> Doc {
    let path_str = imp.path.join(".");
    match &imp.target {
        ImportTarget::Module => text(format!("import {}", path_str)),
        ImportTarget::Item(name) => text(format!("import {}.{}", path_str, name)),
        ImportTarget::Group(names) => {
            let items: Vec<Doc> = names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    if i < names.len() - 1 {
                        concat(vec![text(n.clone()), text(",")])
                    } else {
                        text(n.clone())
                    }
                })
                .collect();
            let fill_items: Vec<Doc> = items
                .into_iter()
                .enumerate()
                .map(|(i, d)| if i > 0 { concat(vec![text(" "), d]) } else { d })
                .collect();
            group(concat(vec![
                text(format!("import {}.{{", path_str)),
                indent(2, concat(vec![softline(), fill(fill_items)])),
                softline(),
                text("}"),
            ]))
        }
        ImportTarget::Wildcard => text(format!("import {}.*", path_str)),
    }
}

fn shared_to_doc(s: &SharedDecl) -> Doc {
    concat(vec![
        text("shared "),
        text(&s.name),
        text(": "),
        type_expr_to_doc(&s.type_expr),
    ])
}

fn annotation_to_doc(ann: &Annotation) -> Doc {
    match &ann.value {
        Some(val) => {
            if val.contains('\n') {
                concat(vec![
                    text(format!("@{} \"\"\"", ann.name)),
                    hardline(),
                    text(val.trim()),
                    hardline(),
                    text("\"\"\""),
                ])
            } else {
                text(format!("@{} \"{}\"", ann.name, val))
            }
        }
        None => text(format!("@{}", ann.name)),
    }
}

fn type_expr_to_doc(ty: &TypeExpr) -> Doc {
    match ty {
        TypeExpr::Named { path, .. } => text(path.join(".")),
        TypeExpr::Generic { path, args, .. } => {
            let args_doc: Vec<Doc> = args.iter().map(type_expr_to_doc).collect();
            concat(vec![
                text(path.join(".")),
                text("<"),
                intersperse(args_doc, text(", ")),
                text(">"),
            ])
        }
        TypeExpr::Ref { inner, .. } => {
            concat(vec![text("ref<"), type_expr_to_doc(inner), text(">")])
        }
        TypeExpr::Tuple { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(type_expr_to_doc).collect();
            concat(vec![text("("), intersperse(elems, text(", ")), text(")")])
        }
        TypeExpr::Unit { .. } => text("()"),
    }
}

fn pattern_to_doc(pat: &Pattern) -> Doc {
    match pat {
        Pattern::Wildcard { .. } => text("_"),
        Pattern::Literal { value, .. } => literal_to_doc(value),
        Pattern::Binding { name, .. } => text(name.clone()),
        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            if type_path.is_empty() {
                text(variant.clone())
            } else {
                text(format!("{}.{}", type_path.join("."), variant))
            }
        }
        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let prefix = if type_path.is_empty() {
                variant.clone()
            } else {
                format!("{}.{}", type_path.join("."), variant)
            };
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![
                text(prefix),
                text("("),
                intersperse(elems, text(", ")),
                text(")"),
            ])
        }
        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            ..
        } => {
            let prefix = if type_path.is_empty() {
                variant.clone()
            } else {
                format!("{}.{}", type_path.join("."), variant)
            };
            let field_docs: Vec<Doc> = fields.iter().map(field_pattern_to_doc).collect();
            group(concat(vec![
                text(prefix),
                text("{"),
                indent(
                    2,
                    concat(vec![
                        softline(),
                        intersperse(field_docs, concat(vec![text(","), line()])),
                    ]),
                ),
                softline(),
                text("}"),
            ]))
        }
        Pattern::Constructor { name, elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![
                text(name.clone()),
                text("("),
                intersperse(elems, text(", ")),
                text(")"),
            ])
        }
        Pattern::Tuple { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![text("("), intersperse(elems, text(", ")), text(")")])
        }
        Pattern::List { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![text("["), intersperse(elems, text(", ")), text("]")])
        }
    }
}

fn field_pattern_to_doc(fp: &FieldPattern) -> Doc {
    match &fp.pattern {
        Some(pat) => concat(vec![text(&fp.name), text(": "), pattern_to_doc(pat)]),
        None => text(&fp.name),
    }
}

fn literal_to_doc(lit: &Literal) -> Doc {
    match lit {
        Literal::Bool(true) => text("true"),
        Literal::Bool(false) => text("false"),
        Literal::Float(s) => text(s.clone()),
        Literal::Int(s) => text(s.clone()),
        Literal::None => text("none"),
        Literal::Unit => text("()"),
    }
}

fn closure_param_to_doc(cp: &ClosureParam) -> Doc {
    match cp {
        ClosureParam::Name { name, .. } => text(name.clone()),
        ClosureParam::Destructured { names, .. } => concat(vec![
            text("("),
            intersperse(names.iter().map(|n| text(n.clone())).collect(), text(", ")),
            text(")"),
        ]),
        ClosureParam::Wildcard { .. } => text("_"),
    }
}

/// Returns true if the expression will render as multiple lines
/// (contains block constructs like closures, if/match, struct literals, etc.).
/// Returns true if the expression contains multi-line constructs
/// (blocks, struct literals, etc.) that warrant breaking after `=`.
fn expr_contains_block(expr: &Expr) -> bool {
    match expr {
        Expr::If { .. }
        | Expr::Match { .. }
        | Expr::Cond { .. }
        | Expr::For { .. }
        | Expr::Loop { .. }
        | Expr::Unless { .. }
        | Expr::While { .. }
        | Expr::Closure { .. }
        | Expr::Receive { .. }
        | Expr::Arena { .. } => true,
        Expr::StructConstruction { fields, .. } => !fields.is_empty(),
        Expr::EnumConstruction { data, .. } => {
            matches!(data, EnumConstructionData::Struct(f) if !f.is_empty())
        }
        Expr::Call { args, .. } => args.iter().any(|a| expr_contains_block(&a.value)),
        Expr::MethodCall { receiver, args, .. } => {
            expr_contains_block(receiver) || args.iter().any(|a| expr_contains_block(&a.value))
        }
        Expr::Binary { right, .. } => expr_contains_block(right),
        Expr::Try { expr, .. } => expr_contains_block(expr),
        Expr::Await { expr, .. } => expr_contains_block(expr),
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            expr_contains_block(condition)
                || expr_contains_block(then_expr)
                || expr_contains_block(else_expr)
        }
        _ => false,
    }
}

fn arm_is_multiline(body: &[Statement]) -> bool {
    if body.len() > 1 {
        return true;
    }
    if body.len() == 1
        && let Statement::Expr(expr) = &body[0]
    {
        return matches!(
            expr,
            Expr::If { .. }
                | Expr::Match { .. }
                | Expr::Cond { .. }
                | Expr::For { .. }
                | Expr::Loop { .. }
                | Expr::Unless { .. }
                | Expr::While { .. }
                | Expr::Closure { .. }
                | Expr::Receive { .. }
                | Expr::Arena { .. }
        );
    }
    false
}

fn binop_str(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::And => "and",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Mod => "%",
        BinOp::Mul => "*",
        BinOp::NotEq => "!=",
        BinOp::Or => "or",
        BinOp::Pipe => "|>",
        BinOp::Sub => "-",
    }
}

fn is_unit_type(ty: &TypeExpr) -> bool {
    matches!(ty, TypeExpr::Unit { .. })
}

fn sig_will_break(f: &Function) -> bool {
    let mut len: usize = 0;
    if f.is_private {
        len += 5;
    }
    len += 3;
    len += f.name.len();
    if !f.type_params.is_empty() {
        len += 1;
        len += f.type_params.join(", ").len();
        len += 1;
    }
    len += 1;
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            len += 2;
        }
        len += param_text_len(p);
    }
    len += 1;
    if let Some(rt) = &f.return_type
        && !is_unit_type(rt)
    {
        len += 4;
        len += type_expr_text_len(rt);
    }
    len > 80
}

fn param_text_len(p: &Param) -> usize {
    match p {
        Param::Self_ { .. } => 4,
        Param::Regular {
            is_move,
            name,
            type_expr,
            default,
            ..
        } => {
            let mut n = 0;
            if *is_move {
                n += 5;
            }
            n += name.len();
            n += 2;
            n += type_expr_text_len(type_expr);
            if let Some(_d) = default {
                n += 3;
                n += 20; // estimate
            }
            n
        }
    }
}

fn type_expr_text_len(ty: &TypeExpr) -> usize {
    match ty {
        TypeExpr::Named { path, .. } => {
            path.iter().map(|s| s.len()).sum::<usize>() + path.len().saturating_sub(1)
        }
        TypeExpr::Generic { path, args, .. } => {
            let path_len: usize =
                path.iter().map(|s| s.len()).sum::<usize>() + path.len().saturating_sub(1);
            let args_len: usize = args.iter().map(type_expr_text_len).sum::<usize>()
                + args.len().saturating_sub(1) * 2;
            path_len + 1 + args_len + 1
        }
        TypeExpr::Ref { inner, .. } => 4 + type_expr_text_len(inner) + 1,
        TypeExpr::Tuple { elements, .. } => {
            let inner: usize = elements.iter().map(type_expr_text_len).sum::<usize>()
                + elements.len().saturating_sub(1) * 2;
            1 + inner + 1
        }
        TypeExpr::Unit { .. } => 2,
    }
}

fn stmt_start_line(stmt: &Statement) -> u32 {
    match stmt {
        Statement::Expr(expr) => expr_start_line(expr),
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => span.start.line,
    }
}

fn stmt_end_line(stmt: &Statement) -> u32 {
    match stmt {
        Statement::Expr(expr) => expr_end_line(expr),
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => span.end.line,
    }
}

fn expr_start_line(expr: &Expr) -> u32 {
    use expo_ast::ast::Expr::*;
    match expr {
        Arena { span, .. }
        | Await { span, .. }
        | Binary { span, .. }
        | Call { span, .. }
        | Closure { span, .. }
        | Cond { span, .. }
        | EnumConstruction { span, .. }
        | FieldAccess { span, .. }
        | For { span, .. }
        | Group { span, .. }
        | Ident { span, .. }
        | If { span, .. }
        | List { span, .. }
        | Literal { span, .. }
        | Loop { span, .. }
        | Match { span, .. }
        | MethodCall { span, .. }
        | Receive { span, .. }
        | Self_ { span, .. }
        | ShortClosure { span, .. }
        | Spawn { span, .. }
        | String { span, .. }
        | StructConstruction { span, .. }
        | Ternary { span, .. }
        | Try { span, .. }
        | Tuple { span, .. }
        | Unary { span, .. }
        | Unless { span, .. }
        | While { span, .. } => span.start.line,
    }
}

fn expr_end_line(expr: &Expr) -> u32 {
    use expo_ast::ast::Expr::*;
    match expr {
        Arena { span, .. }
        | Await { span, .. }
        | Binary { span, .. }
        | Call { span, .. }
        | Closure { span, .. }
        | Cond { span, .. }
        | EnumConstruction { span, .. }
        | FieldAccess { span, .. }
        | For { span, .. }
        | Group { span, .. }
        | Ident { span, .. }
        | If { span, .. }
        | List { span, .. }
        | Literal { span, .. }
        | Loop { span, .. }
        | Match { span, .. }
        | MethodCall { span, .. }
        | Receive { span, .. }
        | Self_ { span, .. }
        | ShortClosure { span, .. }
        | Spawn { span, .. }
        | String { span, .. }
        | StructConstruction { span, .. }
        | Ternary { span, .. }
        | Try { span, .. }
        | Tuple { span, .. }
        | Unary { span, .. }
        | Unless { span, .. }
        | While { span, .. } => span.end.line,
    }
}

fn escape_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
            _ => out.push(c),
        }
    }
    out
}
