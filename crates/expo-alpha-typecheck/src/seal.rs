//! Seal sub-pass: walks the checked AST and asserts that every
//! relevant [`Resolution`] / [`expo_ast::identifier::ResolvedType`]
//! annotation is populated. Panics on violation per the
//! [`COMPILER-NORTHSTAR.md`] contract — seal failures indicate
//! compiler bugs in upstream sub-passes, not user errors.
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
    for item in &file.items {
        if let Item::Function(function) = item {
            seal_function(function);
        }
    }
    if let Some(body) = file.body.as_ref() {
        // Script-mode files keep their top-level statements on
        // `file.body`. There is no synthesized `fn main`; downstream
        // passes (`expo-alpha-ir::lower_script`) consume the body
        // directly. Seal the same statement-tree invariants that
        // function bodies satisfy.
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
    // Exception for the callee position of a `Call`: function names
    // are not first-class values yet, so the outer callee
    // `Expr.resolution` deliberately stays `Unresolved`. Every other
    // position must carry a fully-resolved type.
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
        ExprKind::Unary { operand, .. } => seal_expr(operand),
        other => seal_panic(
            &format!(
                "alpha typecheck seal does not yet recognize expression kind `{}`",
                expr_kind_label(other)
            ),
            expr.span,
        ),
    }
}

/// Seal the callee position of a `Call`. The outer `Expr.resolution`
/// intentionally stays `Unresolved` here (function names aren't
/// values yet) — what we check is that the inner `Ident` carries a
/// `Global(_)` resolution so downstream IR lowering has a concrete
/// target.
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
