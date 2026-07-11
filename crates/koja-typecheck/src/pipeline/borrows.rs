//! Post-resolve position check for `CPtr.borrow`. The borrowed
//! pointer is a zero-cost view of a `Binary`'s payload, valid only
//! while the source is live. Consuming it within the borrowing
//! statement (call argument, chained receiver) is always safe under
//! ordinary scope-exit drop semantics, so those are the only legal
//! positions. Binding, returning, or storing the result is rejected
//! with a teaching diagnostic pointing at `CPtr.copy`.

use koja_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, ExprKind, File, Function, ImplMember, Item, LValue,
    Statement, StringPart,
};
use koja_ast::identifier::Resolution;

use crate::registry::GlobalRegistry;

/// How the expression position under inspection treats a
/// `CPtr.borrow` result.
#[derive(Clone, Copy)]
enum Position<'a> {
    /// Right-hand side of `target = ...`.
    Bound(&'a LValue),
    /// Consumed in-statement as a call argument or chained receiver.
    Consumed,
    /// Any other position (struct field, collection element, ...).
    Escaping,
    /// Explicit `return` or implicit tail-expression return.
    Returned,
}

pub(crate) fn check_file(
    file: &File,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &file.items {
        match item {
            Item::Enum(decl) => check_functions(&decl.functions, registry, diagnostics),
            Item::Extend(block) => check_members(&block.members, registry, diagnostics),
            Item::Function(function) => check_function(function, registry, diagnostics),
            Item::Impl(block) => check_members(&block.members, registry, diagnostics),
            Item::Struct(decl) => check_functions(&decl.functions, registry, diagnostics),
            _ => {}
        }
    }
    if let Some(body) = file.body.as_ref() {
        check_body(body, Position::Escaping, registry, diagnostics);
    }
}

fn check_functions(
    functions: &[Function],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for function in functions {
        check_function(function, registry, diagnostics);
    }
}

fn check_members(
    members: &[ImplMember],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in members {
        if let ImplMember::Function(function) = member {
            check_function(function, registry, diagnostics);
        }
    }
}

fn check_function(
    function: &Function,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(body) = function.body.as_ref() {
        check_body(body, Position::Returned, registry, diagnostics);
    }
}

/// Walk a statement body. The tail statement, when it is a bare
/// expression, produces the body's value, so it checks against
/// `tail` (implicit return for function/closure bodies) instead of
/// the ordinary statement positions.
fn check_body(
    body: &[Statement],
    tail: Position<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((last, leading)) = body.split_last() else {
        return;
    };
    for stmt in leading {
        check_statement(stmt, registry, diagnostics);
    }
    match last {
        Statement::Expr(expr) => check_expr(expr, tail, registry, diagnostics),
        other => check_statement(other, registry, diagnostics),
    }
}

fn check_statement(stmt: &Statement, registry: &GlobalRegistry, diagnostics: &mut Vec<Diagnostic>) {
    match stmt {
        Statement::Assignment { target, value, .. } => {
            check_expr(value, Position::Bound(target), registry, diagnostics);
        }
        Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        Statement::CompoundAssign { value, .. } => {
            check_expr(value, Position::Escaping, registry, diagnostics);
        }
        Statement::Expr(expr) => check_expr(expr, Position::Escaping, registry, diagnostics),
        Statement::Return {
            value: Some(value), ..
        } => check_expr(value, Position::Returned, registry, diagnostics),
    }
}

