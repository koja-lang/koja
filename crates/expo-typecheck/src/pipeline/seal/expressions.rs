//! Expression-shape seal checks. Every expression position must
//! carry a fully-resolved [`expo_ast::identifier::ResolvedType`]
//! that does not leak `Resolution::TypeParam`. The callee position
//! of a `Call` is the one carve-out: function names aren't
//! first-class values yet, so the outer callee `Expr.resolution`
//! stays `Unresolved` while the inner `Ident` carries `Global(_)`.

use expo_ast::ast::{
    ClosureParam, EnumConstructionData, Expr, ExprKind, MatchArm, Pattern, StringPart,
};
use expo_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use expo_ast::labels::{expr_kind_label, pattern_kind_label, pattern_span};

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
        ExprKind::Closure { params, body, .. } => {
            seal_closure_params(params, expr);
            for stmt in body {
                seal_statement(stmt);
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
        // `synthesize` rewrites statement-position fors and
        // resolve diagnoses expression-position fors; seal should
        // never see one.
        ExprKind::For { .. } => seal_panic(
            "typecheck seal saw an `ExprKind::For` after synthesize",
            expr.span,
        ),
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
        ExprKind::List { elements } => {
            for element in elements {
                seal_expr(element);
            }
        }
        ExprKind::Literal { .. } => {}
        ExprKind::Loop { body } => {
            for stmt in body {
                seal_statement(stmt);
            }
        }
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                seal_expr(key);
                seal_expr(value);
            }
        }
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
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                seal_receive_arm(arm);
            }
            if let Some(timeout) = after_timeout {
                seal_expr(timeout);
            }
            for stmt in after_body {
                seal_statement(stmt);
            }
        }
        ExprKind::Self_ { .. } => {}
        ExprKind::ShortClosure { params, body } => {
            seal_closure_params(params, expr);
            seal_expr(body);
        }
        ExprKind::Spawn { expr: inner } => seal_expr(inner),
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
    }
}

/// Receive arms always carry a typed-binding pattern with `local_id`
/// stamped by resolve. Validate the shape, then walk the body. The
/// pattern's annotation `TypeExpr` does not need a separate
/// type-param check — the `local_id`'s scope-recorded
/// `ResolvedType` rides through the body's `Resolution::Local`
/// references and is checked there.
fn seal_receive_arm(arm: &MatchArm) {
    match &arm.pattern {
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            ..
        } => {
            if local_id.is_none() {
                seal_panic(
                    &format!("receive arm binding `{name}` missing local_id after typecheck",),
                    pattern_span(&arm.pattern),
                );
            }
            if resolved_type.is_none() {
                seal_panic(
                    &format!("receive arm binding `{name}` missing resolved_type after typecheck",),
                    pattern_span(&arm.pattern),
                );
            }
        }
        other => seal_panic(
            &format!(
                "typecheck seal expected a typed-binding receive arm pattern, got `{}`",
                pattern_kind_label(other),
            ),
            pattern_span(&arm.pattern),
        ),
    }
    if let Some(guard) = &arm.guard {
        seal_expr(guard);
    }
    for stmt in &arm.body {
        seal_statement(stmt);
    }
}

/// Each closure `Name` param must have its `local_id` stamped by
/// resolve so IR lower can find the binding without re-walking. The
/// AST type-expr annotation, if any, is enforced via the closure's
/// outer `Expr.resolution` (an `AnonymousKind::Function` with each
/// param's resolved type) — already checked by the top-level
/// `seal_no_type_param` walk.
fn seal_closure_params(params: &[ClosureParam], outer: &Expr) {
    for param in params {
        if let ClosureParam::Name {
            local_id: None,
            name,
            ..
        } = param
        {
            seal_panic(
                &format!("closure parameter `{name}` missing local_id after typecheck"),
                outer.span,
            );
        }
    }
}

/// Seal the callee of a `Call`. Two shapes are accepted:
/// - Bare `Ident { Global(_) | Local(_) }` — the outer
///   `Expr.resolution` stays `Unresolved`; resolve carve-out for
///   "function names aren't values yet".
/// - `FieldAccess` with a fn-typed `Expr.resolution` — produced by
///   the field-as-callable rewrite in `resolve_method_call_expr`.
fn seal_call_callee(callee: &Expr) {
    match &callee.kind {
        ExprKind::Ident { name, resolution } => {
            if matches!(resolution, Resolution::Unresolved) {
                seal_panic(
                    &format!("callee `{name}` has Unresolved resolution after typecheck"),
                    callee.span,
                );
            }
        }
        ExprKind::FieldAccess { .. } => {
            if !matches!(
                callee.resolution,
                ResolvedType::Anonymous(AnonymousKind::Function { .. }),
            ) {
                seal_panic(
                    "field-access callee passed typecheck without a fn-typed resolution",
                    callee.span,
                );
            }
        }
        other => seal_panic(
            &format!(
                "call site has a non-identifier callee `{}` that passed typecheck",
                expr_kind_label(other),
            ),
            callee.span,
        ),
    }
}
