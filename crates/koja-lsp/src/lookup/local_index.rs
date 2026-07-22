//! Per-document index of local bindings.
//!
//! Walks every function body in the active file and records each
//! local's [`LocalId`] alongside the surface-syntax name and defining
//! span. Used by hover and go-to-definition to resolve
//! `Resolution::Local(id)` hits without re-walking the AST every
//! lookup.

use std::collections::HashMap;
use std::path::Path;

use koja_ast::ast::*;
use koja_ast::identifier::{LocalId, ResolvedType};
use koja_ast::span::Span;
use koja_parser::ParsedProgram;

/// One entry per local binding: where it was declared and what type
/// the resolver stamped on its initializer (when available).
#[derive(Clone, Debug)]
pub(crate) struct LocalInfo {
    pub name: String,
    pub span: Span,
    pub ty: Option<ResolvedType>,
}

/// Index keyed by [`LocalId`].
#[derive(Default, Debug)]
pub(crate) struct LocalIndex {
    by_id: HashMap<u32, LocalInfo>,
}

impl LocalIndex {
    /// Walk every function body in the active file and collect every
    /// local-id-bearing binding (function params, `self`, locals,
    /// pattern bindings, closure params).
    pub(crate) fn build(parsed: &ParsedProgram, active_path: &Path) -> Self {
        let mut idx = Self::default();
        let Some(file) = parsed.get(active_path) else {
            return idx;
        };
        for item in &file.ast.items {
            match item {
                Item::Function(f) => idx.walk_function(f),
                Item::Impl(imp) => {
                    for m in &imp.members {
                        if let ImplMember::Function(f) = m {
                            idx.walk_function(f);
                        }
                    }
                }
                Item::Protocol(p) => {
                    for m in &p.methods {
                        if let Some(body) = &m.body {
                            for param in &m.params {
                                idx.record_param(param);
                            }
                            idx.walk_body(body);
                        }
                    }
                }
                Item::Struct(s) => {
                    for f in &s.functions {
                        idx.walk_function(f);
                    }
                }
                Item::Enum(e) => {
                    for f in &e.functions {
                        idx.walk_function(f);
                    }
                }
                _ => {}
            }
        }
        idx
    }

    pub(crate) fn get(&self, id: LocalId) -> Option<&LocalInfo> {
        self.by_id.get(&id.as_u32())
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &LocalInfo> {
        self.by_id.values()
    }

    fn walk_function(&mut self, f: &Function) {
        for param in &f.params {
            self.record_param(param);
        }
        if let Some(body) = &f.body {
            self.walk_body(body);
        }
    }

    fn record_param(&mut self, param: &Param) {
        match param {
            Param::Regular {
                name,
                local_id: Some(id),
                span,
                ..
            } => self.insert(
                *id,
                LocalInfo {
                    name: name.clone(),
                    span: *span,
                    ty: None,
                },
            ),
            Param::Self_ {
                local_id: Some(id),
                span,
                ..
            } => self.insert(
                *id,
                LocalInfo {
                    name: "self".to_string(),
                    span: *span,
                    ty: None,
                },
            ),
            _ => {}
        }
    }

    fn walk_body(&mut self, body: &[Statement]) {
        for stmt in body {
            self.walk_stmt(stmt);
        }
    }

    fn walk_stmt(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Expr(expr) => self.walk_expr(expr),
            Statement::Assignment {
                target,
                value,
                type_annotation: _,
                ..
            } => {
                if let Some(id) = target.local_id
                    && target.segments.len() == 1
                {
                    self.insert(
                        id,
                        LocalInfo {
                            name: target.segments[0].clone(),
                            span: target.span,
                            ty: Some(value.resolution.clone()),
                        },
                    );
                }
                self.walk_expr(value);
            }
            Statement::CompoundAssign { value, .. } => self.walk_expr(value),
            Statement::Destructure { pattern, value, .. } => {
                self.walk_pattern(pattern);
                self.walk_expr(value);
            }
            Statement::Return {
                value: Some(expr), ..
            } => self.walk_expr(expr),
            _ => {}
        }
    }

