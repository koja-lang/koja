//! Lowering for conditional control-flow constructs.
//!
//! Each lowering is a recursive [`Lowerer`] method that takes a
//! `&mut CFGBuilder` plus the currently-open [`IRBlockId`], mints
//! fresh per-construct block ids, mutates the builder to wire the
//! arms, and returns the new open block (the merge) plus the result
//! operand.
//!
//! The canonicalization invariant from [`crate::blocks`] is enforced
//! at exactly one site per construct: the entry-block terminator's
//! `then` / `otherwise` slot assignment.

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_typecheck::types::Type;

use crate::Lowerer;
use crate::blocks::{IRBlockId, IRTerminator};
use crate::cfg::CFGBuilder;
use crate::values::{IRInstruction, IROperand};

/// Trailing-value capture for a value-producing arm: `(exit_block,
/// (operand, expo_type))` -- the IR block where the arm exits plus
/// the operand carrying its trailing-expression value (when the arm
/// produces one).
type ArmExit = (Option<IRBlockId>, Option<(IROperand, Type)>);

impl<'a> Lowerer<'a> {
    /// Lower an `unless cond ... end` statement. Statement-shaped
    /// (returns [`IROperand::Unit`]).
    ///
    /// Wires the entry block (caller-supplied `open`) to a
    /// `CondBranch { cond, then: merge, otherwise: body }` -- putting
    /// the body on `otherwise` is the entire structural content of
    /// "unless-ness". Returns `(Some(merge), Unit)` when execution
    /// continues, or `(None, Unit)` when lowering the cond / body
    /// terminates all paths.
    pub fn lower_unless(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        cond: &Expr,
        body: &[Statement],
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let (open, cond_op, _) = self.lower_expr_to_operand(builder, open, cond)?;
        let Some(open) = open else {
            return Ok((None, IROperand::Unit));
        };

        let body_id = self.next_block_id();
        let merge_id = self.next_block_id();

        builder.set_terminator(
            open,
            IRTerminator::CondBranch {
                cond: cond_op,
                then: merge_id,
                otherwise: body_id,
            },
        );
        self.lower_body_block(builder, body_id, "unless_body", body, merge_id)?;
        builder.add_block(merge_id, "unless_end");
        Ok((Some(merge_id), IROperand::Unit))
    }

    /// Lower an `if cond ... end` statement *with no else arm*.
    /// Mirror of [`Self::lower_unless`] -- polarity difference is the
    /// slot the body lands on in the entry `CondBranch`.
    pub fn lower_if_no_else(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        cond: &Expr,
        body: &[Statement],
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let (open, cond_op, _) = self.lower_expr_to_operand(builder, open, cond)?;
        let Some(open) = open else {
            return Ok((None, IROperand::Unit));
        };

        let body_id = self.next_block_id();
        let merge_id = self.next_block_id();

        builder.set_terminator(
            open,
            IRTerminator::CondBranch {
                cond: cond_op,
                then: body_id,
                otherwise: merge_id,
            },
        );
        self.lower_body_block(builder, body_id, "if_body", body, merge_id)?;
        builder.add_block(merge_id, "ifcont");
        Ok((Some(merge_id), IROperand::Unit))
    }

