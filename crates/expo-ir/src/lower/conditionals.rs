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
use crate::blocks::IRTerminator;
use crate::resolved::conditionals::{IRCond, IRCondArm, IRIf, IRIfElse, IRTernary, IRUnless};
use crate::values::IRInstruction;

impl<'a> Lowerer<'a> {
    /// Lowers an `unless cond ... end` statement.
    ///
    /// Mints three fresh [`IRBlockId`](crate::blocks::IRBlockId)s
    /// (`entry`, `body`, `merge`) on the per-function block counter
    /// and records the canonicalized branch:
    ///
    /// - Entry terminator: `CondBranch { cond, then: merge, otherwise:
    ///   body }`. Putting body on `otherwise` is the entire structural
    ///   content of "unless-ness" -- no `Not` operator is emitted at any
    ///   stage.
    /// - Body terminator: `Branch(merge)`. This is the *declared* end of
    ///   the body block; emission honors it iff the body has not already
    ///   self-terminated (e.g. via early `return` / `panic`).
    ///
    /// The condition is lowered to an
    /// [`IROperand`](crate::values::IROperand) via
    /// [`Self::lower_expr_to_operand`]: literals become inline
    /// operand constants emitting no instructions; non-literal cond
    /// expressions emit typed instructions
    /// ([`IRInstruction::BinaryOp`](crate::values::IRInstruction::BinaryOp),
    /// [`IRInstruction::FieldLoad`](crate::values::IRInstruction::FieldLoad),
    /// etc.) or fall through to
    /// [`IRInstruction::Stub`](crate::values::IRInstruction::Stub) and
    /// reference the result via
    /// [`IROperand::Local`](crate::values::IROperand::Local). Body
    /// statements remain AST stubs walked by `expo-codegen` until
    /// statement-level lowering arrives.
    pub fn lower_unless(&mut self, cond: &Expr, body: &[Statement]) -> IRUnless {
        let entry_block = self.next_block_id();
        let body_block = self.next_block_id();
        let merge_block = self.next_block_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        let entry_terminator = IRTerminator::CondBranch {
            cond: cond_operand,
            then: merge_block,
            otherwise: body_block,
        };
        let body_terminator = IRTerminator::Branch(merge_block);

        IRUnless {
            body_block,
            body_stmts: body.to_vec(),
            body_terminator,
            entry_block,
            entry_instructions,
            entry_terminator,
            merge_block,
        }
    }

    /// Lowers an `if cond ... end` statement *with no else arm*. The
    /// else-bearing form (`if cond ... else ... end`, plus ternary)
    /// is a Shape 2 construct with two body blocks and a value
    /// merge; that lift is slice 3.
    ///
    /// Mints three fresh [`IRBlockId`](crate::blocks::IRBlockId)s
    /// (`entry`, `body`, `merge`) on the per-function block counter
    /// and records the canonicalized branch:
    ///
    /// - Entry terminator: `CondBranch { cond, then: body, otherwise:
    ///   merge }`. Putting body on `then` is the entire structural
    ///   content of `if`-no-else polarity (the mirror of
    ///   [`Self::lower_unless`]).
    /// - Body terminator: `Branch(merge)`. This is the *declared* end
    ///   of the body block; emission honors it iff the body has not
    ///   already self-terminated (e.g. via early `return` / `panic`).
    ///
    /// The condition flows through the same construct-agnostic
    /// [`Self::lower_expr_to_operand`] seam used by
    /// [`Self::lower_unless`].
    pub fn lower_if_no_else(&mut self, cond: &Expr, body: &[Statement]) -> IRIf {
        let entry_block = self.next_block_id();
        let body_block = self.next_block_id();
        let merge_block = self.next_block_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        let entry_terminator = IRTerminator::CondBranch {
            cond: cond_operand,
            then: body_block,
            otherwise: merge_block,
        };
        let body_terminator = IRTerminator::Branch(merge_block);

        IRIf {
            body_block,
            body_stmts: body.to_vec(),
            body_terminator,
            entry_block,
            entry_instructions,
            entry_terminator,
            merge_block,
        }
    }

    /// Lowers an `if cond ... else ... end` expression. Shape 2 --
    /// two body blocks plus a value merge.
    ///
    /// Mints four fresh
    /// [`IRBlockId`](crate::blocks::IRBlockId)s (`entry`, `then`,
    /// `else`, `merge`) and pre-allocates an
    /// [`IRValueId`](crate::values::IRValueId) for the merge phi's
    /// destination so the emit walker can synthesize the
    /// [`IRInstruction::Phi`] without minting fresh ids
    /// mid-emission. `merge_phi_ty` is the construct's resolved
    /// expression type (drives `to_llvm_type` at emit).
    ///
    /// The phi itself is **not** pre-staged: `then_stmts` /
    /// `else_stmts` still walk through `compile_statement` until
    /// Phase 4g lifts statement-level lowering, so the per-arm
    /// trailing-expression value isn't visible from this seam. The
    /// emit walker constructs the phi at merge time after both arms
    /// have been compiled (and falls back to `Ok(None)` when either
    /// arm is statement-only or diverges, mirroring today's
    /// [`compile_if`](../../../../expo-codegen/src/control/conditionals.rs)
    /// behavior).
    pub fn lower_if_else(
        &mut self,
        cond: &Expr,
        then_body: &[Statement],
        else_body: &[Statement],
        merge_phi_ty: Type,
    ) -> IRIfElse {
        let entry_block = self.next_block_id();
        let then_block = self.next_block_id();
        let else_block = self.next_block_id();
        let merge_block = self.next_block_id();
        let merge_phi_dest = self.next_value_id();

        let mut entry_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut entry_instructions, cond);

