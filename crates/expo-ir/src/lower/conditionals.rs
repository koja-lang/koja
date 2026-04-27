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

use expo_ast::ast::{Expr, Statement};

use crate::Lowerer;
use crate::blocks::IRTerminator;
use crate::resolved::conditionals::{IRIf, IRUnless};

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
}