    /// Lower an `if cond ... else ... end` expression. Shape 2 --
    /// two body blocks plus a value merge.
    ///
    /// Both arms lower through [`Self::lower_statements_for_value`];
    /// when both arms produce trailing-expression values, a
    /// [`IRInstruction::Phi`] is appended to the merge block and the
    /// returned operand references it. Mixed value / no-value arms
    /// (or arms that early-terminate via `return` / `break`) yield
    /// `IROperand::Unit` and a statement-shaped merge.
    pub fn lower_if_else(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        cond: &Expr,
        then_body: &[Statement],
        else_body: &[Statement],
        result_ty: Type,
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let (open, cond_op, _) = self.lower_expr_to_operand(builder, open, cond)?;
        let Some(open) = open else {
            return Ok((None, IROperand::Unit));
        };

        let then_id = self.next_block_id();
        let else_id = self.next_block_id();
        let merge_id = self.next_block_id();

        builder.set_terminator(
            open,
            IRTerminator::CondBranch {
                cond: cond_op,
                then: then_id,
                otherwise: else_id,
            },
        );

        let (then_exit, then_value) =
            self.lower_value_arm(builder, then_id, "then", then_body, merge_id)?;
        let (else_exit, else_value) =
            self.lower_value_arm(builder, else_id, "else", else_body, merge_id)?;

        builder.add_block(merge_id, "ifcont");

        // Phi only when both arms produced a trailing operand AND
        // their cursors reached the merge (didn't terminate). Type
        // matching is intentionally permissive (mirrors legacy):
        // executor verifies LLVM-type compatibility at emission time.
        let result = match (then_exit, else_exit, &then_value, &else_value) {
            (Some(t_exit), Some(e_exit), Some((t_op, _)), Some((e_op, _))) => {
                let dest = self.next_value_id();
                builder.append(
                    merge_id,
                    IRInstruction::Phi {
                        dest,
                        incomings: vec![(t_exit, t_op.clone()), (e_exit, e_op.clone())],
                        ty: result_ty,
                    },
                );
                IROperand::Local(dest)
            }
            _ => IROperand::Unit,
        };
        Ok((Some(merge_id), result))
    }

    /// Lower a `cond ? then_expr : else_expr` ternary. Shape 2 with
    /// the convenient property that both arms are pure expressions,
    /// so each arm's lowering is a `lower_expr_to_operand` call.
    /// Always produces a value (typecheck rejects unifiable arms).
    pub fn lower_ternary(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        cond: &Expr,
        then_expr: &Expr,
        else_expr: &Expr,
        ty: Type,
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let (open, cond_op, _) = self.lower_expr_to_operand(builder, open, cond)?;
        let Some(open) = open else {
            return Ok((None, IROperand::Unit));
        };

        let then_id = self.next_block_id();
        let else_id = self.next_block_id();
        let merge_id = self.next_block_id();

        builder.set_terminator(
            open,
            IRTerminator::CondBranch {
                cond: cond_op,
                then: then_id,
                otherwise: else_id,
            },
        );

        builder.add_block(then_id, "tern_then");
        let (then_exit, then_op, _) = self.lower_expr_to_operand(builder, then_id, then_expr)?;
        if let Some(t_exit) = then_exit {
            builder.set_terminator(t_exit, IRTerminator::Branch(merge_id));
        }

        builder.add_block(else_id, "tern_else");
        let (else_exit, else_op, _) = self.lower_expr_to_operand(builder, else_id, else_expr)?;
        if let Some(e_exit) = else_exit {
            builder.set_terminator(e_exit, IRTerminator::Branch(merge_id));
        }

        builder.add_block(merge_id, "tern_cont");
        let result = match (then_exit, else_exit) {
            (Some(t_exit), Some(e_exit)) => {
                let dest = self.next_value_id();
                builder.append(
                    merge_id,
                    IRInstruction::Phi {
                        dest,
                        incomings: vec![(t_exit, then_op), (e_exit, else_op)],
                        ty,
                    },
                );
                IROperand::Local(dest)
            }
            _ => IROperand::Unit,
        };
        Ok((Some(merge_id), result))
    }