        IRIfElse {
            else_block,
            else_stmts: else_body.to_vec(),
            else_terminator: IRTerminator::Branch(merge_block),
            entry_block,
            entry_instructions,
            entry_terminator: IRTerminator::CondBranch {
                cond: cond_operand,
                then: then_block,
                otherwise: else_block,
            },
            merge_block,
            merge_phi_dest,
            merge_phi_ty,
            then_block,
            then_stmts: then_body.to_vec(),
            then_terminator: IRTerminator::Branch(merge_block),
        }
    }

    /// Lowers a `cond ? then_expr : else_expr` ternary. Shape 2 with
    /// the convenient property that both arms are pure expressions,
    /// so each arm's instruction sequence + result operand are
    /// known at lowering time -- no AST stubs survive.
    ///
    /// Pre-stages a single [`IRInstruction::Phi`] in
    /// `merge_instructions` whose incomings are
    /// `[(then_block, then_value), (else_block, else_value)]`.
    /// Ternary always produces a value (typecheck rejects arms
    /// whose types don't unify), so the phi is unconditional --
    /// emission just runs `merge_instructions` through
    /// `execute_instructions`.
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
    /// the shape-2 conditional pattern from
    /// [`Self::lower_if_else`].
    ///
    /// Mints fresh
    /// [`IRBlockId`](crate::blocks::IRBlockId)s for every arm's
    /// `check_block` and `body_block` (the first arm's check_block
    /// is treated as the construct's implicit entry by emission;
    /// see [`IRCondArm`] doc), an optional `else_block`, the shared
    /// `merge_block`, and pre-allocates an
    /// [`IRValueId`](crate::values::IRValueId) for the merge phi's
    /// destination.
    ///
    /// Per arm: lowers `arm.condition` into `check_instructions`
    /// via [`Self::lower_expr_to_operand`] and builds the
    /// canonicalized branch
    /// `CondBranch { cond, then: body_block, otherwise: <next> }`,
    /// where `<next>` resolves to:
    ///
    /// - `arms[i+1].check_block` for `i < N-1`
    /// - `else_block` for the last arm when else is present
    /// - `merge_block` for the last arm when no else is present
    ///
    /// Bodies remain AST `Vec<Statement>` stubs walked by emission
    /// until Phase 4g lifts statement-level lowering.
    pub fn lower_cond(
        &mut self,
        arms: &[CondArm],
        else_body: Option<&[Statement]>,
        merge_phi_ty: Type,
    ) -> IRCond {
        debug_assert!(
            !arms.is_empty(),
            "lower_cond invoked with no arms; shim must guard the empty-and-no-else case",
        );

        let merge_block = self.next_block_id();
        let merge_phi_dest = self.next_value_id();

        let check_blocks: Vec<_> = (0..arms.len()).map(|_| self.next_block_id()).collect();
        let body_blocks: Vec<_> = (0..arms.len()).map(|_| self.next_block_id()).collect();
        let else_block = else_body.map(|_| self.next_block_id());

        let lowered_arms: Vec<IRCondArm> = arms
            .iter()
            .enumerate()
            .map(|(i, arm)| {
                let mut check_instructions = Vec::new();
                let cond_operand =
                    self.lower_expr_to_operand(&mut check_instructions, &arm.condition);

                let next_block = if i + 1 < arms.len() {
                    check_blocks[i + 1]
                } else if let Some(eb) = else_block {
                    eb
                } else {
                    merge_block
                };

                IRCondArm {
                    body_block: body_blocks[i],
                    body_stmts: arm.body.clone(),
                    body_terminator: IRTerminator::Branch(merge_block),
                    check_block: check_blocks[i],
                    check_instructions,
                    check_terminator: IRTerminator::CondBranch {
                        cond: cond_operand,
                        then: body_blocks[i],
                        otherwise: next_block,
                    },
                }
            })
            .collect();

        IRCond {
            arms: lowered_arms,
            else_block,
            else_stmts: else_body.map(<[Statement]>::to_vec),
            else_terminator: else_block.map(|_| IRTerminator::Branch(merge_block)),
            merge_block,
            merge_phi_dest,
            merge_phi_ty,
        }
    }
}
