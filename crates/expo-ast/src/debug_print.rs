//! Compact tree printer for `--emit-ast` output.
//!
//! Replaces Rust's `{:#?}` pretty-debug with a 2-space-indent tree
//! where each node sits on its own line. The header carries the node
//! kind plus a few inlined leaf fields (`name`, `op`, `visibility`,
//! literal payload, ...), and a trailing `@start_line:start_col-
//! end_line:end_col` span suffix. Children hang below at the next
//! indent level.
//!
//! Single public entry point: [`format_file`].
//!
//! Every enum dispatch is exhaustive (no `_` catch-alls) so adding a
//! new AST variant is a compile error until the printer is updated.

use std::fmt::Write as _;

use crate::ast::{
    AliasDecl, AnnotationValue, Arg, AssignTarget, BinOp, BinarySegment, ClosureParam, CompoundOp,
    CondArm, Constant, EnumConstructionData, EnumDecl, EnumVariant, EnumVariantData, Expr,
    ExprKind, FieldInit, FieldPattern, File, Function, ImplBlock, ImplMember, Item, LValue,
    Literal, MatchArm, Param, PassMode, Pattern, ProtocolDecl, ProtocolMethod, Statement,
    StringPart, StructDecl, StructField, TypeAlias, TypeExpr, TypeParam, UnaryOp, Visibility,
};
use crate::identifier::{AnonymousKind, Resolution, ResolvedType};
use crate::span::Span;

/// Render `file` as a compact indented tree suitable for `--emit-ast`
/// output. Always returns text that ends with `\n`.
pub fn format_file(file: &File) -> String {
    let mut out = String::new();
    Printer::new(&mut out).file(file);
    out
}

struct Printer<'a> {
    out: &'a mut String,
    depth: usize,
}

impl<'a> Printer<'a> {
    fn new(out: &'a mut String) -> Self {
        Self { out, depth: 0 }
    }

    fn indent_buf(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
    }

    /// Emit a label-only line (no span) at the current depth.
    fn line(&mut self, body: &str) {
        self.indent_buf();
        self.out.push_str(body);
        self.out.push('\n');
    }

    /// Emit a header line with a trailing `@span` suffix.
    fn header(&mut self, body: &str, span: Span) {
        self.indent_buf();
        self.out.push_str(body);
        self.out.push(' ');
        self.out.push_str(&format_span(span));
        self.out.push('\n');
    }

    /// Emit `header @span` and run `body` one level deeper.
    fn nested(&mut self, header: &str, span: Span, body: impl FnOnce(&mut Self)) {
        self.header(header, span);
        self.depth += 1;
        body(self);
        self.depth -= 1;
    }

    /// Emit a label-only line and run `body` one level deeper.
    fn section(&mut self, label: &str, body: impl FnOnce(&mut Self)) {
        self.line(label);
        self.depth += 1;
        body(self);
        self.depth -= 1;
    }

