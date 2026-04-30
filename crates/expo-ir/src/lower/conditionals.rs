//! Lowering for conditional control-flow constructs.
//!
//! Each lowering is a pure-semantic [`Lowerer`] method: it mints
//! fresh [`IRBlockId`](crate::blocks::IRBlockId)s on the per-function
//! counter and records the canonicalized branch decisions on the
//! corresponding `IR*` value. Lowerings here do no name resolution
//! beyond what [`Lowerer::lower_expr_to_operand`] dispatches into.
//!
//! The canonicalization invariant from [`crate::blocks`] is enforced
//! at exactly one site per construct: the entry-block terminator's
//! `then` / `otherwise` slot assignment.

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_typecheck::types::Type;

use crate::Lowerer;
use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator};
use crate::resolved::conditionals::{IRCond, IRCondArm, IRIf, IRIfElse, IRTernary, IRUnless};
use crate::values::{IRInstruction, IROperand, IRValueId};

impl<'a> Lowerer<'a> {
    /// Lowers an `unless cond ... end` statement.
    ///
    /// Mints three fresh [`IRBlockId`](crate::blocks::IRBlockId)s
    /// (`entry`, `body`, `merge`) and records the canonicalized
    /// branch:
    ///
    /// - Entry terminator: `CondBranch { cond, then: merge, otherwise:
    ///   body }`. Putting body on `otherwise` is the entire
    ///   structural content of "unless-ness" -- no `Not` operator is
    ///   emitted at any stage.
    /// - Body block: lowered statement stream with the body's natural
    ///   terminator (early `Return` / `Break`) overriding the
    ///   default `Branch(merge)` back-edge when present.
    pub fn lower_unless(&mut self, cond: &Expr, body: &[Statement]) -> Result<IRUnless, String> {
        let entry_block = self.next_block_id();
        let body_id = self.next_block_id();
        let merge_block = self.next_block_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        let body = self.lower_body_block(body_id, "unless_body", body, merge_block)?;

        Ok(IRUnless {
            body,
            entry_block,
            entry_instructions,
            entry_terminator: IRTerminator::CondBranch {
                cond: cond_operand,
                then: merge_block,
                otherwise: body_id,
            },
            merge_block,
        })
    }

    /// Lowers an `if cond ... end` statement *with no else arm*.
    /// Mirror of [`Self::lower_unless`] -- the polarity difference is
    /// fully encoded by which slot the body block lands on in the
    /// entry `CondBranch`.
    pub fn lower_if_no_else(&mut self, cond: &Expr, body: &[Statement]) -> Result<IRIf, String> {
        let entry_block = self.next_block_id();
        let body_id = self.next_block_id();
        let merge_block = self.next_block_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        let body = self.lower_body_block(body_id, "if_body", body, merge_block)?;

        Ok(IRIf {
            body,
            entry_block,
            entry_instructions,
            entry_terminator: IRTerminator::CondBranch {
                cond: cond_operand,
                then: body_id,
                otherwise: merge_block,
            },
            merge_block,
        })
    }

    /// Lowers an `if cond ... else ... end` expression. Shape 2 --
    /// two body blocks plus a value merge.
    ///
    /// Both arms lower through [`Self::lower_statements_for_value`];
    /// when both arms produce trailing-expression values with
    /// matching Expo types, a [`IRInstruction::Phi`] is pre-staged
    /// in `merge_instructions`. Mixed value / no-value arms (or
    /// arms that early-terminate via `return` / `break`) yield a
    /// statement-shaped construct with empty `merge_instructions`
    /// and `merge_value = None`.
    pub fn lower_if_else(
        &mut self,
        cond: &Expr,
        then_body: &[Statement],
        else_body: &[Statement],
        result_ty: Type,
    ) -> Result<IRIfElse, String> {
        let entry_block = self.next_block_id();
        let then_id = self.next_block_id();
        let else_id = self.next_block_id();
        let merge_block = self.next_block_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        let (then, then_value) = self.lower_value_arm(then_id, "then", then_body, merge_block)?;
        let (else_arm, else_value) =
            self.lower_value_arm(else_id, "else", else_body, merge_block)?;

        // Stage the merge phi whenever both arms produce a trailing
        // value. Type matching is intentionally permissive (we don't
        // unify Expo types here) -- the executor's
        // [`crate::values::IRInstruction::Phi`] arm verifies LLVM
        // type compatibility at emission time and surfaces a clear
        // error on real mismatches. This mirrors the legacy
        // `emit_if_else`'s LLVM-type-keyed phi assembly, which
        // accepted any pair of arm values whose lowered LLVM types
        // matched even when their typecheck-time Expo types
        // differed (e.g. `Result<String, Unknown>` vs
        // `Result<String, Error>` lower to the same union layout).
        let (merge_instructions, merge_value) = match (&then_value, &else_value) {
            (Some((then_op, then_ty)), Some((else_op, _else_ty))) => {
                let dest = self.next_value_id();
                let phi = IRInstruction::Phi {
                    dest,
                    incomings: vec![(then_id, then_op.clone()), (else_id, else_op.clone())],
                    ty: then_ty.clone(),
                };
                (vec![phi], Some(dest))
            }
            _ => (Vec::new(), None),
        };

        Ok(IRIfElse {
            else_arm,
            entry_block,
            entry_instructions,
            entry_terminator: IRTerminator::CondBranch {
                cond: cond_operand,
                then: then_id,
                otherwise: else_id,
            },
            merge_block,
            merge_instructions,
            merge_value,
            result_ty,
            then,
        })
    }

