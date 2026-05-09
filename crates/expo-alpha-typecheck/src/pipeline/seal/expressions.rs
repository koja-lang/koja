//! Expression-shape seal checks. Every expression position must
//! carry a fully-resolved [`expo_ast::identifier::ResolvedType`]
//! that does not leak `Resolution::TypeParam`. The callee position
//! of a `Call` is the one carve-out: function names aren't
//! first-class values yet, so the outer callee `Expr.resolution`
//! stays `Unresolved` while the inner `Ident` carries `Global(_)`.

use expo_ast::ast::{EnumConstructionData, Expr, ExprKind, StringPart};
use expo_ast::identifier::Resolution;
use expo_ast::labels::expr_kind_label;

use super::patterns::seal_pattern;
use super::statements::seal_statement;
use super::{seal_no_type_param, seal_panic};

pub(super) fn seal_expr(expr: &Expr) {
    // The callee position of a `Call` is the one carve-out: function
    // names aren't first-class values yet, so the outer callee
    // `Expr.resolution` stays `Unresolved`. Every other position must
    // carry a fully-resolved type that doesn't leak `TypeParam` —
    // those are decl-side annotations and have no business on a
    // construction-site value.
    if !expr.resolution.is_resolved() {
        seal_panic("expression missing resolution", expr.span);
    }
    seal_no_type_param(&expr.resolution, expr.span);
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            seal_expr(left);
            seal_expr(right);
        }
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                seal_expr(&segment.value);
            }
        }
        ExprKind::Call {
            callee,
            args,
            type_args,
        } => {
            seal_call_callee(callee);
            for arg in args {
                seal_expr(&arg.value);
            }
            for ty in type_args {
                seal_no_type_param(ty, expr.span);
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
            // `Resolution::Global` (struct names, callees) and
            // `Resolution::Local` (param/local references) satisfy
            // seal. `Resolution::Unresolved` and a leaked
            // `Resolution::TypeParam` are both compiler bugs.
            match resolution {
                Resolution::Global(_) | Resolution::Local(_) => {}
                Resolution::TypeParam { .. } => seal_panic(
                    &format!("identifier `{name}` resolves to a TypeParam after typecheck"),
                    expr.span,
                ),
                Resolution::Unresolved => seal_panic(
                    &format!("identifier `{name}` has Unresolved resolution after typecheck"),
                    expr.span,
                ),
            }
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                seal_expr(&arm.condition);
                for stmt in &arm.body {
                    seal_statement(stmt);
                }
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    seal_statement(stmt);
                }
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
        ExprKind::Match { subject, arms } => {
            seal_expr(subject);
            for arm in arms {
                seal_pattern(&arm.pattern);
                if let Some(guard) = &arm.guard {
                    seal_expr(guard);
                }
                for stmt in &arm.body {
                    seal_statement(stmt);
                }
            }
        }
        ExprKind::Self_ { .. } => {}
        ExprKind::MethodCall {
            receiver,
            args,
            type_args,
            ..
        } => {
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
            for ty in type_args {
                seal_no_type_param(ty, expr.span);
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
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            seal_expr(condition);
            seal_expr(then_expr);
            seal_expr(else_expr);
        }
        ExprKind::Unary { operand, .. } => seal_expr(operand),
        ExprKind::Unless { condition, body } => {
            seal_expr(condition);
            for stmt in body {
                seal_statement(stmt);
            }
        }
        ExprKind::While { condition, body } => {
            seal_expr(condition);
            for stmt in body {
                seal_statement(stmt);
            }
        }
        // `synthesize` rewrites statement-position fors and
        // resolve diagnoses expression-position fors; seal should
        // never see one.
        ExprKind::For { .. } => seal_panic(
            "alpha typecheck seal saw an `ExprKind::For` after synthesize",
            expr.span,
        ),
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
