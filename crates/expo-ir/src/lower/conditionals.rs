//! Lowering for conditional control-flow constructs.
//!
//! Each lowering is a pure-semantic function: it mints fresh
//! [`IRBlockId`](crate::blocks::IRBlockId)s on the per-function
//! counter and records the canonicalized branch decisions on the
//! corresponding `IR*` value. Lowerings here do no name resolution,
//! so they take only a mutable [`FnLowerState`] reference rather
//! than a [`LowerCtx`](crate::lower::ctx::LowerCtx).
//!
//! The canonicalization invariant from [`crate::blocks`] is enforced
//! at exactly one site per construct: the entry-block terminator's
//! `then` / `otherwise` slot assignment.

use expo_ast::ast::{Expr, Statement};

use crate::FnLowerState;
use crate::blocks::IRTerminator;
use crate::lower::values::lower_expr_to_operand;
use crate::resolved::conditionals::IRUnless;

/// Lowers an `unless cond ... end` statement.
///
/// Mints three fresh [`IRBlockId`](crate::blocks::IRBlockId)s
/// (`entry`, `body`, `merge`) on `state`'s per-function block counter
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
/// The condition is lowered to an [`IROperand`](crate::values::IROperand)
/// via [`lower_expr_to_operand`]: literals become inline operand
/// constants emitting no instructions; non-literal cond expressions
/// emit one [`IRInstruction::Stub`](crate::values::IRInstruction::Stub)
/// onto `entry_instructions` and reference it via
/// [`IROperand::Local`](crate::values::IROperand::Local). Body
/// statements remain AST stubs walked by `expo-codegen` until
/// statement-level lowering arrives.
pub fn lower_unless(state: &mut FnLowerState, cond: &Expr, body: &[Statement]) -> IRUnless {
    let entry_block = state.next_block_id();
    let body_block = state.next_block_id();
    let merge_block = state.next_block_id();

    let mut entry_instructions = Vec::new();
    let cond_operand = lower_expr_to_operand(state, &mut entry_instructions, cond);

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
