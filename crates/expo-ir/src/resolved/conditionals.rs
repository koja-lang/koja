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

/// Outcome of lowering an `unless cond ... end` statement.
///
/// The construct names three blocks:
///
/// - `entry_block` — the block emission is positioned at when the
///   walker starts. The id is minted by lowering so future passes
///   (e.g. predecessor tracking) have a stable name to refer to;
///   emission today still uses the current LLVM builder position
///   directly because no upstream IR producer feeds blocks into
///   emission yet.
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
pub struct IRUnless {
    pub body_block: IRBlockId,
    pub body_stmts: Vec<Statement>,
    pub body_terminator: IRTerminator,
    pub entry_block: IRBlockId,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
}