    fn file(&mut self, file: &File) {
        let mut header = String::from("File");
        if !file.package.is_empty() {
            let _ = write!(header, " {}", file.package);
        }
        if let Some(path) = &file.path {
            let _ = write!(header, " {:?}", path.display().to_string());
        }
        self.nested(&header, file.span, |p| {
            if !file.comments.is_empty() {
                p.section("comments", |p| {
                    for c in &file.comments {
                        p.header(&format!("Comment {:?}", c.text), c.span);
                    }
                });
            }
            if let Some(body) = &file.body {
                p.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                });
            }
            if !file.items.is_empty() {
                p.section("items", |p| {
                    for item in &file.items {
                        p.item(item);
                    }
                });
            }
        });
    }

    // ---------------------------------------------------------------
    // Items
    // ---------------------------------------------------------------

    fn item(&mut self, item: &Item) {
        match item {
            Item::Alias(alias) => self.alias(alias),
            Item::Constant(c) => self.constant(c),
            Item::Enum(e) => self.enum_decl(e),
            Item::Function(f) => self.function("Function", f),
            Item::Impl(i) => self.impl_block(i),
            Item::Protocol(p) => self.protocol(p),
            Item::Struct(s) => self.struct_decl(s),
            Item::TypeAlias(t) => self.type_alias(t),
        }
    }

    fn alias(&mut self, alias: &AliasDecl) {
        let header = format!("Alias {} as {}", alias.path.join("."), alias.local_name,);
        self.header(&header, alias.span);
    }

    fn constant(&mut self, c: &Constant) {
        let header = format!("Constant {}", c.name);
        self.nested(&header, c.span, |p| {
            p.annotations(&c.annotations);
            if let Some(ty) = &c.type_annotation {
                p.line(&format!("type: {}", type_expr_inline(ty)));
            }
            p.section("value", |p| p.expr(&c.value));
        });
    }

    fn enum_decl(&mut self, e: &EnumDecl) {
        let header = format!("EnumDecl {}{}", e.name, format_type_params(&e.type_params));
        self.nested(&header, e.span, |p| {
            p.annotations(&e.annotations);
            if !e.variants.is_empty() {
                p.section("variants", |p| {
                    for v in &e.variants {
                        p.enum_variant(v);
                    }
                });
            }
            if !e.functions.is_empty() {
                p.section("functions", |p| {
                    for f in &e.functions {
                        p.function("Function", f);
                    }
                });
            }
        });
    }

    fn enum_variant(&mut self, v: &EnumVariant) {
        match &v.data {
            EnumVariantData::Unit => {
                self.header(&format!("EnumVariant {} (Unit)", v.name), v.span);
            }
            EnumVariantData::Tuple(types) => {
                self.nested(&format!("EnumVariant {} (Tuple)", v.name), v.span, |p| {
                    for ty in types {
                        p.line(&format!("type: {}", type_expr_inline(ty)));
                    }
                });
            }
            EnumVariantData::Struct(fields) => {
                self.nested(&format!("EnumVariant {} (Struct)", v.name), v.span, |p| {
                    for f in fields {
                        p.struct_field(f);
                    }
                });
            }
        }
    }

    fn function(&mut self, kind: &str, f: &Function) {
        let header = format!(
            "{} {} ({}){}",
            kind,
            f.name,
            format_visibility(f.visibility),
            format_type_params(&f.type_params),
        );
        self.nested(&header, f.span, |p| {
            p.annotations(&f.annotations);
            if !f.params.is_empty() {
                p.section("params", |p| {
                    for param in &f.params {
                        p.param(param);
                    }
                });
            }
            if let Some(ret) = &f.return_type {
                p.line(&format!("return: {}", type_expr_inline(ret)));
            }
            match &f.body {
                Some(body) => p.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                }),
                None => p.line("body: <none>"),
            }
        });
    }

    fn impl_block(&mut self, i: &ImplBlock) {
        self.nested("ImplBlock", i.span, |p| {
            p.line(&format!("target: {}", type_expr_inline(&i.target)));
            if let Some(trait_expr) = &i.trait_expr {
                p.line(&format!("trait: {}", type_expr_inline(trait_expr)));
            }
            if !i.members.is_empty() {
                p.section("members", |p| {
                    for m in &i.members {
                        p.impl_member(m);
                    }
                });
            }
        });
    }

    fn impl_member(&mut self, m: &ImplMember) {
        match m {
            ImplMember::Function(f) => self.function("Function", f),
            ImplMember::TypeAlias(t) => self.type_alias(t),
        }
    }

    fn protocol(&mut self, p: &ProtocolDecl) {
        let header = format!(
            "ProtocolDecl {}{}",
            p.name,
            format_type_params(&p.type_params),
        );
        self.nested(&header, p.span, |printer| {
            printer.annotations(&p.annotations);
            if !p.methods.is_empty() {
                printer.section("methods", |printer| {
                    for method in &p.methods {
                        printer.protocol_method(method);
                    }
                });
            }
        });
    }

    fn protocol_method(&mut self, m: &ProtocolMethod) {
        let header = format!(
            "ProtocolMethod {}{}",
            m.name,
            format_type_params(&m.type_params)
        );
        self.nested(&header, m.span, |p| {
            p.annotations(&m.annotations);
            if !m.params.is_empty() {
                p.section("params", |p| {
                    for param in &m.params {
                        p.param(param);
                    }
                });
            }
            if let Some(ret) = &m.return_type {
                p.line(&format!("return: {}", type_expr_inline(ret)));
            }
            match &m.body {
                Some(body) => p.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                }),
                None => p.line("body: <required>"),
            }
        });
    }

    fn struct_decl(&mut self, s: &StructDecl) {
        let header = format!(
            "StructDecl {}{}",
            s.name,
            format_type_params(&s.type_params)
        );
        self.nested(&header, s.span, |p| {
            p.annotations(&s.annotations);
            if !s.fields.is_empty() {
                p.section("fields", |p| {
                    for f in &s.fields {
                        p.struct_field(f);
                    }
                });
            }
            if !s.functions.is_empty() {
                p.section("functions", |p| {
                    for f in &s.functions {
                        p.function("Function", f);
                    }
                });
            }
        });
    }

    fn struct_field(&mut self, f: &StructField) {
        let header = format!("{}: {}", f.name, type_expr_inline(&f.type_expr));
        if f.default.is_some() {
            self.nested(&header, f.span, |p| {
                if let Some(expr) = &f.default {
                    p.section("default", |p| p.expr(expr));
                }
            });
        } else {
            self.header(&header, f.span);
        }
    }

    fn type_alias(&mut self, t: &TypeAlias) {
        let header = format!("TypeAlias {}", t.name);
        self.nested(&header, t.span, |p| {
            p.annotations(&t.annotations);
            p.line(&format!("type: {}", type_expr_inline(&t.type_expr)));
        });
    }

    // ---------------------------------------------------------------
    // Statements
    // ---------------------------------------------------------------

    fn statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Expr(expr) => self.expr(expr),
            Statement::Assignment {
                target,
                type_annotation,
                value,
                span,
            } => self.nested("Assignment", *span, |p| {
                p.assign_target(target);
                if let Some(ty) = type_annotation {
                    p.line(&format!("type: {}", type_expr_inline(ty)));
                }
                p.section("value", |p| p.expr(value));
            }),
            Statement::CompoundAssign {
                target,
                op,
                value,
                span,
            } => self.nested(
                &format!("CompoundAssign {}", format_compound_op(*op)),
                *span,
                |p| {
                    p.line(&format!("target: {}", format_lvalue(target)));
                    p.section("value", |p| p.expr(value));
                },
            ),
            Statement::Return { value, span } => match value {
                Some(expr) => self.nested("Return", *span, |p| p.expr(expr)),
                None => self.header("Return", *span),
            },
            Statement::Break { span } => self.header("Break", *span),
        }
    }

    fn assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::LValue(lv) => {
                self.line(&format!("target: {}", format_lvalue(lv)));
            }
            AssignTarget::Pattern(pat) => {
                self.section("target", |p| p.pattern(pat));
            }
        }
    }

    // ---------------------------------------------------------------
    // Expressions
    // ---------------------------------------------------------------

    fn expr(&mut self, expr: &Expr) {
        let head = expr_header(expr);
        let has_children = expr_has_children(&expr.kind);
        if has_children {
            self.nested(&head, expr.span, |p| p.expr_children(&expr.kind));
        } else {
            self.header(&head, expr.span);
        }
    }

    fn expr_children(&mut self, kind: &ExprKind) {
        match kind {
            ExprKind::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::BinaryLiteral { segments } => {
                for seg in segments {
                    self.binary_segment(seg);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.section("callee", |p| p.expr(callee));
                if !args.is_empty() {
                    self.section("args", |p| {
                        for arg in args {
                            p.arg(arg);
                        }
                    });
                }
            }
            ExprKind::Closure {
                params,
                return_type,
                body,
            } => {
                if !params.is_empty() {
                    self.section("params", |p| {
                        for param in params {
                            p.closure_param(param);
                        }
                    });
                }
                if let Some(ret) = return_type {
                    self.line(&format!("return: {}", type_expr_inline(ret)));
                }
                self.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                });
            }
            ExprKind::Cond { arms, else_body } => {
                for arm in arms {
                    self.cond_arm(arm);
                }
                if let Some(body) = else_body {
                    self.section("else", |p| {
                        for stmt in body {
                            p.statement(stmt);
                        }
                    });
                }
            }
            ExprKind::EnumConstruction { data, .. } => match data {
                EnumConstructionData::Unit => {}
                EnumConstructionData::Tuple(values) => {
                    for v in values {
                        self.expr(v);
                    }
                }
                EnumConstructionData::Struct(fields) => {
                    for f in fields {
                        self.field_init(f);
                    }
                }
            },
            ExprKind::FieldAccess { receiver, .. } => {
                self.expr(receiver);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.section("pattern", |p| p.pattern(pattern));
                self.section("iterable", |p| p.expr(iterable));
                self.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                });
            }
            ExprKind::Group { expr } => self.expr(expr),
            ExprKind::Ident { .. } => {}
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.section("condition", |p| p.expr(condition));
                self.section("then", |p| {
                    for stmt in then_body {
                        p.statement(stmt);
                    }
                });
                if let Some(body) = else_body {
                    self.section("else", |p| {
                        for stmt in body {
                            p.statement(stmt);
                        }
                    });
                }
            }
            ExprKind::List { elements } => {
                for e in elements {
                    self.expr(e);
                }
            }
            ExprKind::Literal { .. } => {}
            ExprKind::Loop { body } => {
                for stmt in body {
                    self.statement(stmt);
                }
            }
            ExprKind::Map { entries } => {
                for (key, value) in entries {
                    self.section("entry", |p| {
                        p.section("key", |p| p.expr(key));
                        p.section("value", |p| p.expr(value));
                    });
                }
            }
            ExprKind::Match { subject, arms } => {
                self.section("subject", |p| p.expr(subject));
                for arm in arms {
                    self.match_arm(arm);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.section("receiver", |p| p.expr(receiver));
                if !args.is_empty() {
                    self.section("args", |p| {
                        for arg in args {
                            p.arg(arg);
                        }
                    });
                }
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                for arm in arms {
                    self.match_arm(arm);
                }
                if let Some(timeout) = after_timeout {
                    self.section("after_timeout", |p| p.expr(timeout));
                    self.section("after_body", |p| {
                        for stmt in after_body {
                            p.statement(stmt);
                        }
                    });
                }
            }
            ExprKind::Self_ { .. } => {}
            ExprKind::ShortClosure { params, body } => {
                if !params.is_empty() {
                    self.section("params", |p| {
                        for param in params {
                            p.closure_param(param);
                        }
                    });
                }
                self.section("body", |p| p.expr(body));
            }
            ExprKind::Spawn { expr } => self.expr(expr),
            ExprKind::String { parts, .. } => {
                for part in parts {
                    self.string_part(part);
                }
            }
            ExprKind::StructConstruction { fields, .. } => {
                for f in fields {
                    self.field_init(f);
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.section("condition", |p| p.expr(condition));
                self.section("then", |p| p.expr(then_expr));
                self.section("else", |p| p.expr(else_expr));
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Unless { condition, body } => {
                self.section("condition", |p| p.expr(condition));
                self.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                });
            }
            ExprKind::While { condition, body } => {
                self.section("condition", |p| p.expr(condition));
                self.section("body", |p| {
                    for stmt in body {
                        p.statement(stmt);
                    }
                });
            }
        }
    }

    fn match_arm(&mut self, arm: &MatchArm) {
        self.nested("arm", arm.span, |p| {
            p.section("pattern", |p| p.pattern(&arm.pattern));
            if let Some(guard) = &arm.guard {
                p.section("guard", |p| p.expr(guard));
            }
            p.section("body", |p| {
                for stmt in &arm.body {
                    p.statement(stmt);
                }
            });
        });
    }

    fn cond_arm(&mut self, arm: &CondArm) {
        self.nested("arm", arm.span, |p| {
            p.section("condition", |p| p.expr(&arm.condition));
            p.section("body", |p| {
                for stmt in &arm.body {
                    p.statement(stmt);
                }
            });
        });
    }

    fn arg(&mut self, arg: &Arg) {
        let header = match &arg.name {
            Some(name) => format!("Arg {name}:"),
            None => String::from("Arg"),
        };
        self.nested(&header, arg.span, |p| p.expr(&arg.value));
    }

    fn field_init(&mut self, f: &FieldInit) {
        self.nested(&format!("field {}", f.name), f.span, |p| p.expr(&f.value));
    }

    fn string_part(&mut self, part: &StringPart) {
        match part {
            StringPart::Literal { value, span } => {
                self.header(&format!("literal {value:?}"), *span);
            }
            StringPart::Interpolation { expr, format, span } => {
                let header = match format {
                    Some(fmt) => format!("interpolation fmt={fmt:?}"),
                    None => String::from("interpolation"),
                };
                self.nested(&header, *span, |p| p.expr(expr));
            }
        }
    }

    fn binary_segment(&mut self, seg: &BinarySegment) {
        self.nested("segment", seg.span, |p| {
            p.section("value", |p| p.expr(&seg.value));
            if let Some(size) = &seg.size {
                p.section("size", |p| p.expr(size));
            }
            p.line(&format!("unit: {:?}", seg.unit));
            if let Some(s) = seg.signedness {
                p.line(&format!("signedness: {s:?}"));
            }
            if let Some(e) = seg.endianness {
                p.line(&format!("endianness: {e:?}"));
            }
            if let Some(ann) = &seg.type_ann {
                p.line(&format!("type: {}", type_expr_inline(ann)));
            }
        });
    }

    // ---------------------------------------------------------------
    // Parameters (function + closure)
    // ---------------------------------------------------------------

    fn param(&mut self, param: &Param) {
        match param {
            Param::Self_ { mode, span, .. } => {
                self.header(&format!("Self ({})", format_pass_mode(*mode)), *span);
            }
            Param::Regular {
                mode,
                name,
                type_expr,
                default,
                span,
                ..
            } => {
                let header = format!(
                    "Regular {}: {} ({})",
                    name,
                    type_expr_inline(type_expr),
                    format_pass_mode(*mode),
                );
                if default.is_some() {
                    self.nested(&header, *span, |p| {
                        if let Some(def) = default {
                            p.section("default", |p| p.expr(def));
                        }
                    });
                } else {
                    self.header(&header, *span);
                }
            }
        }
    }

    fn closure_param(&mut self, param: &ClosureParam) {
        match param {
            ClosureParam::Name {
                mode,
                name,
                span,
                type_expr,
                ..
            } => {
                let mut header = format!("Name {name}");
                if let Some(ty) = type_expr {
                    let _ = write!(header, ": {}", type_expr_inline(ty));
                }
                let _ = write!(header, " ({})", format_pass_mode(*mode));
                self.header(&header, *span);
            }
            ClosureParam::Destructured { names, span } => {
                self.header(&format!("Destructured ({})", names.join(", ")), *span);
            }
            ClosureParam::Wildcard { span } => self.header("Wildcard", *span),
        }
    }

    // ---------------------------------------------------------------
    // Patterns
    // ---------------------------------------------------------------

    fn pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Wildcard { span } => self.header("Wildcard", *span),
            Pattern::Literal { value, span, .. } => {
                self.header(&format!("Literal {}", format_literal(value)), *span);
            }
            Pattern::Binary { segments, span } => {
                self.nested("Binary", *span, |p| {
                    for seg in segments {
                        p.binary_segment(seg);
                    }
                });
            }
            Pattern::Binding { name, span, .. } => {
                self.header(&format!("Binding {name}"), *span);
            }
            Pattern::EnumUnit {
                type_path,
                variant,
                span,
                resolved_type,
            } => {
                let header =
                    enum_pattern_header("EnumUnit", type_path, variant, resolved_type.as_ref());
                self.header(&header, *span);
            }
            Pattern::EnumTuple {
                type_path,
                variant,
                elements,
                span,
                resolved_type,
            } => {
                let header =
                    enum_pattern_header("EnumTuple", type_path, variant, resolved_type.as_ref());
                self.nested(&header, *span, |p| {
                    for e in elements {
                        p.pattern(e);
                    }
                });
            }
            Pattern::EnumStruct {
                type_path,
                variant,
                fields,
                span,
                resolved_type,
            } => {
                let header =
                    enum_pattern_header("EnumStruct", type_path, variant, resolved_type.as_ref());
                self.nested(&header, *span, |p| {
                    for f in fields {
                        p.field_pattern(f);
                    }
                });
            }
            Pattern::Constructor {
                name,
                elements,
                span,
                resolved_type,
            } => {
                let mut header = format!("Constructor {name}");
                if let Some(ty) = resolved_type {
                    let _ = write!(header, " -> {ty}");
                }
                self.nested(&header, *span, |p| {
                    for e in elements {
                        p.pattern(e);
                    }
                });
            }
            Pattern::Struct {
                type_path,
                fields,
                span,
                resolved_type,
            } => {
                let mut header = format!("Struct {}", type_path.join("."));
                if let Some(ty) = resolved_type {
                    let _ = write!(header, " -> {ty}");
                }
                self.nested(&header, *span, |p| {
                    for f in fields {
                        p.field_pattern(f);
                    }
                });
            }
            Pattern::TypedBinding {
                name,
                type_expr,
                span,
                ..
            } => {
                self.header(
                    &format!("TypedBinding {name}: {}", type_expr_inline(type_expr)),
                    *span,
                );
            }
            Pattern::List { elements, span } => {
                self.nested("List", *span, |p| {
                    for e in elements {
                        p.pattern(e);
                    }
                });
            }
            Pattern::Or { patterns, span } => {
                self.nested("Or", *span, |p| {
                    for pat in patterns {
                        p.pattern(pat);
                    }
                });
            }
        }
    }

    fn field_pattern(&mut self, f: &FieldPattern) {
        self.nested(&format!("field {}", f.name), f.span, |p| {
            p.pattern(&f.pattern)
        });
    }

    // ---------------------------------------------------------------
    // Annotations
    // ---------------------------------------------------------------

    fn annotations(&mut self, annotations: &[crate::ast::Annotation]) {
        if annotations.is_empty() {
            return;
        }
        self.section("annotations", |p| {
            for a in annotations {
                let header = match &a.value {
                    None => format!("@{}", a.name),
                    Some(AnnotationValue::String(s)) => format!("@{} {s:?}", a.name),
                    Some(AnnotationValue::False) => format!("@{} false", a.name),
                };
                p.header(&header, a.span);
            }
        });
    }
}

