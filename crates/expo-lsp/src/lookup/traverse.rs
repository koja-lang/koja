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
    match expr {
        Expr::Ident { name, span } => {
            if span_contains(span, line, col) {
                return classify_name(name, ctx);
            }
        }
        Expr::Call {
            callee, args, span, ..
        } => {
            if span_contains(span, line, col) {
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
        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(receiver, line, col, ctx) {
                    return Some(info);
                }
                if let Expr::Ident {
                    span: recv_span, ..
                } = receiver.as_ref()
                {
                    let method_start = recv_span.end.column + 2;
                    let method_end = method_start + method.len() as u32;
                    if line == recv_span.end.line
                        && col >= method_start
                        && col <= method_end
                        && let Some(mangled) = resolve_method_name(receiver, method, ctx)
                    {
                        return Some(SymbolInfo::Function { name: mangled });
                    }
                } else if let Some(mangled) = resolve_method_name(receiver, method, ctx)
                    && cursor_on_method(receiver, method, span, line, col)
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
        Expr::FieldAccess { receiver, span, .. } => {
            if span_contains(span, line, col)
                && let Some(info) = find_in_expr(receiver, line, col, ctx)
            {
                return Some(info);
            }
        }
        Expr::Binary {
            left, right, span, ..
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(left, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_expr(right, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        Expr::If {
            condition,
            then_body,
            else_body,
            span,
        } => {
            if span_contains(span, line, col) {
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
        Expr::Match {
            subject,
            arms,
            span,
        } => {
            if span_contains(span, line, col) {
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
        Expr::Cond {
            arms,
            else_body,
            span,
        } => {
            if span_contains(span, line, col) {
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
        Expr::Group { expr, span } => {
            if span_contains(span, line, col) {
                return find_in_expr(expr, line, col, ctx);
            }
        }
        Expr::StructConstruction {
            type_path, span, ..
        } => {
            if span_contains(span, line, col) {
                let name = type_path.last()?;
                return classify_name(name, ctx);
            }
        }
        Expr::EnumConstruction {
            type_path, span, ..
        } => {
            if span_contains(span, line, col) {
                let name = type_path.first()?;
                return classify_name(name, ctx);
            }
        }
        Expr::While {
            condition,
            body,
            span,
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(body, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        Expr::Loop { body, span } => {
            if span_contains(span, line, col)
                && let Some(info) = find_in_body(body, line, col, ctx)
            {
                return Some(info);
            }
        }
        Expr::Closure { body, span, .. } => {
            if span_contains(span, line, col)
                && let Some(info) = find_in_body(body, line, col, ctx)
            {
                return Some(info);
            }
        }
        Expr::ShortClosure { body, span, .. } => {
            if span_contains(span, line, col) {
                return find_in_expr(body, line, col, ctx);
            }
        }
        Expr::Unless {
            condition,
            body,
            span,
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(body, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        Expr::List { elements, span } => {
            if span_contains(span, line, col) {
                for e in elements {
                    if let Some(info) = find_in_expr(e, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        Expr::Map { entries, span } => {
            if span_contains(span, line, col) {
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
        Expr::Spawn { expr, span, .. } => {
            if span_contains(span, line, col) {
                return find_in_expr(expr, line, col, ctx);
            }
        }
        Expr::Receive {
            arms,
            after_timeout,
            after_body,
            span: recv_span,
            ..
        } => {
            if span_contains(recv_span, line, col) {
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
        Expr::For {
            iterable,
            body,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(iterable, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_body(body, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        Expr::String { parts, span, .. } => {
            if span_contains(span, line, col) {
                for part in parts {
                    if let StringPart::Interpolation { expr, .. } = part
                        && let Some(info) = find_in_expr(expr, line, col, ctx)
                    {
                        return Some(info);
                    }
                }
            }
        }
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            if span_contains(span, line, col) {
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
        Expr::Unary { operand, span, .. } => {
            if span_contains(span, line, col) {
                return find_in_expr(operand, line, col, ctx);
            }
        }
        Expr::BinaryLiteral { segments, span } => {
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
        Expr::Arena { .. } | Expr::Literal { .. } | Expr::Self_ { .. } => {}
    }
    None
}

/// Resolves the mangled function name for a method call by inferring the
/// receiver's type from the type context (e.g. `Int_band` for `5.band(3)`).
fn resolve_method_name(receiver: &Expr, method: &str, ctx: &TypeContext) -> Option<String> {
    let type_name = match receiver {
        Expr::Literal { value, .. } => match value {
            Literal::Int(_) => Some("Int"),
            Literal::Float(_) => Some("Float"),
            Literal::Bool(_) => Some("Bool"),
            Literal::String(_) => Some("String"),
            Literal::Unit => None,
        },
        Expr::Ident { name, .. } => {
            if ctx.is_struct(name) || ctx.is_enum(name) {
                Some(name.as_str())
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(tn) = type_name {
        let mangled = format!("{tn}_{method}");
        if ctx.functions.contains_key(&mangled) {
            return Some(mangled);
        }
    }
    // For variable receivers where we don't know the type, try all known types
    ctx.functions
        .keys()
        .find(|k| k.ends_with(&format!("_{method}")))
        .cloned()
}

/// Returns true if the cursor is positioned on the method name portion of a
/// method call (after the `.`), not on the receiver or arguments.
fn cursor_on_method(receiver: &Expr, method: &str, span: &Span, line: u32, col: u32) -> bool {
    let recv_end = match receiver {
        Expr::Literal { span: s, .. }
        | Expr::Ident { span: s, .. }
        | Expr::Group { span: s, .. }
        | Expr::Call { span: s, .. }
        | Expr::MethodCall { span: s, .. }
        | Expr::FieldAccess { span: s, .. } => s.end.column,
        _ => return span_contains(span, line, col),
    };
    let method_start = recv_end + 2;
    let method_end = method_start + method.len() as u32;
    line == span.start.line && col >= method_start && col <= method_end
}
