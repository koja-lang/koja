//! AST visitor shared across closure pass slices.
//!
//! Each slice's discovery walk visits every [`Expr`] reachable from a
//! function body and inspects it for generic instantiations to register
//! with the [`crate::IRProgram`]. The traversal scaffolding (descend
//! into block bodies, conditional arms, loop bodies, match patterns,
//! etc.) is identical across slices and lives here so individual
//! visitors can focus on the per-`ExprKind` decision logic.
//!
//! The visitor is an in-order traversal: a parent `Expr` is visited
//! before its children. Visitors that only care about specific
//! `ExprKind` variants ignore the rest.
//!
//! Slice 1 uses this for struct/enum construction discovery; slice 2
//! reuses it for free-function call sites; slice 3 for generic method
//! calls. If a future elaboration sub-pass needs the same shape, the
//! helper can move to a shared location at that point.

use expo_ast::ast::{
    AssignTarget, BinarySegment, EnumConstructionData, Expr, ExprKind, Function, MatchArm, Pattern,
    Statement, StringPart,
};

/// Visit every [`Expr`] reachable from `function`'s body, calling
/// `visitor` on each. No-op when the function has no body
/// (e.g. compiler-synthesized intrinsics, extern declarations).
pub fn visit_function_exprs<F>(function: &Function, mut visitor: F)
where
    F: FnMut(&Expr),
{
    let Some(body) = function.body.as_ref() else {
        return;
    };
    visit_statements(body, &mut visitor);
}

fn visit_statements<F>(statements: &[Statement], visitor: &mut F)
where
    F: FnMut(&Expr),
{
    for statement in statements {
        visit_statement(statement, visitor);
    }
}

fn visit_statement<F>(statement: &Statement, visitor: &mut F)
where
    F: FnMut(&Expr),
{
    match statement {
        Statement::Assignment {
            target,
            value,
            type_annotation: _,
            ..
        } => {
            visit_assign_target(target, visitor);
            visit_expr(value, visitor);
        }
        Statement::Break { .. } => {}
        Statement::CompoundAssign { value, .. } => visit_expr(value, visitor),
        Statement::Expr(expr) => visit_expr(expr, visitor),
        Statement::Return { value, .. } => {
            if let Some(expr) = value {
                visit_expr(expr, visitor);
            }
        }
    }
}

fn visit_assign_target<F>(target: &AssignTarget, visitor: &mut F)
where
    F: FnMut(&Expr),
{
    match target {
        AssignTarget::LValue(_) => {}
        AssignTarget::Pattern(pattern) => visit_pattern(pattern, visitor),
    }
}

fn visit_expr<F>(expr: &Expr, visitor: &mut F)
where
    F: FnMut(&Expr),
{
    visitor(expr);
    visit_expr_children(expr, visitor);
}

fn visit_expr_children<F>(expr: &Expr, visitor: &mut F)
where
    F: FnMut(&Expr),
{
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            visit_expr(left, visitor);
            visit_expr(right, visitor);
        }
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                visit_binary_segment(segment, visitor);
            }
        }
        ExprKind::Call { callee, args } => {
            visit_expr(callee, visitor);
            for arg in args {
                visit_expr(&arg.value, visitor);
            }
        }
        ExprKind::Closure { body, .. } => visit_statements(body, visitor),
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                visit_expr(&arm.condition, visitor);
                visit_statements(&arm.body, visitor);
            }
            if let Some(body) = else_body {
                visit_statements(body, visitor);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Unit => {}
            EnumConstructionData::Tuple(exprs) => {
                for expr in exprs {
                    visit_expr(expr, visitor);
                }
            }
            EnumConstructionData::Struct(field_inits) => {
                for field in field_inits {
                    visit_expr(&field.value, visitor);
                }
            }
        },
        ExprKind::FieldAccess { receiver, .. } => visit_expr(receiver, visitor),
        ExprKind::For {
            pattern,
            iterable,
            body,
        } => {
            visit_pattern(pattern, visitor);
            visit_expr(iterable, visitor);
            visit_statements(body, visitor);
        }
        ExprKind::Group { expr: inner } => visit_expr(inner, visitor),
        ExprKind::Ident { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            visit_expr(condition, visitor);
            visit_statements(then_body, visitor);
            if let Some(body) = else_body {
                visit_statements(body, visitor);
            }
        }
        ExprKind::List { elements } => {
            for element in elements {
                visit_expr(element, visitor);
            }
        }
        ExprKind::Literal { .. } => {}
        ExprKind::Loop { body } => visit_statements(body, visitor),
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                visit_expr(key, visitor);
                visit_expr(value, visitor);
            }
        }
        ExprKind::Match { subject, arms } => {
            visit_expr(subject, visitor);
            for arm in arms {
                visit_match_arm(arm, visitor);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            visit_expr(receiver, visitor);
            for arg in args {
                visit_expr(&arg.value, visitor);
            }
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                visit_match_arm(arm, visitor);
            }
            if let Some(timeout) = after_timeout {
                visit_expr(timeout, visitor);
            }
            visit_statements(after_body, visitor);
        }
        ExprKind::Self_ { .. } => {}
        ExprKind::ShortClosure { body, .. } => visit_expr(body, visitor),
        ExprKind::Spawn { expr: inner } => visit_expr(inner, visitor),
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    visit_expr(expr, visitor);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                visit_expr(&field.value, visitor);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr(condition, visitor);
            visit_expr(then_expr, visitor);
            visit_expr(else_expr, visitor);
        }
        ExprKind::Unary { operand, .. } => visit_expr(operand, visitor),
        ExprKind::Unless { condition, body } => {
            visit_expr(condition, visitor);
            visit_statements(body, visitor);
        }
        ExprKind::While { condition, body } => {
            visit_expr(condition, visitor);
            visit_statements(body, visitor);
        }
    }
}

fn visit_match_arm<F>(arm: &MatchArm, visitor: &mut F)
where
    F: FnMut(&Expr),
{
    visit_pattern(&arm.pattern, visitor);
    if let Some(guard) = arm.guard.as_ref() {
        visit_expr(guard, visitor);
    }
    visit_statements(&arm.body, visitor);
}

fn visit_pattern<F>(_pattern: &Pattern, _visitor: &mut F)
where
    F: FnMut(&Expr),
{
    // Patterns can contain nested expressions in `Bind { default }` and
    // similar, but no current closure-pass slice cares about discovery
    // inside patterns. Future elaboration sub-passes (numeric coercion
    // staging, etc.) may extend this; leave as a no-op for now.
}

fn visit_binary_segment<F>(segment: &BinarySegment, visitor: &mut F)
where
    F: FnMut(&Expr),
{
    visit_expr(&segment.value, visitor);
}