// -------------------------------------------------------------------
// Header helpers: pure functions producing the single-line label for
// each node. Keeping these separate from Printer keeps the walker
// small and makes header shape easy to eyeball.
// -------------------------------------------------------------------

fn expr_header(expr: &Expr) -> String {
    let mut out = match &expr.kind {
        ExprKind::Binary { op, .. } => format!("Binary {}", format_bin_op(*op)),
        ExprKind::BinaryLiteral { .. } => String::from("BinaryLiteral"),
        ExprKind::Call { .. } => String::from("Call"),
        ExprKind::Closure { .. } => String::from("Closure"),
        ExprKind::Cond { .. } => String::from("Cond"),
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => format!(
            "EnumConstruction {}.{} ({})",
            type_path.join("."),
            variant,
            enum_ctor_data_label(data),
        ),
        ExprKind::FieldAccess { field, .. } => format!("FieldAccess .{field}"),
        ExprKind::For { .. } => String::from("For"),
        ExprKind::Group { .. } => String::from("Group"),
        ExprKind::Ident { name, resolution } => match resolution {
            Resolution::Global(id) => format!("Ident {name} -> {id}"),
            Resolution::Local(local_id) => format!("Ident {name} -> local {local_id}"),
            Resolution::TypeParam { owner, index } => {
                format!("Ident {name} -> type param of {owner} #{index}")
            }
            Resolution::Unresolved => format!("Ident {name}"),
        },
        ExprKind::If { .. } => String::from("If"),
        ExprKind::List { elements } => format!("List ({} elems)", elements.len()),
        ExprKind::Literal { value } => format!("Literal {}", format_literal(value)),
        ExprKind::Loop { .. } => String::from("Loop"),
        ExprKind::Map { entries } => format!("Map ({} entries)", entries.len()),
        ExprKind::Match { .. } => String::from("Match"),
        ExprKind::MethodCall { method, .. } => format!("MethodCall .{method}"),
        ExprKind::Receive { .. } => String::from("Receive"),
        ExprKind::Self_ { .. } => String::from("Self"),
        ExprKind::ShortClosure { .. } => String::from("ShortClosure"),
        ExprKind::Spawn { .. } => String::from("Spawn"),
        ExprKind::String { multiline, .. } => {
            if *multiline {
                String::from("String (multiline)")
            } else {
                String::from("String")
            }
        }
        ExprKind::StructConstruction { type_path, .. } => {
            format!("StructConstruction {}", type_path.join("."))
        }
        ExprKind::Ternary { .. } => String::from("Ternary"),
        ExprKind::Unary { op, .. } => format!("Unary {}", format_unary_op(*op)),
        ExprKind::Unless { .. } => String::from("Unless"),
        ExprKind::While { .. } => String::from("While"),
    };
    if let Some(ty) = &expr.resolved_type {
        let _ = write!(out, " : {}", ty.display());
    }
    if !matches!(expr.resolution, ResolvedType::Unresolved) {
        let _ = write!(out, " ~> {}", format_resolved_type(&expr.resolution));
    }
    out
}

