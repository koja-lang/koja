//! Detach match-pattern binds that the arm body mutates.
//!
//! A pattern bind borrows the subject's payload storage (no `Clone`,
//! never dropped). That is only sound while the bind stays read-only.
//! A field assignment through the bind (`bound.field = value`)
//! rebuilds the local and drops the stale leaf, freeing storage the
//! subject still owns, and the subject's own release frees it again.
//!
//! The detach runs once at arm entry, after the guard. Any bind the
//! arm body assigns through gets a `Clone` of its borrowed payload
//! written back to the slot and leaves `borrowed_slots`, making it an
//! independent owner that the normal slot-drop machinery releases.
//! Detaching per-assignment instead would leave the slot's ownership
//! state path-dependent when the mutation sits inside a branch.

use std::collections::BTreeSet;

use koja_ast::ast::{EnumConstructionData, Expr, ExprKind, Statement, StringPart};

use crate::function::{IRBlockId, IRInstruction};
use crate::local::IRLocalId;
use crate::types::IRType;

use super::ctx::FnLowerCtx;

/// Clone every heap-managed bind in `binds` that `body` assigns
/// through, write the owned copy back into its slot, and clear its
/// borrowed marking. Emitted at the head of the arm's body block,
/// before the body's own statements lower.
pub(super) fn detach_mutated_binds(
    binds: &[(IRLocalId, IRType)],
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) {
    if !binds.iter().any(|(_, ty)| ty.is_heap_managed()) {
        return;
    }
    let mut assigned = BTreeSet::new();
    collect_assigned_locals(body, &mut assigned);
    for (local, ty) in binds {
        // Only borrowed slots need the detach. A bind that already
        // owns its value (a binary-match bind writing a fresh block)
        // must keep it, as cloning over it would leak the original.
        if !ty.is_heap_managed() || !assigned.contains(local) || !ctx.slot_is_borrowed(*local) {
            continue;
        }
        let borrowed = ctx.fresh_value(ty.clone());
        ctx.cfg.append(
            block,
            IRInstruction::LocalRead {
                dest: borrowed,
                local: *local,
                ty: ty.clone(),
            },
        );
        let owned = ctx.fresh_value(ty.clone());
        ctx.cfg.append(
            block,
            IRInstruction::Clone {
                dest: owned,
                source: borrowed,
                ty: ty.clone(),
            },
        );
        ctx.cfg.append(
            block,
            IRInstruction::LocalWrite {
                local: *local,
                value: owned,
            },
        );
        ctx.unmark_slot_borrowed(*local);
    }
}

/// Head locals of every assignment statement in `body`, recursing
/// into nested statement bodies (loops, conditionals, nested matches,
/// closures). Single-segment reassignment mints a fresh `LocalId`, so
/// only a field assignment can hit an existing bind slot, but
/// collecting every head keeps the walk simple. Non-bind ids never
/// intersect the bind set.
fn collect_assigned_locals(body: &[Statement], assigned: &mut BTreeSet<IRLocalId>) {
    for statement in body {
        match statement {
            Statement::Assignment { target, value, .. }
            | Statement::CompoundAssign { target, value, .. } => {
                if let Some(local_id) = target.local_id {
                    assigned.insert(IRLocalId::from_local_id(local_id));
                }
                collect_assigned_in_expr(value, assigned);
            }
            Statement::Break { .. } => {}
            Statement::Destructure { value, .. } | Statement::Expr(value) => {
                collect_assigned_in_expr(value, assigned);
            }
            Statement::Return { value, .. } => {
                if let Some(value) = value {
                    collect_assigned_in_expr(value, assigned);
                }
            }
        }
    }
}

fn collect_assigned_in_expr(expr: &Expr, assigned: &mut BTreeSet<IRLocalId>) {
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            collect_assigned_in_expr(left, assigned);
            collect_assigned_in_expr(right, assigned);
        }
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                collect_assigned_in_expr(&segment.value, assigned);
                if let Some(size) = &segment.size {
                    collect_assigned_in_expr(size, assigned);
                }
            }
        }
        ExprKind::Call { callee, args, .. } => {
            collect_assigned_in_expr(callee, assigned);
            for arg in args {
                collect_assigned_in_expr(&arg.value, assigned);
            }
        }
        ExprKind::Closure { body, .. } => collect_assigned_locals(body, assigned),
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                collect_assigned_in_expr(&arm.condition, assigned);
                collect_assigned_locals(&arm.body, assigned);
            }
            if let Some(body) = else_body {
                collect_assigned_locals(body, assigned);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => {
                for field in fields {
                    collect_assigned_in_expr(&field.value, assigned);
                }
            }
            EnumConstructionData::Tuple(elements) => {
                for element in elements {
                    collect_assigned_in_expr(element, assigned);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::Fail { value } => collect_assigned_in_expr(value, assigned),
        ExprKind::FieldAccess { receiver, .. } => collect_assigned_in_expr(receiver, assigned),
        ExprKind::For { iterable, body, .. } => {
            collect_assigned_in_expr(iterable, assigned);
            collect_assigned_locals(body, assigned);
        }
        ExprKind::Group { expr } | ExprKind::Spawn { expr } | ExprKind::Try { expr } => {
            collect_assigned_in_expr(expr, assigned);
        }
        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            collect_assigned_in_expr(condition, assigned);
            collect_assigned_locals(then_body, assigned);
            if let Some(body) = else_body {
                collect_assigned_locals(body, assigned);
            }
        }
        ExprKind::List { elements } | ExprKind::Tuple { elements } => {
            for element in elements {
                collect_assigned_in_expr(element, assigned);
            }
        }
        ExprKind::Loop { body } => collect_assigned_locals(body, assigned),
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                collect_assigned_in_expr(key, assigned);
                collect_assigned_in_expr(value, assigned);
            }
        }
        ExprKind::Match { subject, arms } => {
            collect_assigned_in_expr(subject, assigned);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_assigned_in_expr(guard, assigned);
                }
                collect_assigned_locals(&arm.body, assigned);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            collect_assigned_in_expr(receiver, assigned);
            for arg in args {
                collect_assigned_in_expr(&arg.value, assigned);
            }
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                collect_assigned_locals(&arm.body, assigned);
            }
            if let Some(timeout) = after_timeout {
                collect_assigned_in_expr(timeout, assigned);
            }
            collect_assigned_locals(after_body, assigned);
        }
        ExprKind::Rescue {
            subject, handler, ..
        } => {
            collect_assigned_in_expr(subject, assigned);
            collect_assigned_in_expr(handler, assigned);
        }
        ExprKind::ShortClosure { body, .. } => collect_assigned_in_expr(body, assigned),
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    collect_assigned_in_expr(expr, assigned);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                collect_assigned_in_expr(&field.value, assigned);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_assigned_in_expr(condition, assigned);
            collect_assigned_in_expr(then_expr, assigned);
            collect_assigned_in_expr(else_expr, assigned);
        }
        ExprKind::Unary { operand, .. } => collect_assigned_in_expr(operand, assigned),
        ExprKind::Unless { condition, body } | ExprKind::While { condition, body } => {
            collect_assigned_in_expr(condition, assigned);
            collect_assigned_locals(body, assigned);
        }
    }
}
