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

use koja_ast::ast::{LValue, Statement};
use koja_ast::visit::{self, Visitor};

use crate::function::{IRBlockId, IRInstruction};
use crate::local::IRLocalId;
use crate::types::IRType;

use super::ctx::FnLowerCtx;
use super::patterns::pattern_binding_ids;

/// Clone every heap-managed bind in `binds` that `body` assigns
/// through, write the owned copy back into its slot, and clear its
/// borrowed marking. Emitted at the head of the arm's body block,
/// before the body's own statements lower.
pub(super) fn detach_mutated_binds(
    binds: &[(IRLocalId, IRType)],
    body: &[Statement],
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) {
    if !binds.iter().any(|(_, ty)| ty.is_heap_managed()) {
        return;
    }
    let mut assigned = AssignedLocals::default();
    visit::walk_body(&mut assigned, body);
    for (local, ty) in binds {
        // Only borrowed slots need the detach. A bind that already
        // owns its value (a binary-match bind writing a fresh block)
        // must keep it, as cloning over it would leak the original.
        if !ty.is_heap_managed()
            || !assigned.locals.contains(local)
            || !ctx.slot_is_borrowed(*local)
        {
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

/// Every local a body writes, including writes inside nested bodies
/// (loops, conditionals, nested matches, closures). Assignment and
/// destructuring rebind an existing name in place, so a whole-slot
/// write can hit a bind just like a field write can. Non-bind ids
/// never intersect the bind set.
#[derive(Default)]
struct AssignedLocals {
    locals: BTreeSet<IRLocalId>,
}

impl<'ast> Visitor<'ast> for AssignedLocals {
    fn visit_lvalue(&mut self, lvalue: &'ast LValue) {
        if let Some(local_id) = lvalue.local_id {
            self.locals.insert(IRLocalId::from_local_id(local_id));
        }
    }

    fn visit_statement(&mut self, statement: &'ast Statement) {
        if let Statement::Destructure { pattern, .. } = statement {
            self.locals.extend(
                pattern_binding_ids(pattern)
                    .into_iter()
                    .map(IRLocalId::from_local_id),
            );
        }
        visit::walk_statement(self, statement);
    }
}