/// Compact rendering of a [`ResolvedType`] for `--emit-ast` output.
/// `Named` leaves show the head `<id>`; generics show `<id><arg, ...>`
/// recursively. Anonymous function types render as
/// `fn (T, U) -> R`. Unresolved heads render as `?` so partial
/// resolution states are visible during development.
fn format_resolved_type(ty: &ResolvedType) -> String {
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            let rendered_params: Vec<String> =
                params.iter().map(|p| format_resolved_type(&p.ty)).collect();
            format!(
                "fn ({}) -> {}",
                rendered_params.join(", "),
                format_resolved_type(ret),
            )
        }
        ResolvedType::Named {
            resolution,
            type_args,
        } => {
            let head = match resolution {
                Resolution::Global(id) => id.to_string(),
                Resolution::Local(local_id) => format!("local {local_id}"),
                Resolution::TypeParam { owner, index } => format!("typeparam {owner}#{index}"),
                Resolution::Unresolved => String::from("?"),
            };
            if type_args.is_empty() {
                head
            } else {
                let args: Vec<String> = type_args.iter().map(format_resolved_type).collect();
                format!("{head}<{}>", args.join(", "))
            }
        }
        ResolvedType::Unresolved => String::from("?"),
    }
}

fn expr_has_children(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::Binary { .. }
        | ExprKind::BinaryLiteral { .. }
        | ExprKind::Call { .. }
        | ExprKind::Closure { .. }
        | ExprKind::Cond { .. }
        | ExprKind::FieldAccess { .. }
        | ExprKind::For { .. }
        | ExprKind::Group { .. }
        | ExprKind::If { .. }
        | ExprKind::Loop { .. }
        | ExprKind::Match { .. }
        | ExprKind::MethodCall { .. }
        | ExprKind::Receive { .. }
        | ExprKind::ShortClosure { .. }
        | ExprKind::Spawn { .. }
        | ExprKind::Ternary { .. }
        | ExprKind::Unary { .. }
        | ExprKind::Unless { .. }
        | ExprKind::While { .. } => true,
        ExprKind::EnumConstruction { data, .. } => !matches!(data, EnumConstructionData::Unit),
        ExprKind::List { elements } => !elements.is_empty(),
        ExprKind::Map { entries } => !entries.is_empty(),
        ExprKind::String { parts, .. } => !parts.is_empty(),
        ExprKind::StructConstruction { fields, .. } => !fields.is_empty(),
        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => false,
    }
}