    fn walk_pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Binding {
                local_id: Some(id),
                name,
                span,
            } => self.insert(
                *id,
                LocalInfo {
                    name: name.clone(),
                    span: *span,
                    ty: None,
                },
            ),
            Pattern::TypedBinding {
                local_id: Some(id),
                name,
                span,
                resolved_type,
                ..
            } => self.insert(
                *id,
                LocalInfo {
                    name: name.clone(),
                    span: *span,
                    ty: resolved_type.clone(),
                },
            ),
            Pattern::EnumTuple { elements, .. }
            | Pattern::Constructor { elements, .. }
            | Pattern::Tuple { elements, .. } => {
                for sub in elements {
                    self.walk_pattern(sub);
                }
            }
            Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
                for fp in fields {
                    self.walk_pattern(&fp.pattern);
                }
            }
            Pattern::List { elements, .. }
            | Pattern::Or {
                patterns: elements, ..
            } => {
                for sub in elements {
                    self.walk_pattern(sub);
                }
            }
            _ => {}
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args, .. } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.walk_expr(receiver);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::FieldAccess { receiver, .. } => self.walk_expr(receiver),
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.walk_expr(condition);
                self.walk_body(then_body);
                if let Some(eb) = else_body {
                    self.walk_body(eb);
                }
            }
            ExprKind::Match { subject, arms } => {
                self.walk_expr(subject);
                for arm in arms {
                    self.walk_pattern(&arm.pattern);
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_body(&arm.body);
                }
            }
            ExprKind::Cond { arms, else_body } => {
                for arm in arms {
                    self.walk_expr(&arm.condition);
                    self.walk_body(&arm.body);
                }
                if let Some(eb) = else_body {
                    self.walk_body(eb);
                }
            }
            ExprKind::Group { expr: inner } => self.walk_expr(inner),
            ExprKind::While { condition, body } => {
                self.walk_expr(condition);
                self.walk_body(body);
            }
            ExprKind::Loop { body } => self.walk_body(body),
            ExprKind::Closure { params, body, .. } => {
                for p in params {
                    self.record_closure_param(p);
                }
                self.walk_body(body);
            }
            ExprKind::ShortClosure { params, body } => {
                for p in params {
                    self.record_closure_param(p);
                }
                self.walk_expr(body);
            }
            ExprKind::Unless { condition, body } => {
                self.walk_expr(condition);
                self.walk_body(body);
            }
            ExprKind::List { elements } => {
                for e in elements {
                    self.walk_expr(e);
                }
            }
            ExprKind::Map { entries } => {
                for (k, v) in entries {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::Spawn { expr: inner } => self.walk_expr(inner),
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                for arm in arms {
                    self.walk_pattern(&arm.pattern);
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_body(&arm.body);
                }
                if let Some(t) = after_timeout {
                    self.walk_expr(t);
                }
                self.walk_body(after_body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.walk_pattern(pattern);
                self.walk_expr(iterable);
                self.walk_body(body);
            }
            ExprKind::String { parts, .. } => {
                for part in parts {
                    if let StringPart::Interpolation { expr, .. } = part {
                        self.walk_expr(expr);
                    }
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.walk_expr(condition);
                self.walk_expr(then_expr);
                self.walk_expr(else_expr);
            }
            ExprKind::Tuple { elements } => {
                for e in elements {
                    self.walk_expr(e);
                }
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::BinaryLiteral { segments } => {
                for seg in segments {
                    self.walk_expr(&seg.value);
                    if let Some(s) = &seg.size {
                        self.walk_expr(s);
                    }
                }
            }
            ExprKind::StructConstruction { fields, .. } => {
                for f in fields {
                    self.walk_expr(&f.value);
                }
            }
            ExprKind::EnumConstruction { data, .. } => match data {
                EnumConstructionData::Tuple(args) => {
                    for a in args {
                        self.walk_expr(a);
                    }
                }
                EnumConstructionData::Struct(fields) => {
                    for f in fields {
                        self.walk_expr(&f.value);
                    }
                }
                EnumConstructionData::Unit => {}
            },
            ExprKind::Literal { .. } | ExprKind::Self_ { .. } | ExprKind::Ident { .. } => {}
        }
    }

    fn record_closure_param(&mut self, param: &ClosureParam) {
        if let ClosureParam::Name {
            local_id: Some(id),
            name,
            span,
            ..
        } = param
        {
            self.insert(
                *id,
                LocalInfo {
                    name: name.clone(),
                    span: *span,
                    ty: None,
                },
            );
        }
    }

    fn insert(&mut self, id: LocalId, info: LocalInfo) {
        self.by_id.insert(id.as_u32(), info);
    }
}