    /// Lowers a `cond ? then_expr : else_expr` ternary. Shape 2 with
    /// the convenient property that both arms are pure expressions,
    /// so each arm's instruction sequence + result operand are
    /// known at lowering time -- no statement-body lowering needed.
    ///
    /// Pre-stages a single [`IRInstruction::Phi`] in
    /// `merge_instructions` whose incomings are
    /// `[(then_block, then_value), (else_block, else_value)]`.
    /// Ternary always produces a value (typecheck rejects arms
    /// whose types don't unify), so the phi is unconditional.
    pub fn lower_ternary(
        &mut self,
        cond: &Expr,
        then_expr: &Expr,
        else_expr: &Expr,
        ty: Type,
    ) -> IRTernary {
        let entry_block = self.next_block_id();
        let then_block = self.next_block_id();
        let else_block = self.next_block_id();
        let merge_block = self.next_block_id();
        let merge_value = self.next_value_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        let mut then_instructions = Vec::new();
        let then_value = self.lower_expr_to_operand(&mut then_instructions, then_expr);

        let mut else_instructions = Vec::new();
        let else_value = self.lower_expr_to_operand(&mut else_instructions, else_expr);

        let merge_instructions = vec![IRInstruction::Phi {
            dest: merge_value,
            incomings: vec![
                (then_block, then_value.clone()),
                (else_block, else_value.clone()),
            ],
            ty,
        }];

        IRTernary {
            else_block,
            else_instructions,
            else_terminator: IRTerminator::Branch(merge_block),
            else_value,
            entry_block,
            entry_instructions,
            entry_terminator: IRTerminator::CondBranch {
                cond: cond_operand,
                then: then_block,
                otherwise: else_block,
            },
            merge_block,
            merge_instructions,
            merge_value,
            then_block,
            then_instructions,
            then_terminator: IRTerminator::Branch(merge_block),
            then_value,
        }
    }

    /// Lowers a `cond ... end` expression. N-arm generalization of
    /// the shape-2 conditional pattern from [`Self::lower_if_else`].
    ///
    /// Bodies + the optional else flow through
    /// [`Self::lower_statements_for_value`]; when **every** arm + the
    /// else (when present) produces a trailing-expression value with
    /// matching Expo types, a [`IRInstruction::Phi`] is pre-staged in
    /// `merge_instructions`. The all-or-nothing contract matches
    /// legacy `compile_cond` semantics; mixed arms yield a
    /// statement-shaped construct.
    pub fn lower_cond(
        &mut self,
        arms: &[CondArm],
        else_body: Option<&[Statement]>,
        result_ty: Type,
    ) -> Result<IRCond, String> {
        debug_assert!(
            !arms.is_empty(),
            "lower_cond invoked with no arms; shim must guard the empty-and-no-else case",
        );

        let merge_block = self.next_block_id();
        let check_block_ids: Vec<_> = (0..arms.len()).map(|_| self.next_block_id()).collect();
        let body_block_ids: Vec<_> = (0..arms.len()).map(|_| self.next_block_id()).collect();
        let else_id = else_body.map(|_| self.next_block_id());

        let mut lowered_arms: Vec<IRCondArm> = Vec::with_capacity(arms.len());
        let mut arm_values: Vec<Option<(IROperand, Type)>> = Vec::with_capacity(arms.len());

        for (i, arm) in arms.iter().enumerate() {
            let mut check_instructions = Vec::new();
            let cond_operand = self.lower_expr_to_operand(&mut check_instructions, &arm.condition);

            let next_block = if i + 1 < arms.len() {
                check_block_ids[i + 1]
            } else if let Some(eb) = else_id {
                eb
            } else {
                merge_block
            };

            let (body, value) =
                self.lower_value_arm(body_block_ids[i], "cond_body", &arm.body, merge_block)?;

            arm_values.push(value);
            lowered_arms.push(IRCondArm {
                body,
                check_block: check_block_ids[i],
                check_instructions,
                check_terminator: IRTerminator::CondBranch {
                    cond: cond_operand,
                    then: body_block_ids[i],
                    otherwise: next_block,
                },
            });
        }

        let (else_arm, else_value) = match (else_id, else_body) {
            (Some(eid), Some(body)) => {
                let (block, value) = self.lower_value_arm(eid, "cond_else", body, merge_block)?;
                (Some(block), value)
            }
            _ => (None, None),
        };

        let (merge_instructions, merge_value) =
            self.try_stage_cond_phi(&arm_values, else_value.as_ref(), &body_block_ids, else_id);

        Ok(IRCond {
            arms: lowered_arms,
            else_arm,
            merge_block,
            merge_instructions,
            merge_value,
            result_ty,
        })
    }