fn enum_ctor_data_label(data: &EnumConstructionData) -> &'static str {
    match data {
        EnumConstructionData::Unit => "Unit",
        EnumConstructionData::Tuple(_) => "Tuple",
        EnumConstructionData::Struct(_) => "Struct",
    }
}

fn enum_pattern_header(
    kind: &str,
    type_path: &[String],
    variant: &str,
    resolved_type: Option<&crate::identifier::TypeIdentifier>,
) -> String {
    let mut header = format!("{kind} {}.{variant}", type_path.join("."));
    if let Some(ty) = resolved_type {
        let _ = write!(header, " -> {ty}");
    }
    header
}

// -------------------------------------------------------------------
// Scalar formatters
// -------------------------------------------------------------------

fn format_span(span: Span) -> String {
    format!("@{span}")
}

fn format_bin_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "Add",
        BinOp::And => "And",
        BinOp::Concat => "Concat",
        BinOp::Div => "Div",
        BinOp::Eq => "Eq",
        BinOp::Gt => "Gt",
        BinOp::GtEq => "GtEq",
        BinOp::Lt => "Lt",
        BinOp::LtEq => "LtEq",
        BinOp::Mod => "Mod",
        BinOp::Mul => "Mul",
        BinOp::NotEq => "NotEq",
        BinOp::Or => "Or",
        BinOp::Sub => "Sub",
    }
}

