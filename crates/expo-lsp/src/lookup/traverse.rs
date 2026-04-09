//! AST traversal helpers for finding symbols at a cursor position.
//!
//! Recursively walks expressions and statements to locate the innermost
//! symbol that contains the given cursor position.

use expo_ast::ast::*;
use expo_ast::span::Span;
use expo_typecheck::context::TypeContext;

use super::span::span_contains;
use super::{SymbolInfo, classify_name};

/// Attempts to match a function name identifier at the cursor position.
///
/// Accounts for the `fn ` keyword prefix when calculating the identifier's
/// column range.
pub(crate) fn find_in_ident_at_name(
    name: &str,
    span: &Span,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    if span.start.line != line {
        return None;
    }
    let name_start = span.start.column;
    let fn_keyword_len = if name_start >= 4 { 3 } else { 0 };
    let ident_start = name_start + fn_keyword_len;
    let ident_end = ident_start + name.len() as u32;

    if col >= ident_start && col <= ident_end {
        return classify_name(name, ctx);
    }
    None
}

/// Searches function parameters for type annotations at the cursor position.
pub(crate) fn find_in_params(
    params: &[Param],
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    for param in params {
        if let Param::Regular { type_expr, .. } = param
            && let Some(info) = find_in_type_expr(type_expr, line, col, ctx)
        {
            return Some(info);
        }
    }
    None
}

/// Searches a statement for a symbol at the cursor position by
/// delegating to expression and type expression traversal.
pub(crate) fn find_in_statement(
    stmt: &Statement,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    match stmt {
        Statement::Expr(expr) => find_in_expr(expr, line, col, ctx),
        Statement::Assignment {
            type_annotation,
            value,
            ..
        } => {
            if let Some(te) = type_annotation
                && let Some(info) = find_in_type_expr(te, line, col, ctx)
            {
                return Some(info);
            }
            find_in_expr(value, line, col, ctx)
        }
        Statement::CompoundAssign { value, .. } => find_in_expr(value, line, col, ctx),
        Statement::Return {
            value: Some(expr), ..
        } => find_in_expr(expr, line, col, ctx),
        _ => None,
    }
}

/// Searches a statement body (slice) for a symbol at the cursor position.
fn find_in_body(body: &[Statement], line: u32, col: u32, ctx: &TypeContext) -> Option<SymbolInfo> {
    body.iter()
        .find_map(|stmt| find_in_statement(stmt, line, col, ctx))
}

