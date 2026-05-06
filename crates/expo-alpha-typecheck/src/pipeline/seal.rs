//! Seal sub-pass: assert every relevant [`Resolution`] /
//! [`expo_ast::identifier::ResolvedType`] annotation is populated.
//! Panics on violation per [`COMPILER-NORTHSTAR.md`] — seal failures
//! are upstream compiler bugs, not user errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../../design/COMPILER-NORTHSTAR.md

use expo_ast::ast::{
    AssignTarget, EnumConstructionData, Expr, ExprKind, File, Function, ImplMember, Item, LValue,
    Statement, StringPart,
};
use expo_ast::identifier::Resolution;
use expo_ast::span::Span;

use crate::program::CheckedProgram;
use expo_ast::labels::expr_kind_label;

/// Asserts the sealed-AST invariants on `program`. Panics on violation.
pub(crate) fn seal_ast(program: &CheckedProgram) {
    for pkg in &program.packages {
        for file in &pkg.files {
            seal_file(file);
        }
    }
}

fn seal_file(file: &File) {
    for item in &file.items {
        match item {
            Item::Function(function) => seal_function(function),
            Item::Struct(decl) => {
                for function in &decl.functions {
                    seal_function(function);
                }
            }
            Item::Enum(decl) => {
                for function in &decl.functions {
                    seal_function(function);
                }
            }
            Item::Impl(impl_block) => {
                for member in &impl_block.members {
                    if let ImplMember::Function(function) = member {
                        seal_function(function);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(body) = file.body.as_ref() {
        // Script-mode files keep their top-level statements on
        // `file.body`; downstream passes consume them directly. Seal
        // the same statement-tree invariants function bodies satisfy.
        for stmt in body {
            seal_statement(stmt);
        }
    }
}

fn seal_function(function: &Function) {
    let Some(body) = function.body.as_ref() else {
        return;
    };
    for stmt in body {
        seal_statement(stmt);
    }
}

fn seal_statement(stmt: &Statement) {
    match stmt {
        Statement::Assignment {
            span,
            target,
            value,
            ..
        } => {
            seal_assign_target(target, *span);
            seal_expr(value);
        }
        Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            seal_compound_target(target, *span);
            seal_expr(value);
        }
        Statement::Expr(expr) => seal_expr(expr),
        Statement::Return {
            value: Some(value), ..
        } => seal_expr(value),
    }
}

/// Assignment targets must be single-segment [`AssignTarget::LValue`]s
/// — the resolver rejected pattern destructuring and dotted lvalues
/// upstream, so reaching seal with anything else is a compiler bug.
fn seal_assign_target(target: &AssignTarget, statement_span: Span) {
    match target {
        AssignTarget::LValue(lvalue) => {
            if lvalue.segments.len() != 1 {
                seal_panic(
                    &format!(
                        "assignment target has {} segments; resolver rejects multi-segment \
                         targets",
                        lvalue.segments.len(),
                    ),
                    lvalue.span,
                );
            }
        }
        AssignTarget::Pattern(_) => seal_panic(
            "assignment target is a destructuring pattern; resolver rejects this shape",
            statement_span,
        ),
    }
}

/// Compound-assign targets are bare `LValue`s (the AST shape only
/// admits the single-segment case as a happy-path; the resolver
/// rejects multi-segment forms and undeclared names). Past resolve,
/// a compound-assign target must carry both single-segment shape
/// *and* a stamped `local_id`.
fn seal_compound_target(target: &LValue, statement_span: Span) {
    if target.segments.len() != 1 {
        seal_panic(
            &format!(
                "compound-assign target has {} segments; resolver rejects multi-segment \
                 targets",
                target.segments.len(),
            ),
            target.span,
        );
    }
    if target.local_id.is_none() {
        seal_panic(
            &format!(
                "compound-assign target `{}` carries no LocalId; resolver should have \
                 stamped it on success or diagnosed otherwise",
                target.segments[0],
            ),
            statement_span,
        );
    }
}

fn seal_expr(expr: &Expr) {
    // The callee position of a `Call` is the one carve-out: function
    // names aren't first-class values yet, so the outer callee
    // `Expr.resolution` stays `Unresolved`. Every other position must
    // carry a fully-resolved type.
    if !expr.resolution.is_resolved() {
        seal_panic("expression missing resolution", expr.span);
    }
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            seal_expr(left);
            seal_expr(right);
        }
        ExprKind::Call { callee, args } => {
            seal_call_callee(callee);
            for arg in args {
                seal_expr(&arg.value);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => {
                for field in fields {
                    seal_expr(&field.value);
                }
            }
            EnumConstructionData::Tuple(exprs) => {
                for expr in exprs {
                    seal_expr(expr);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::FieldAccess { receiver, .. } => seal_expr(receiver),
        ExprKind::Group { expr: inner } => seal_expr(inner),
        ExprKind::Ident { name, resolution } => {
            // Both `Resolution::Global` (struct names, callees) and
            // `Resolution::Local` (param/local references) satisfy seal.
            // Only `Resolution::Unresolved` is a violation.
            if matches!(resolution, Resolution::Unresolved) {
                seal_panic(
                    &format!("identifier `{name}` has Unresolved resolution after typecheck"),
                    expr.span,
                );
            }
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            seal_expr(condition);
            for stmt in then_body {
                seal_statement(stmt);
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    seal_statement(stmt);
                }
            }
        }
        ExprKind::Literal { .. } => {}
        ExprKind::Self_ { .. } => {}
        ExprKind::MethodCall { receiver, args, .. } => {
            // Static method calls: receiver must resolve like any
            // other `Ident` reference (its `resolution` is the
            // struct id, populated by resolve). Args follow the same
            // rule as `Call`. The outer `Expr.resolution` is the
            // method's return type, already enforced by the
            // top-of-fn check.
            seal_expr(receiver);
            for arg in args {
                seal_expr(&arg.value);
            }
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    seal_expr(expr);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                seal_expr(&field.value);
            }
        }
        ExprKind::Unary { operand, .. } => seal_expr(operand),
        ExprKind::Unless { condition, body } => {
            seal_expr(condition);
            for stmt in body {
                seal_statement(stmt);
            }
        }
        other => seal_panic(
            &format!(
                "alpha typecheck seal does not yet recognize expression kind `{}`",
                expr_kind_label(other)
            ),
            expr.span,
        ),
    }
}

/// Seal the callee of a `Call`: the outer `Expr.resolution` stays
/// `Unresolved` (function names aren't values yet); we check the inner
/// `Ident` carries a `Global(_)` resolution so IR lowering has a
/// concrete target.
fn seal_call_callee(callee: &Expr) {
    let ExprKind::Ident { name, resolution } = &callee.kind else {
        seal_panic(
            &format!(
                "call site has a non-identifier callee `{}` that passed typecheck",
                expr_kind_label(&callee.kind),
            ),
            callee.span,
        );
    };
    if matches!(resolution, Resolution::Unresolved) {
        seal_panic(
            &format!("callee `{name}` has Unresolved resolution after typecheck"),
            callee.span,
        );
    }
}

fn seal_panic(message: &str, span: Span) -> ! {
    panic!(
        "alpha typecheck seal violation: {message} at line {}, column {}",
        span.start.line, span.start.column
    );
}