fn format_unary_op(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "Neg",
        UnaryOp::Not => "Not",
    }
}

fn format_pass_mode(mode: PassMode) -> &'static str {
    match mode {
        PassMode::Borrow => "Borrow",
        PassMode::Copy => "Copy",
        PassMode::Move => "Move",
    }
}

fn format_visibility(v: Visibility) -> &'static str {
    match v {
        Visibility::Private => "Private",
        Visibility::Public => "Public",
    }
}

fn format_compound_op(op: CompoundOp) -> &'static str {
    match op {
        CompoundOp::Add => "Add",
        CompoundOp::Div => "Div",
        CompoundOp::Mul => "Mul",
        CompoundOp::Sub => "Sub",
    }
}

fn format_literal(lit: &Literal) -> String {
    match lit {
        Literal::Bool(b) => format!("Bool {b}"),
        Literal::Float(s) => format!("Float {s}"),
        Literal::Int(s) => format!("Int {s}"),
        Literal::String(s) => format!("String {s:?}"),
        Literal::Unit => String::from("Unit"),
    }
}

fn format_lvalue(lv: &LValue) -> String {
    lv.segments.join(".")
}

fn format_type_params(params: &[TypeParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = params
        .iter()
        .map(|p| {
            if p.bounds.is_empty() {
                p.name.clone()
            } else {
                format!("{}: {}", p.name, p.bounds.join(" & "))
            }
        })
        .collect();
    format!("<{}>", parts.join(", "))
}

/// Single-line, source-like rendering of a [`TypeExpr`] suitable for
/// inlining on a header. Retains the AST kind tag on the outermost
/// level (`Named`, `Generic`, ...) but renders inner type arguments
/// without the tag so nested generics stay readable.
fn type_expr_inline(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named { path, .. } => format!("Named {}", path.join(".")),
        TypeExpr::Generic { path, args, .. } => format!(
            "Generic {}<{}>",
            path.join("."),
            args.iter()
                .map(type_expr_brief)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        TypeExpr::Unit { .. } => String::from("Unit"),
        TypeExpr::Self_ { .. } => String::from("Self"),
        TypeExpr::Function {
            params,
            param_modes,
            return_type,
            ..
        } => format!(
            "fn({}) -> {}",
            format_fn_params(params, param_modes),
            type_expr_brief(return_type),
        ),
        TypeExpr::Union { types, .. } => format!(
            "Union {}",
            types
                .iter()
                .map(type_expr_brief)
                .collect::<Vec<_>>()
                .join(" | "),
        ),
    }
}

/// Compact source-like rendering with no AST kind tag. Used when a
/// `TypeExpr` appears inside a `Generic` / `Function` / `Union`
/// argument where context already disambiguates.
fn type_expr_brief(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named { path, .. } => path.join("."),
        TypeExpr::Generic { path, args, .. } => format!(
            "{}<{}>",
            path.join("."),
            args.iter()
                .map(type_expr_brief)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        TypeExpr::Unit { .. } => String::from("()"),
        TypeExpr::Self_ { .. } => String::from("Self"),
        TypeExpr::Function {
            params,
            param_modes,
            return_type,
            ..
        } => format!(
            "fn({}) -> {}",
            format_fn_params(params, param_modes),
            type_expr_brief(return_type),
        ),
        TypeExpr::Union { types, .. } => types
            .iter()
            .map(type_expr_brief)
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

fn format_fn_params(params: &[TypeExpr], modes: &[PassMode]) -> String {
    params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mode_prefix = match modes.get(i) {
                Some(PassMode::Move) => "move ",
                Some(PassMode::Copy) => "copy ",
                _ => "",
            };
            format!("{mode_prefix}{}", type_expr_brief(p))
        })
        .collect::<Vec<_>>()
        .join(", ")
}
