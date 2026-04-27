//! Resolved metadata for conditional control-flow constructs.
//!
//! Each lowering produces a value describing the function-scoped
//! [`IRBlockId`]s the construct names, the terminators that finish
//! those blocks, and the AST stubs emission still walks for
//! expressions and statement bodies.
//!
//! Constructs in this module honor the canonicalization invariant
//! documented in [`crate::blocks`]: control-flow negation is encoded
//! by branch-target ordering on [`IRTerminator::CondBranch`]; no
//! construct emits a `Not` operator or a `negated` flag.

use expo_ast::ast::Statement;

use crate::blocks::{IRBlockId, IRTerminator};
use crate::values::IRInstruction;

/// Outcome of lowering an `unless cond ... end` statement.
///
/// The construct names three blocks:
///
/// - `entry_block` — the block emission is positioned at when the
///   walker starts. Holds `entry_instructions` (the lowered cond
///   expression's instruction sequence) followed by
///   `entry_terminator` (the canonicalized cond-branch).
/// - `body_block` — runs when `cond` is **falsy**. Holds the
///   `unless` body's statements as an AST stub.
/// - `merge_block` — landing point after the construct. Not
///   terminated by this construct (whatever follows the `unless`
///   decides that), so it appears as an [`IRBlockId`] only.
///
/// `entry_terminator` is always
/// `IRTerminator::CondBranch { cond, then: merge_block, otherwise:
/// body_block }`. Putting the body block on `otherwise` is the entire
/// structural content of "unless-ness." `body_terminator` is
/// `IRTerminator::Branch(merge_block)`, the declared end of the body
/// block; emission honors it only when the body has not already
/// terminated itself (e.g. via early `return` or `panic`).
///
/// Fields are stored as parallel slots (an [`IRBlockId`], an
/// instruction sequence, and a terminator) rather than embedded in
/// [`crate::blocks::IRBasicBlock`] values. Structurally identical to
/// [`IRIf`]; the only difference is which slot the body lands on
/// (`otherwise` here, `then` for `IRIf`). Both dissolve in slice 5+
/// when [`crate::blocks::IRBasicBlock`] is promoted to first-class
/// and `body_stmts` retires (statement-level lowering).
pub struct IRUnless {
    pub body_block: IRBlockId,
    pub body_stmts: Vec<Statement>,
    pub body_terminator: IRTerminator,
    pub entry_block: IRBlockId,
    pub entry_instructions: Vec<IRInstruction>,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
}

/// Outcome of lowering an `if cond ... end` statement *with no else
/// arm*. The else-bearing form (and ternary) is a Shape 2 construct
/// with two body blocks plus a value merge; that lift is slice 3 and
/// produces a separate IR type.
///
/// Structurally identical to [`IRUnless`]; the only difference is
/// which slot the body lands on. Three blocks:
///
/// - `entry_block` — the block emission is positioned at when the
///   walker starts. Holds `entry_instructions` (the lowered cond
///   expression's instruction sequence) followed by
///   `entry_terminator` (the canonicalized cond-branch).
/// - `body_block` — runs when `cond` is **truthy**. Holds the
///   `if` body's statements as an AST stub.
/// - `merge_block` — landing point after the construct. Not
///   terminated by this construct, so it appears as an
///   [`IRBlockId`] only.
///
/// `entry_terminator` is always
/// `IRTerminator::CondBranch { cond, then: body_block, otherwise:
/// merge_block }`. Putting the body block on `then` is the entire
/// structural content of `if`-no-else polarity (the mirror of
/// [`IRUnless`]). `body_terminator` is
/// `IRTerminator::Branch(merge_block)`, the declared end of the
/// body block; emission honors it only when the body has not
/// already terminated itself (e.g. via early `return` or `panic`).
///
/// Both [`IRUnless`] and `IRIf` dissolve in slice 5+ when
/// [`crate::blocks::IRBasicBlock`] is promoted to first-class and
/// `body_stmts` retires (statement-level lowering). Until then, the
/// duplication is the cost of direct construct names; the truly
/// construct-agnostic emission mechanic (`execute_instructions`)
/// is shared at the `expo-codegen` seam.
pub struct IRIf {
    pub body_block: IRBlockId,
    pub body_stmts: Vec<Statement>,
    pub body_terminator: IRTerminator,
    pub entry_block: IRBlockId,
    pub entry_instructions: Vec<IRInstruction>,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
}