/// Searches a type expression for a symbol at the cursor position,
/// resolving named types and generic base types.
pub(crate) fn find_in_type_expr(
    type_expr: &TypeExpr,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    match type_expr {
        TypeExpr::Named { path, span } => {
            if span_contains(span, line, col) {
                let name = path.last()?;
                return classify_name(name, ctx);
            }
        }
        TypeExpr::Generic { path, args, span } => {
            if span_contains(span, line, col) {
                for arg in args {
                    if let Some(info) = find_in_type_expr(arg, line, col, ctx) {
                        return Some(info);
                    }
                }
                let name = path.last()?;
                return classify_name(name, ctx);
            }
        }
        TypeExpr::Union { types, span } => {
            if span_contains(span, line, col) {
                for t in types {
                    if let Some(info) = find_in_type_expr(t, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                for p in params {
                    if let Some(info) = find_in_type_expr(p, line, col, ctx) {
                        return Some(info);
                    }
                }
                if let Some(info) = find_in_type_expr(return_type, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        TypeExpr::Unit { .. } | TypeExpr::Self_ { .. } => {}
    }
    None
}

/// Recursively searches a match pattern for a symbol at the cursor
/// position, resolving type names and enum paths.
fn find_in_pattern(pat: &Pattern, line: u32, col: u32, ctx: &TypeContext) -> Option<SymbolInfo> {
    match pat {
        Pattern::TypedBinding {
            type_expr, span, ..
        } => {
            if span_contains(span, line, col) {
                return find_in_type_expr(type_expr, line, col, ctx);
            }
        }
        Pattern::EnumUnit {
            type_path, span, ..
        } => {
            if span_contains(span, line, col) {
                let name = type_path.first()?;
                return classify_name(name, ctx);
            }
        }
        Pattern::EnumTuple {
            type_path,
            elements,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                for sub in elements {
                    if let Some(info) = find_in_pattern(sub, line, col, ctx) {
                        return Some(info);
                    }
                }
                let name = type_path.first()?;
                return classify_name(name, ctx);
            }
        }
        Pattern::EnumStruct {
            type_path,
            fields,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                for fp in fields {
                    if let Some(sub) = &fp.pattern
                        && let Some(info) = find_in_pattern(sub, line, col, ctx)
                    {
                        return Some(info);
                    }
                }
                let name = type_path.first()?;
                return classify_name(name, ctx);
            }
        }
        Pattern::Constructor {
            name,
            elements,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                for sub in elements {
                    if let Some(info) = find_in_pattern(sub, line, col, ctx) {
                        return Some(info);
                    }
                }
                return classify_name(name, ctx);
            }
        }
        Pattern::List { elements, span } => {
            if span_contains(span, line, col) {
                for sub in elements {
                    if let Some(info) = find_in_pattern(sub, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        Pattern::Binary { segments, span } => {
            if span_contains(span, line, col) {
                for seg in segments {
                    if let Some(info) = find_in_expr(&seg.value, line, col, ctx) {
                        return Some(info);
                    }
                    if let Some(sz) = &seg.size
                        && let Some(info) = find_in_expr(sz, line, col, ctx)
                    {
                        return Some(info);
                    }
                }
            }
        }
        Pattern::Or { patterns, span } => {
            if span_contains(span, line, col) {
                for sub in patterns {
                    if let Some(info) = find_in_pattern(sub, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Binding { .. } => {}
    }
    None
}

/// Recursively searches an expression tree for a symbol at the cursor
/// position, descending into sub-expressions and statement bodies.
fn find_in_expr(expr: &Expr, line: u32, col: u32, ctx: &TypeContext) -> Option<SymbolInfo> {
    match &expr.kind {
        ExprKind::Ident { name } => {
            if span_contains(&expr.span, line, col) {
                let mut info = classify_name(name, ctx);
                if let Some(SymbolInfo::Variable { type_display, .. }) = &mut info {
                    *type_display = expr.resolved_type.as_ref().map(|ty| ty.display());
                }
                return info;
            }
        }
        ExprKind::Call { callee, args, .. } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(callee, line, col, ctx) {
                    return Some(info);
                }
                for arg in args {
                    if let Some(info) = find_in_expr(&arg.value, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(receiver, line, col, ctx) {
                    return Some(info);
                }
                if let ExprKind::Ident { .. } = &receiver.kind {
                    let method_start = receiver.span.end.column + 2;
                    let method_end = method_start + method.len() as u32;
                    if line == receiver.span.end.line
                        && col >= method_start
                        && col <= method_end
                        && let Some(mangled) = resolve_method_name(receiver, method, ctx)
                    {
                        return Some(SymbolInfo::Function { name: mangled });
                    }
                } else if let Some(mangled) = resolve_method_name(receiver, method, ctx)
                    && cursor_on_method(receiver, method, &expr.span, line, col)
                {
                    return Some(SymbolInfo::Function { name: mangled });
                }
                for arg in args {
                    if let Some(info) = find_in_expr(&arg.value, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::FieldAccess { receiver, .. } => {
            if span_contains(&expr.span, line, col)
                && let Some(info) = find_in_expr(receiver, line, col, ctx)
            {
                return Some(info);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(left, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_expr(right, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(then_body, line, col, ctx) {
                    return Some(info);
                }
                if let Some(else_stmts) = else_body
                    && let Some(info) = find_in_body(else_stmts, line, col, ctx)
                {
                    return Some(info);
                }
            }
        }
        ExprKind::Match { subject, arms } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(subject, line, col, ctx) {
                    return Some(info);
                }
                for arm in arms {
                    if let Some(info) = find_in_pattern(&arm.pattern, line, col, ctx) {
                        return Some(info);
                    }
                    if let Some(guard) = &arm.guard
                        && let Some(info) = find_in_expr(guard, line, col, ctx)
                    {
                        return Some(info);
                    }
                    if let Some(info) = find_in_body(&arm.body, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::Cond { arms, else_body } => {
            if span_contains(&expr.span, line, col) {
                for arm in arms {
                    if let Some(info) = find_in_expr(&arm.condition, line, col, ctx) {
                        return Some(info);
                    }
                    if let Some(info) = find_in_body(&arm.body, line, col, ctx) {
                        return Some(info);
                    }
                }
                if let Some(body) = else_body
                    && let Some(info) = find_in_body(body, line, col, ctx)
                {
                    return Some(info);
                }
            }
        }
        ExprKind::Group { expr: inner } => {
            if span_contains(&expr.span, line, col) {
                return find_in_expr(inner, line, col, ctx);
            }
        }
        ExprKind::StructConstruction { type_path, .. } => {
            if span_contains(&expr.span, line, col) {
                let name = type_path.last()?;
                return classify_name(name, ctx);
            }
        }
        ExprKind::EnumConstruction { type_path, .. } => {
            if span_contains(&expr.span, line, col) {
                let name = type_path.first()?;
                return classify_name(name, ctx);
            }
        }
        ExprKind::While { condition, body } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(body, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        ExprKind::Loop { body } => {
            if span_contains(&expr.span, line, col)
                && let Some(info) = find_in_body(body, line, col, ctx)
            {
                return Some(info);
            }
        }
        ExprKind::Closure { body, .. } => {
            if span_contains(&expr.span, line, col)
                && let Some(info) = find_in_body(body, line, col, ctx)
            {
                return Some(info);
            }
        }
        ExprKind::ShortClosure { body, .. } => {
            if span_contains(&expr.span, line, col) {
                return find_in_expr(body, line, col, ctx);
            }
        }
        ExprKind::Unless { condition, body } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(body, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        ExprKind::List { elements } => {
            if span_contains(&expr.span, line, col) {
                for e in elements {
                    if let Some(info) = find_in_expr(e, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::Map { entries } => {
            if span_contains(&expr.span, line, col) {
                for (k, v) in entries {
                    if let Some(info) = find_in_expr(k, line, col, ctx) {
                        return Some(info);
                    }
                    if let Some(info) = find_in_expr(v, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::Spawn { expr: inner, .. } => {
            if span_contains(&expr.span, line, col) {
                return find_in_expr(inner, line, col, ctx);
            }
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
            ..
        } => {
            if span_contains(&expr.span, line, col) {
                for arm in arms {
                    if let Some(r) = find_in_pattern(&arm.pattern, line, col, ctx) {
                        return Some(r);
                    }
                    if let Some(guard) = &arm.guard
                        && let Some(r) = find_in_expr(guard, line, col, ctx)
                    {
                        return Some(r);
                    }
                    if let Some(r) = find_in_body(&arm.body, line, col, ctx) {
                        return Some(r);
                    }
                }
                if let Some(timeout) = after_timeout
                    && let Some(r) = find_in_expr(timeout, line, col, ctx)
                {
                    return Some(r);
                }
                for stmt in after_body {
                    if let Some(r) = find_in_statement(stmt, line, col, ctx) {
                        return Some(r);
                    }
                }
            }
        }
        ExprKind::For { iterable, body, .. } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(iterable, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(body, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        ExprKind::String { parts, .. } => {
            if span_contains(&expr.span, line, col) {
                for part in parts {
                    if let StringPart::Interpolation { expr, .. } = part
                        && let Some(info) = find_in_expr(expr, line, col, ctx)
                    {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            if span_contains(&expr.span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_expr(then_expr, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_expr(else_expr, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        ExprKind::Unary { operand, .. } => {
            if span_contains(&expr.span, line, col) {
                return find_in_expr(operand, line, col, ctx);
            }
        }
        ExprKind::BinaryLiteral { segments } => {
            if span_contains(&expr.span, line, col) {
                for seg in segments {
                    if let Some(info) = find_in_expr(&seg.value, line, col, ctx) {
                        return Some(info);
                    }
                    if let Some(sz) = &seg.size
                        && let Some(info) = find_in_expr(sz, line, col, ctx)
                    {
                        return Some(info);
                    }
                }
            }
        }
        ExprKind::Arena { .. } | ExprKind::Literal { .. } | ExprKind::Self_ => {}
    }
    None
}

// ---------------------------------------------------------------------------
// find_expr_at: returns the innermost Expr at a cursor position
// ---------------------------------------------------------------------------

/// A call site found by `find_enclosing_call`.
pub(crate) struct CallSite<'a> {
    pub expr: &'a Expr,
    pub active_param: usize,
}

/// Returns a reference to the innermost `Expr` node whose span contains the
/// given cursor position, walking through all items in the module.
pub(crate) fn find_expr_at(module: &Module, line: u32, col: u32) -> Option<&Expr> {
    for item in &module.items {
        let result = match item {
            Item::Function(f) => {
                if !span_contains(&f.span, line, col) {
                    continue;
                }
                f.body
                    .as_ref()
                    .and_then(|b| find_expr_at_in_body(b, line, col))
            }
            Item::Impl(imp) => imp.members.iter().find_map(|m| {
                if let ImplMember::Function(f) = m
                    && span_contains(&f.span, line, col)
                {
                    return f
                        .body
                        .as_ref()
                        .and_then(|b| find_expr_at_in_body(b, line, col));
                }
                None
            }),
            Item::Protocol(p) => {
                if !span_contains(&p.span, line, col) {
                    continue;
                }
                p.methods.iter().find_map(|m| {
                    if span_contains(&m.span, line, col) {
                        m.body
                            .as_ref()
                            .and_then(|b| find_expr_at_in_body(b, line, col))
                    } else {
                        None
                    }
                })
            }
            Item::Struct(s) => {
                if !span_contains(&s.span, line, col) {
                    continue;
                }
                s.functions.iter().find_map(|f| {
                    f.body
                        .as_ref()
                        .and_then(|b| find_expr_at_in_body(b, line, col))
                })
            }
            Item::Enum(e) => {
                if !span_contains(&e.span, line, col) {
                    continue;
                }
                e.functions.iter().find_map(|f| {
                    f.body
                        .as_ref()
                        .and_then(|b| find_expr_at_in_body(b, line, col))
                })
            }
            _ => None,
        };
        if result.is_some() {
            return result;
        }
    }
    None
}

fn find_expr_at_in_body(body: &[Statement], line: u32, col: u32) -> Option<&Expr> {
    body.iter()
        .find_map(|stmt| find_expr_at_in_stmt(stmt, line, col))
}

fn find_expr_at_in_stmt(stmt: &Statement, line: u32, col: u32) -> Option<&Expr> {
    match stmt {
        Statement::Expr(expr) => find_expr_at_inner(expr, line, col),
        Statement::Assignment { value, .. } => find_expr_at_inner(value, line, col),
        Statement::CompoundAssign { value, .. } => find_expr_at_inner(value, line, col),
        Statement::Return {
            value: Some(expr), ..
        } => find_expr_at_inner(expr, line, col),
        _ => None,
    }
}

/// Recursively descends into the expression tree and returns the innermost
/// `Expr` whose span contains the cursor.
fn find_expr_at_inner(expr: &Expr, line: u32, col: u32) -> Option<&Expr> {
    if !span_contains(&expr.span, line, col) {
        return None;
    }

    let child = match &expr.kind {
        ExprKind::Call { callee, args } => find_expr_at_inner(callee, line, col).or_else(|| {
            args.iter()
                .find_map(|a| find_expr_at_inner(&a.value, line, col))
        }),
        ExprKind::MethodCall { receiver, args, .. } => find_expr_at_inner(receiver, line, col)
            .or_else(|| {
                args.iter()
                    .find_map(|a| find_expr_at_inner(&a.value, line, col))
            }),
        ExprKind::FieldAccess { receiver, .. } => find_expr_at_inner(receiver, line, col),
        ExprKind::Binary { left, right, .. } => {
            find_expr_at_inner(left, line, col).or_else(|| find_expr_at_inner(right, line, col))
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => find_expr_at_inner(condition, line, col)
            .or_else(|| find_expr_at_in_body(then_body, line, col))
            .or_else(|| {
                else_body
                    .as_ref()
                    .and_then(|b| find_expr_at_in_body(b, line, col))
            }),
        ExprKind::Match { subject, arms } => find_expr_at_inner(subject, line, col).or_else(|| {
            arms.iter().find_map(|arm| {
                arm.guard
                    .as_ref()
                    .and_then(|g| find_expr_at_inner(g, line, col))
                    .or_else(|| find_expr_at_in_body(&arm.body, line, col))
            })
        }),
        ExprKind::Cond { arms, else_body } => arms
            .iter()
            .find_map(|arm| {
                find_expr_at_inner(&arm.condition, line, col)
                    .or_else(|| find_expr_at_in_body(&arm.body, line, col))
            })
            .or_else(|| {
                else_body
                    .as_ref()
                    .and_then(|b| find_expr_at_in_body(b, line, col))
            }),
        ExprKind::Group { expr: inner } => find_expr_at_inner(inner, line, col),
        ExprKind::While { condition, body } => find_expr_at_inner(condition, line, col)
            .or_else(|| find_expr_at_in_body(body, line, col)),
        ExprKind::Loop { body } | ExprKind::Closure { body, .. } => {
            find_expr_at_in_body(body, line, col)
        }
        ExprKind::ShortClosure { body, .. } => find_expr_at_inner(body, line, col),
        ExprKind::Unless { condition, body } => find_expr_at_inner(condition, line, col)
            .or_else(|| find_expr_at_in_body(body, line, col)),
        ExprKind::List { elements } => elements
            .iter()
            .find_map(|e| find_expr_at_inner(e, line, col)),
        ExprKind::Map { entries } => entries.iter().find_map(|(k, v)| {
            find_expr_at_inner(k, line, col).or_else(|| find_expr_at_inner(v, line, col))
        }),
        ExprKind::Spawn { expr: inner, .. } => find_expr_at_inner(inner, line, col),
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
            ..
        } => arms
            .iter()
            .find_map(|arm| {
                arm.guard
                    .as_ref()
                    .and_then(|g| find_expr_at_inner(g, line, col))
                    .or_else(|| find_expr_at_in_body(&arm.body, line, col))
            })
            .or_else(|| {
                after_timeout
                    .as_ref()
                    .and_then(|t| find_expr_at_inner(t, line, col))
            })
            .or_else(|| find_expr_at_in_body(after_body, line, col)),
        ExprKind::For { iterable, body, .. } => find_expr_at_inner(iterable, line, col)
            .or_else(|| find_expr_at_in_body(body, line, col)),
        ExprKind::String { parts, .. } => parts.iter().find_map(|p| {
            if let StringPart::Interpolation { expr, .. } = p {
                find_expr_at_inner(expr, line, col)
            } else {
                None
            }
        }),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => find_expr_at_inner(condition, line, col)
            .or_else(|| find_expr_at_inner(then_expr, line, col))
            .or_else(|| find_expr_at_inner(else_expr, line, col)),
        ExprKind::Unary { operand, .. } => find_expr_at_inner(operand, line, col),
        ExprKind::BinaryLiteral { segments } => segments.iter().find_map(|seg| {
            find_expr_at_inner(&seg.value, line, col).or_else(|| {
                seg.size
                    .as_ref()
                    .and_then(|s| find_expr_at_inner(s, line, col))
            })
        }),
        ExprKind::StructConstruction { fields, .. } => fields
            .iter()
            .find_map(|f| find_expr_at_inner(&f.value, line, col)),
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Tuple(args) => {
                args.iter().find_map(|a| find_expr_at_inner(a, line, col))
            }
            EnumConstructionData::Struct(fields) => fields
                .iter()
                .find_map(|f| find_expr_at_inner(&f.value, line, col)),
            EnumConstructionData::Unit => None,
        },
        ExprKind::Arena { body } => find_expr_at_in_body(body, line, col),
        ExprKind::Literal { .. } | ExprKind::Self_ | ExprKind::Ident { .. } => None,
    };

    Some(child.unwrap_or(expr))
}

// ---------------------------------------------------------------------------
// find_enclosing_call: returns the innermost Call/MethodCall at a position
// ---------------------------------------------------------------------------

/// Returns the innermost `Call` or `MethodCall` expression enclosing the
/// cursor, along with the index of the argument the cursor falls within.
pub(crate) fn find_enclosing_call(module: &Module, line: u32, col: u32) -> Option<CallSite<'_>> {
    for item in &module.items {
        let result = match item {
            Item::Function(f) => {
                if !span_contains(&f.span, line, col) {
                    continue;
                }
                f.body
                    .as_ref()
                    .and_then(|b| find_call_in_body(b, line, col))
            }
            Item::Impl(imp) => imp.members.iter().find_map(|m| {
                if let ImplMember::Function(f) = m
                    && span_contains(&f.span, line, col)
                {
                    return f
                        .body
                        .as_ref()
                        .and_then(|b| find_call_in_body(b, line, col));
                }
                None
            }),
            Item::Protocol(p) => {
                if !span_contains(&p.span, line, col) {
                    continue;
                }
                p.methods.iter().find_map(|m| {
                    if span_contains(&m.span, line, col) {
                        m.body
                            .as_ref()
                            .and_then(|b| find_call_in_body(b, line, col))
                    } else {
                        None
                    }
                })
            }
            Item::Struct(s) => {
                if !span_contains(&s.span, line, col) {
                    continue;
                }
                s.functions.iter().find_map(|f| {
                    f.body
                        .as_ref()
                        .and_then(|b| find_call_in_body(b, line, col))
                })
            }
            Item::Enum(e) => {
                if !span_contains(&e.span, line, col) {
                    continue;
                }
                e.functions.iter().find_map(|f| {
                    f.body
                        .as_ref()
                        .and_then(|b| find_call_in_body(b, line, col))
                })
            }
            _ => None,
        };
        if result.is_some() {
            return result;
        }
    }
    None
}

fn find_call_in_body<'a>(body: &'a [Statement], line: u32, col: u32) -> Option<CallSite<'a>> {
    body.iter().find_map(|stmt| match stmt {
        Statement::Expr(expr) => find_call_inner(expr, line, col),
        Statement::Assignment { value, .. } => find_call_inner(value, line, col),
        Statement::CompoundAssign { value, .. } => find_call_inner(value, line, col),
        Statement::Return {
            value: Some(expr), ..
        } => find_call_inner(expr, line, col),
        _ => None,
    })
}

/// Recursively descends into the expression tree looking for the innermost
/// `Call` or `MethodCall` that encloses the cursor position.
fn find_call_inner<'a>(expr: &'a Expr, line: u32, col: u32) -> Option<CallSite<'a>> {
    if !span_contains(&expr.span, line, col) {
        return None;
    }

    // Always try children first so we find the innermost call.
    let child = match &expr.kind {
        ExprKind::Call { callee, args } => find_call_inner(callee, line, col).or_else(|| {
            args.iter()
                .find_map(|a| find_call_inner(&a.value, line, col))
        }),
        ExprKind::MethodCall { receiver, args, .. } => find_call_inner(receiver, line, col)
            .or_else(|| {
                args.iter()
                    .find_map(|a| find_call_inner(&a.value, line, col))
            }),
        ExprKind::FieldAccess { receiver, .. } => find_call_inner(receiver, line, col),
        ExprKind::Binary { left, right, .. } => {
            find_call_inner(left, line, col).or_else(|| find_call_inner(right, line, col))
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => find_call_inner(condition, line, col)
            .or_else(|| find_call_in_body(then_body, line, col))
            .or_else(|| {
                else_body
                    .as_ref()
                    .and_then(|b| find_call_in_body(b, line, col))
            }),
        ExprKind::Match { subject, arms } => find_call_inner(subject, line, col).or_else(|| {
            arms.iter().find_map(|arm| {
                arm.guard
                    .as_ref()
                    .and_then(|g| find_call_inner(g, line, col))
                    .or_else(|| find_call_in_body(&arm.body, line, col))
            })
        }),
        ExprKind::Cond { arms, else_body } => arms
            .iter()
            .find_map(|arm| {
                find_call_inner(&arm.condition, line, col)
                    .or_else(|| find_call_in_body(&arm.body, line, col))
            })
            .or_else(|| {
                else_body
                    .as_ref()
                    .and_then(|b| find_call_in_body(b, line, col))
            }),
        ExprKind::Group { expr: inner } => find_call_inner(inner, line, col),
        ExprKind::While { condition, body } => {
            find_call_inner(condition, line, col).or_else(|| find_call_in_body(body, line, col))
        }
        ExprKind::Loop { body } | ExprKind::Closure { body, .. } => {
            find_call_in_body(body, line, col)
        }
        ExprKind::ShortClosure { body, .. } => find_call_inner(body, line, col),
        ExprKind::Unless { condition, body } => {
            find_call_inner(condition, line, col).or_else(|| find_call_in_body(body, line, col))
        }
        ExprKind::List { elements } => elements.iter().find_map(|e| find_call_inner(e, line, col)),
        ExprKind::Map { entries } => entries.iter().find_map(|(k, v)| {
            find_call_inner(k, line, col).or_else(|| find_call_inner(v, line, col))
        }),
        ExprKind::Spawn { expr: inner, .. } => find_call_inner(inner, line, col),
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
            ..
        } => arms
            .iter()
            .find_map(|arm| {
                arm.guard
                    .as_ref()
                    .and_then(|g| find_call_inner(g, line, col))
                    .or_else(|| find_call_in_body(&arm.body, line, col))
            })
            .or_else(|| {
                after_timeout
                    .as_ref()
                    .and_then(|t| find_call_inner(t, line, col))
            })
            .or_else(|| find_call_in_body(after_body, line, col)),
        ExprKind::For { iterable, body, .. } => {
            find_call_inner(iterable, line, col).or_else(|| find_call_in_body(body, line, col))
        }
        ExprKind::String { parts, .. } => parts.iter().find_map(|p| {
            if let StringPart::Interpolation { expr, .. } = p {
                find_call_inner(expr, line, col)
            } else {
                None
            }
        }),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => find_call_inner(condition, line, col)
            .or_else(|| find_call_inner(then_expr, line, col))
            .or_else(|| find_call_inner(else_expr, line, col)),
        ExprKind::Unary { operand, .. } => find_call_inner(operand, line, col),
        ExprKind::BinaryLiteral { segments } => segments.iter().find_map(|seg| {
            find_call_inner(&seg.value, line, col).or_else(|| {
                seg.size
                    .as_ref()
                    .and_then(|s| find_call_inner(s, line, col))
            })
        }),
        ExprKind::StructConstruction { fields, .. } => fields
            .iter()
            .find_map(|f| find_call_inner(&f.value, line, col)),
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Tuple(args) => {
                args.iter().find_map(|a| find_call_inner(a, line, col))
            }
            EnumConstructionData::Struct(fields) => fields
                .iter()
                .find_map(|f| find_call_inner(&f.value, line, col)),
            EnumConstructionData::Unit => None,
        },
        ExprKind::Arena { body } => find_call_in_body(body, line, col),
        _ => None,
    };

    if child.is_some() {
        return child;
    }

    // If no deeper call was found, check whether *this* node is a call.
    match &expr.kind {
        ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => {
            let active_param = compute_active_param(args, line, col);
            Some(CallSite { expr, active_param })
        }
        _ => None,
    }
}

/// Determines which parameter index the cursor is on by comparing its
/// position against the argument spans.
fn compute_active_param(args: &[Arg], line: u32, col: u32) -> usize {
    for (i, arg) in args.iter().enumerate() {
        if span_contains(&arg.span, line, col) {
            return i;
        }
    }
    // Cursor is past all args (e.g. after trailing comma or in empty parens).
    args.len()
}

/// Resolves the mangled function name for a method call using the
/// receiver's `resolved_type` (e.g. `Int_band` for `5.band(3)`).
fn resolve_method_name(receiver: &Expr, method: &str, ctx: &TypeContext) -> Option<String> {
    let resolved = receiver.resolved_type.as_ref()?;
    let type_name = resolved.display();
    let mangled = format!("{type_name}_{method}");
    if ctx.functions.contains_key(&mangled) {
        return Some(mangled);
    }
    None
}

/// Returns true if the cursor is positioned on the method name portion of a
/// method call (after the `.`), not on the receiver or arguments.
fn cursor_on_method(receiver: &Expr, method: &str, span: &Span, line: u32, col: u32) -> bool {
    let recv_end = match &receiver.kind {
        ExprKind::Literal { .. }
        | ExprKind::Ident { .. }
        | ExprKind::Group { .. }
        | ExprKind::Call { .. }
        | ExprKind::MethodCall { .. }
        | ExprKind::FieldAccess { .. } => receiver.span.end.column,
        _ => return span_contains(span, line, col),
    };
    let method_start = recv_end + 2;
    let method_end = method_start + method.len() as u32;
    line == span.start.line && col >= method_start && col <= method_end
}
