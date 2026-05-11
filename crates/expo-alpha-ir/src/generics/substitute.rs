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

fn substitute_in_statements(
    body: &mut [Statement],
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) {
    for stmt in body {
        substitute_in_statement(stmt, args, owner);
    }
}

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
    substitute_in_statements(body, args, owner);
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
                mode: param.mode,
                name: param.name.clone(),
                ty: substitute_resolved_type(&param.ty, args, owner),
            })
            .collect(),
        return_type: substitute_resolved_type(&signature.return_type, args, owner),
        impl_args: signature
            .impl_args
            .iter()
            .map(|ty| substitute_resolved_type(ty, args, owner))
            .collect(),
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
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                substitute_in_expr(&mut segment.value, args, owner);
                if let Some(size) = segment.size.as_mut() {
                    substitute_in_expr(size, args, owner);
                }
            }
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
        ExprKind::Closure { body, .. } => substitute_in_statements(body, args, owner),
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                substitute_in_expr(&mut arm.condition, args, owner);
                substitute_in_statements(&mut arm.body, args, owner);
            }
            if let Some(else_body) = else_body {
                substitute_in_statements(else_body, args, owner);
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
        ExprKind::For { iterable, body, .. } => {
            substitute_in_expr(iterable, args, owner);
            substitute_in_statements(body, args, owner);
        }
        ExprKind::Group { expr: inner } => substitute_in_expr(inner, args, owner),
        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            substitute_in_expr(condition, args, owner);
            substitute_in_statements(then_body, args, owner);
            if let Some(else_body) = else_body {
                substitute_in_statements(else_body, args, owner);
            }
        }
        ExprKind::List { elements } => {
            for element in elements {
                substitute_in_expr(element, args, owner);
            }
        }
        ExprKind::Loop { body } => substitute_in_statements(body, args, owner),
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                substitute_in_expr(key, args, owner);
                substitute_in_expr(value, args, owner);
            }
        }
        ExprKind::Match { subject, arms } => {
            // Supported patterns carry no `ResolvedType` slots
            // (wildcards / literals / bindings / enum constructors
            // / struct destructures are leaves or carry only paths
            // and named-field patterns), so the pattern walk is a
            // no-op; the subject, arm guards, and arm bodies need
            // substitution.
            substitute_in_expr(subject, args, owner);
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    substitute_in_expr(guard, args, owner);
                }
                substitute_in_statements(&mut arm.body, args, owner);
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
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    substitute_in_expr(guard, args, owner);
                }
                substitute_in_statements(&mut arm.body, args, owner);
            }
            if let Some(timeout) = after_timeout.as_mut() {
                substitute_in_expr(timeout, args, owner);
            }
            substitute_in_statements(after_body, args, owner);
        }
        ExprKind::ShortClosure { body, .. } => substitute_in_expr(body, args, owner),
        ExprKind::Spawn { expr: inner } => substitute_in_expr(inner, args, owner),
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
            substitute_in_statements(body, args, owner);
        }
        ExprKind::While { condition, body } => {
            substitute_in_expr(condition, args, owner);
            substitute_in_statements(body, args, owner);
        }
    }
}