fn check_expr(
    expr: &Expr,
    position: Position<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if is_cptr_borrow(expr, registry) {
        emit_escape(position, expr, diagnostics);
    }
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            check_expr(left, Position::Escaping, registry, diagnostics);
            check_expr(right, Position::Escaping, registry, diagnostics);
        }
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                check_expr(&segment.value, Position::Escaping, registry, diagnostics);
                if let Some(size) = segment.size.as_ref() {
                    check_expr(size, Position::Escaping, registry, diagnostics);
                }
            }
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                check_expr(&arg.value, Position::Consumed, registry, diagnostics);
            }
        }
        ExprKind::Closure { body, .. } => {
            check_body(body, Position::Returned, registry, diagnostics);
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                check_expr(&arm.condition, Position::Escaping, registry, diagnostics);
                check_body(&arm.body, Position::Escaping, registry, diagnostics);
            }
            if let Some(else_body) = else_body {
                check_body(else_body, Position::Escaping, registry, diagnostics);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => {
                for field in fields {
                    check_expr(&field.value, Position::Escaping, registry, diagnostics);
                }
            }
            EnumConstructionData::Tuple(exprs) => {
                for expr in exprs {
                    check_expr(expr, Position::Escaping, registry, diagnostics);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::FieldAccess { receiver, .. } => {
            check_expr(receiver, Position::Escaping, registry, diagnostics);
        }
        ExprKind::For { iterable, body, .. } => {
            check_expr(iterable, Position::Escaping, registry, diagnostics);
            check_body(body, Position::Escaping, registry, diagnostics);
        }
        // Parentheses are pure grouping, so `(CPtr.borrow(b)).read()`
        // consumes the same as the unparenthesized chain.
        ExprKind::Group { expr: inner } => check_expr(inner, position, registry, diagnostics),
        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            check_expr(condition, Position::Escaping, registry, diagnostics);
            check_body(then_body, Position::Escaping, registry, diagnostics);
            if let Some(else_body) = else_body {
                check_body(else_body, Position::Escaping, registry, diagnostics);
            }
        }
        ExprKind::List { elements } => {
            for element in elements {
                check_expr(element, Position::Escaping, registry, diagnostics);
            }
        }
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                check_expr(key, Position::Escaping, registry, diagnostics);
                check_expr(value, Position::Escaping, registry, diagnostics);
            }
        }
        ExprKind::Loop { body } => check_body(body, Position::Escaping, registry, diagnostics),
        ExprKind::Match { subject, arms } => {
            check_expr(subject, Position::Escaping, registry, diagnostics);
            for arm in arms {
                if let Some(guard) = arm.guard.as_ref() {
                    check_expr(guard, Position::Escaping, registry, diagnostics);
                }
                check_body(&arm.body, Position::Escaping, registry, diagnostics);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            check_expr(receiver, Position::Consumed, registry, diagnostics);
            for arg in args {
                check_expr(&arg.value, Position::Consumed, registry, diagnostics);
            }
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                if let Some(guard) = arm.guard.as_ref() {
                    check_expr(guard, Position::Escaping, registry, diagnostics);
                }
                check_body(&arm.body, Position::Escaping, registry, diagnostics);
            }
            if let Some(timeout) = after_timeout.as_ref() {
                check_expr(timeout, Position::Escaping, registry, diagnostics);
            }
            check_body(after_body, Position::Escaping, registry, diagnostics);
        }
        ExprKind::ShortClosure { body, .. } => {
            check_expr(body, Position::Returned, registry, diagnostics);
        }
        ExprKind::Spawn { expr: inner } => {
            check_expr(inner, Position::Escaping, registry, diagnostics);
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr: inner, .. } = part {
                    check_expr(inner, Position::Escaping, registry, diagnostics);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                check_expr(&field.value, Position::Escaping, registry, diagnostics);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            check_expr(condition, Position::Escaping, registry, diagnostics);
            check_expr(then_expr, Position::Escaping, registry, diagnostics);
            check_expr(else_expr, Position::Escaping, registry, diagnostics);
        }
        ExprKind::Unary { operand, .. } => {
            check_expr(operand, Position::Escaping, registry, diagnostics);
        }
        ExprKind::Unless { condition, body } => {
            check_expr(condition, Position::Escaping, registry, diagnostics);
            check_body(body, Position::Escaping, registry, diagnostics);
        }
        ExprKind::While { condition, body } => {
            check_expr(condition, Position::Escaping, registry, diagnostics);
            check_body(body, Position::Escaping, registry, diagnostics);
        }
    }
}

fn emit_escape(position: Position<'_>, expr: &Expr, diagnostics: &mut Vec<Diagnostic>) {
    let opening = match position {
        Position::Bound(target) => {
            format!(
                "a borrowed pointer cannot be bound to `{}`",
                target.segments.join("."),
            )
        }
        Position::Consumed => return,
        Position::Escaping => "a borrowed pointer cannot be stored".to_string(),
        Position::Returned => "a borrowed pointer cannot be returned".to_string(),
    };
    diagnostics.push(Diagnostic::error(
        format!(
            "{opening}: it is only valid within the statement that borrows it. Pass it \
             directly to a call, or use `CPtr.copy(...)` for an owned copy",
        ),
        expr.span,
    ));
}

/// True when `expr` is a static call to the `Global.CPtr.borrow`
/// intrinsic. Resolve rewrites static receivers to a synthetic
/// `Ident` carrying the type's `Resolution::Global`, so the match is
/// exact even through aliasing or local shadowing.
fn is_cptr_borrow(expr: &Expr, registry: &GlobalRegistry) -> bool {
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &expr.kind
    else {
        return false;
    };
    if method != "borrow" {
        return false;
    }
    let ExprKind::Ident {
        resolution: Resolution::Global(receiver_id),
        ..
    } = &receiver.kind
    else {
        return false;
    };
    registry.get(*receiver_id).is_some_and(|entry| {
        entry.identifier.is_in_package("Global") && entry.identifier.path() == ["CPtr"]
    })
}
