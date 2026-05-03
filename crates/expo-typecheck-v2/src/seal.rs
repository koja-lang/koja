//! Seal sub-pass: walks the checked AST and asserts that every
//! relevant `resolution` / `resolved_type` field is populated. Panics
//! on violation per the [`COMPILER-NORTHSTAR.md`] contract — seal
//! failures indicate compiler bugs in upstream sub-passes, not user
//! errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md

use expo_ast::ast::{Expr, ExprKind, File, Function, Item, Statement};
use expo_ast::identifier::Resolution;
use expo_ast::span::Span;

use crate::labels::expr_kind_label;
use crate::program::CheckedProgram;

/// Asserts the sealed-AST invariants on `program`. Panics on violation.
pub(crate) fn seal_ast(program: &CheckedProgram) {
    for pkg in &program.packages {
        for file in &pkg.files {
            seal_file(file);
        }
    }
}

fn seal_file(file: &File) {
    if file.body.is_some() {
        seal_panic(
            "file.body must be hoisted into a synthesized fn main by `lift_script` \
             before sealing — this is a `lift_script` invariant violation",
            file.span,
        );
    }
    for item in &file.items {
        if let Item::Function(function) = item {
            seal_function(function);
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
        Statement::Assignment { value, .. } | Statement::CompoundAssign { value, .. } => {
            seal_expr(value);
        }
        Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        Statement::Expr(expr) => seal_expr(expr),
        Statement::Return {
            value: Some(value), ..
        } => seal_expr(value),
    }
}

fn seal_expr(expr: &Expr) {
    if expr.resolved_type.is_none() {
        seal_panic("expression missing resolved_type", expr.span);
    }
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            seal_expr(left);
            seal_expr(right);
        }
        ExprKind::Group { expr: inner } => seal_expr(inner),
        ExprKind::Ident { name, resolution } => {
            if matches!(resolution, Resolution::Unresolved) {
                seal_panic(
                    &format!("identifier `{name}` has Unresolved resolution after typecheck"),
                    expr.span,
                );
            }
        }
        ExprKind::Literal { .. } => {}
        other => seal_panic(
            &format!(
                "v2 typecheck seal does not yet recognize expression kind `{}`",
                expr_kind_label(other)
            ),
            expr.span,
        ),
    }
}

fn seal_panic(message: &str, span: Span) -> ! {
    panic!(
        "v2 typecheck seal violation: {message} at line {}, column {}",
        span.start.line, span.start.column
    );
}