    /// Lower a `cond ... end` expression. N-arm generalization of
    /// [`Self::lower_if_else`]. Each arm gets a check block
    /// (containing the cond lift + a `CondBranch`) plus a body block;
    /// the optional `else` arm gets one body block. Failure edges
    /// chain: arm[i].check.otherwise -> arm[i+1].check (or else /
    /// merge for the last arm).
    pub fn lower_cond(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        arms: &[CondArm],
        else_body: Option<&[Statement]>,
        result_ty: Type,
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        debug_assert!(
            !arms.is_empty(),
            "lower_cond invoked with no arms; shim must guard the empty-and-no-else case",
        );

        let merge_id = self.next_block_id();
        let arm_check_ids: Vec<_> = (0..arms.len()).map(|_| self.next_block_id()).collect();
        let arm_body_ids: Vec<_> = (0..arms.len()).map(|_| self.next_block_id()).collect();
        let else_id = else_body.map(|_| self.next_block_id());

        // Branch from caller's open block into the first arm's check.
        builder.set_terminator(open, IRTerminator::Branch(arm_check_ids[0]));

        let mut arm_exits: Vec<ArmExit> = Vec::with_capacity(arms.len());

        for (i, arm) in arms.iter().enumerate() {
            let check_id = arm_check_ids[i];
            let body_id = arm_body_ids[i];
            let next_id = if i + 1 < arms.len() {
                arm_check_ids[i + 1]
            } else if let Some(eid) = else_id {
                eid
            } else {
                merge_id
            };

            builder.add_block(check_id, "cond_check");
            let (check_exit, cond_op, _) =
                self.lower_expr_to_operand(builder, check_id, &arm.condition)?;
            if let Some(check_exit) = check_exit {
                builder.set_terminator(
                    check_exit,
                    IRTerminator::CondBranch {
                        cond: cond_op,
                        then: body_id,
                        otherwise: next_id,
                    },
                );
            }
            let (body_exit, body_value) =
                self.lower_value_arm(builder, body_id, "cond_body", &arm.body, merge_id)?;
            arm_exits.push((body_exit, body_value));
        }

        let (else_exit, else_value) = match (else_id, else_body) {
            (Some(eid), Some(body)) => {
                self.lower_value_arm(builder, eid, "cond_else", body, merge_id)?
            }
            _ => (None, None),
        };

        builder.add_block(merge_id, "cond_end");

        // All-or-nothing phi: every arm + else (when present) must
        // contribute, else the construct is statement-shaped.
        let result = self.try_stage_cond_phi(
            builder,
            merge_id,
            &arm_exits,
            else_id.is_some(),
            else_exit,
            else_value.as_ref(),
            result_ty,
        );
        Ok((Some(merge_id), result))
    }

    /// Lower a value-arm body block: add the block to the builder,
    /// lower stmts via [`Self::lower_statements_for_value`], close
    /// the exit (when present) with `Branch(merge)`, return the exit
    /// + trailing value pair for the merge phi.
    fn lower_value_arm(
        &mut self,
        builder: &mut CFGBuilder,
        id: IRBlockId,
        label: &str,
        stmts: &[Statement],
        merge_id: IRBlockId,
    ) -> Result<ArmExit, String> {
        builder.add_block(id, label);
        let (exit, value) = self.lower_statements_for_value(builder, id, stmts)?;
        if let Some(exit) = exit {
            builder.set_terminator(exit, IRTerminator::Branch(merge_id));
        }
        Ok((exit, value))
    }

    /// Append a merge [`IRInstruction::Phi`] at `merge_id` iff every
    /// arm + the else (when present) produced a trailing value with
    /// reachable exit. Returns `IROperand::Local(phi.dest)` on
    /// success or `IROperand::Unit` for statement-shaped constructs.
    #[allow(clippy::too_many_arguments)]
    fn try_stage_cond_phi(
        &mut self,
        builder: &mut CFGBuilder,
        merge_id: IRBlockId,
        arm_exits: &[ArmExit],
        has_else: bool,
        else_exit: Option<IRBlockId>,
        else_value: Option<&(IROperand, Type)>,
        ty: Type,
    ) -> IROperand {
        if !has_else {
            return IROperand::Unit;
        }
        if arm_exits
            .iter()
            .any(|(exit, value)| exit.is_none() || value.is_none())
        {
            return IROperand::Unit;
        }
        if else_exit.is_none() || else_value.is_none() {
            return IROperand::Unit;
        }
        let mut incomings: Vec<(IRBlockId, IROperand)> = Vec::with_capacity(arm_exits.len() + 1);
        for (exit, value) in arm_exits {
            let exit = exit.expect("guarded by any(is_none) check above");
            let (op, _) = value.as_ref().expect("guarded by any(is_none) check above");
            incomings.push((exit, op.clone()));
        }
        let e_exit = else_exit.expect("guarded above");
        let (e_op, _) = else_value.expect("guarded above");
        incomings.push((e_exit, e_op.clone()));
        let dest = self.next_value_id();
        builder.append(
            merge_id,
            IRInstruction::Phi {
                dest,
                incomings,
                ty,
            },
        );
        IROperand::Local(dest)
    }
}