    /// Lower an arm body that may contribute to a merge phi: lowers
    /// via [`Self::lower_statements_for_value`] and packages the
    /// result as a [`IRBasicBlock`] plus the optional trailing
    /// `(operand, type)` for the surrounding pre-stager.
    fn lower_value_arm(
        &mut self,
        id: IRBlockId,
        label: &str,
        stmts: &[Statement],
        default_target: IRBlockId,
    ) -> Result<(IRBasicBlock, Option<(IROperand, Type)>), String> {
        let (instructions, terminator, trailing_op, trailing_ty) =
            self.lower_statements_for_value(stmts)?;
        let block = IRBasicBlock {
            id,
            instructions,
            label: label.to_string(),
            terminator: terminator.unwrap_or(IRTerminator::Branch(default_target)),
        };
        let value = match (trailing_op, trailing_ty) {
            (Some(op), Some(ty)) => Some((op, ty)),
            _ => None,
        };
        Ok((block, value))
    }

    /// Pre-stage the merge [`IRInstruction::Phi`] for a `cond`
    /// expression iff every arm + the else (when present) produced a
    /// trailing value with matching Expo type. Returns
    /// `(merge_instructions, Some(dest))` on success or
    /// `(empty, None)` to signal a statement-shaped construct.
    fn try_stage_cond_phi(
        &mut self,
        arm_values: &[Option<(IROperand, Type)>],
        else_value: Option<&(IROperand, Type)>,
        arm_block_ids: &[IRBlockId],
        else_id: Option<IRBlockId>,
    ) -> (Vec<IRInstruction>, Option<IRValueId>) {
        if arm_values.iter().any(Option::is_none) {
            return (Vec::new(), None);
        }
        if else_id.is_some() && else_value.is_none() {
            return (Vec::new(), None);
        }
        if else_id.is_none() {
            // Without an else clause the all-arm-otherwise edge falls
            // straight into merge, leaving the phi an incoming short.
            // Legacy `compile_cond` keeps such constructs statement-
            // shaped (typecheck enforces `else` for value-producing
            // `cond`).
            return (Vec::new(), None);
        }
        // Type matching is intentionally permissive (matches the
        // [`Self::lower_if_else`] policy): the executor checks LLVM
        // type compatibility at emission time. The phi's `ty` field
        // carries the first arm's Expo type as a label for diagnostics.
        let mut incomings: Vec<(IRBlockId, IROperand)> = Vec::with_capacity(arm_values.len() + 1);
        let mut first_ty: Option<Type> = None;
        for (id, value) in arm_block_ids.iter().zip(arm_values.iter()) {
            let (op, ty) = value.as_ref().expect("guarded by any(is_none) check above");
            first_ty.get_or_insert_with(|| ty.clone());
            incomings.push((*id, op.clone()));
        }
        if let (Some(eid), Some((op, _ty))) = (else_id, else_value) {
            incomings.push((eid, op.clone()));
        }
        let dest = self.next_value_id();
        let ty = first_ty.expect("at least one arm contributes when guards pass");
        (
            vec![IRInstruction::Phi {
                dest,
                incomings,
                ty,
            }],
            Some(dest),
        )
    }
}
