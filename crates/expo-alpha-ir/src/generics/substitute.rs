//! AST-side substitution helpers for monomorphization.
//!
//! [`substitute_in_function`] walks every [`ResolvedType`] slot
//! reachable from a function's body — `Expr.resolution`, the
//! `type_args` carried on call/method-call expressions — and rewrites
//! it via [`super::substitute_resolved_type`]. Mono drives this on a
//! cloned [`Function`] before re-lowering, so the body sees concrete
//! resolutions everywhere a `TypeParam` previously stood.
//!
//! [`substitute_signature`] does the same for a [`FunctionSignature`]
//! — params and return type — yielding the substituted signature
//! [`crate::lower::package::lower_function_inner`] needs.

use expo_alpha_typecheck::{FunctionSignature, ResolvedParam};
use expo_ast::ast::{
    AssignTarget, EnumConstructionData, Expr, ExprKind, Function, Statement, StringPart,
};
use expo_ast::identifier::{GlobalRegistryId, ResolvedType};

use super::substitute_resolved_type;

/// Substitute every [`ResolvedType`] reachable from `function`'s body
/// in place. Caller is responsible for cloning before substituting if
/// the original needs to stay intact (mono always clones).
pub(super) fn substitute_in_function(
    function: &mut Function,
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    for stmt in body {
        substitute_in_statement(stmt, args, owner);
    }
}

/// Clone `signature` with every `params[].ty` and `return_type`
/// rewritten via [`substitute_resolved_type`]. Used by mono to feed
/// [`crate::lower::package::lower_function_inner`] a concrete shape
/// without mutating the registry-owned template.
pub(super) fn substitute_signature(
    signature: &FunctionSignature,
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) -> FunctionSignature {
    FunctionSignature {
        dispatch: signature.dispatch,
        params: signature
            .params
            .iter()
            .map(|param| ResolvedParam {
                name: param.name.clone(),
                ty: substitute_resolved_type(&param.ty, args, owner),
            })
            .collect(),
        return_type: substitute_resolved_type(&signature.return_type, args, owner),
    }
}

fn substitute_in_statement(stmt: &mut Statement, args: &[ResolvedType], owner: GlobalRegistryId) {
    match stmt {
        Statement::Assignment { target, value, .. } => {
            if let AssignTarget::LValue(_) = target {
                // LValues carry no ResolvedType slots today.
            }
            substitute_in_expr(value, args, owner);
        }
        Statement::Break { .. } => {}
        Statement::CompoundAssign { value, .. } => substitute_in_expr(value, args, owner),
        Statement::Expr(expr) => substitute_in_expr(expr, args, owner),
        Statement::Return { value: None, .. } => {}
        Statement::Return {
            value: Some(value), ..
        } => substitute_in_expr(value, args, owner),
    }
}

fn substitute_in_expr(expr: &mut Expr, args: &[ResolvedType], owner: GlobalRegistryId) {
    expr.resolution = substitute_resolved_type(&expr.resolution, args, owner);
    match &mut expr.kind {
        ExprKind::Binary { left, right, .. } => {
            substitute_in_expr(left, args, owner);
            substitute_in_expr(right, args, owner);
        }
        ExprKind::Call {
            callee,
            args: call_args,
            type_args,
        } => {
            substitute_in_expr(callee, args, owner);
            for arg in call_args {
                substitute_in_expr(&mut arg.value, args, owner);
            }
            for ty in type_args {
                *ty = substitute_resolved_type(ty, args, owner);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => {
                for field in fields {
                    substitute_in_expr(&mut field.value, args, owner);
                }
            }
            EnumConstructionData::Tuple(exprs) => {
                for inner in exprs {
                    substitute_in_expr(inner, args, owner);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::FieldAccess { receiver, .. } => substitute_in_expr(receiver, args, owner),
        ExprKind::Group { expr: inner } => substitute_in_expr(inner, args, owner),
        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            substitute_in_expr(condition, args, owner);
            for stmt in then_body {
                substitute_in_statement(stmt, args, owner);
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    substitute_in_statement(stmt, args, owner);
                }
            }
        }
        ExprKind::MethodCall {
            receiver,
            args: call_args,
            type_args,
            ..
        } => {
            substitute_in_expr(receiver, args, owner);
            for arg in call_args {
                substitute_in_expr(&mut arg.value, args, owner);
            }
            for ty in type_args {
                *ty = substitute_resolved_type(ty, args, owner);
            }
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    substitute_in_expr(expr, args, owner);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                substitute_in_expr(&mut field.value, args, owner);
            }
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                substitute_in_expr(&mut arm.condition, args, owner);
                for stmt in &mut arm.body {
                    substitute_in_statement(stmt, args, owner);
                }
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    substitute_in_statement(stmt, args, owner);
                }
            }
        }
        ExprKind::Match { subject, arms } => {
            // Today's supported patterns carry no `ResolvedType`
            // slots (wildcards / literals / bindings are leaves), so
            // the pattern walk is a no-op; only the subject and arm
            // bodies need substitution.
            substitute_in_expr(subject, args, owner);
            for arm in arms {
                for stmt in &mut arm.body {
                    substitute_in_statement(stmt, args, owner);
                }
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            substitute_in_expr(condition, args, owner);
            substitute_in_expr(then_expr, args, owner);
            substitute_in_expr(else_expr, args, owner);
        }
        ExprKind::Unary { operand, .. } => substitute_in_expr(operand, args, owner),
        ExprKind::Unless { condition, body } => {
            substitute_in_expr(condition, args, owner);
            for stmt in body {
                substitute_in_statement(stmt, args, owner);
            }
        }
        // Feature gaps surface during lowering — no substitution needed
        // for shapes the lowering pass refuses to translate. Listed
        // explicitly so a future ExprKind addition is a compile error
        // rather than a silent miss.
        ExprKind::BinaryLiteral { .. }
        | ExprKind::Closure { .. }
        | ExprKind::For { .. }
        | ExprKind::List { .. }
        | ExprKind::Loop { .. }
        | ExprKind::Map { .. }
        | ExprKind::Receive { .. }
        | ExprKind::ShortClosure { .. }
        | ExprKind::Spawn { .. }
        | ExprKind::While { .. } => {}
    }
}
