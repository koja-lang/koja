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
use expo_typecheck::types::Type;

use crate::blocks::{IRBlockId, IRTerminator};
use crate::values::{IRInstruction, IROperand, IRValueId};

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

/// Outcome of lowering an `if cond ... else ... end` expression.
/// Shape 2 -- two body blocks plus a value merge. Distinct from
/// [`IRIf`] because the with-else form can flow back as a value:
/// when both arms produce a `TypedValue` of compatible types,
/// emission constructs an [`IRInstruction::Phi`] in `merge_block`
/// whose dest is `merge_phi_dest`. When either arm is
/// statement-shaped (no trailing expression value, or the arm
/// diverges via early return / panic), emission drops the phi and
/// the construct returns `None` -- mirroring today's lenient
/// behavior in `compile_if`.
///
/// Five blocks:
///
/// - `entry_block` -- holds `entry_instructions` (the lowered cond)
///   followed by `entry_terminator`
///   (`CondBranch { cond, then: then_block, otherwise: else_block }`).
/// - `then_block` -- runs when `cond` is truthy. Holds the then-arm
///   statements as an AST stub (`then_stmts`); declared exit is
///   `then_terminator` = `Branch(merge_block)`.
/// - `else_block` -- runs when `cond` is falsy. Holds the else-arm
///   statements as an AST stub (`else_stmts`); declared exit is
///   `else_terminator` = `Branch(merge_block)`.
/// - `merge_block` -- landing point. Emission positions there after
///   walking both arms; if both produced values, an
///   [`IRInstruction::Phi`] is synthesized at emit time (its
///   incomings reference the *actual* end blocks of each arm, which
///   may differ from `then_block` / `else_block` when bodies
///   contain nested control flow).
///
/// `merge_phi_dest` and `merge_phi_ty` are pre-allocated at lowering
/// time so the emit walker can construct the phi without minting a
/// fresh value id mid-emission. Pre-allocation also ensures the
/// dest stays stable if a future slice fans the merge instruction
/// out for inspection (e.g. ownership analysis in Phase 6).
///
/// Dissolves in Phase 4g together with [`IRUnless`] / [`IRIf`] when
/// `IRBasicBlock` becomes first-class and `then_stmts` / `else_stmts`
/// retire (statement-level lowering).
pub struct IRIfElse {
    pub else_block: IRBlockId,
    pub else_stmts: Vec<Statement>,
    pub else_terminator: IRTerminator,
    pub entry_block: IRBlockId,
    pub entry_instructions: Vec<IRInstruction>,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
    pub merge_phi_dest: IRValueId,
    pub merge_phi_ty: Type,
    pub then_block: IRBlockId,
    pub then_stmts: Vec<Statement>,
    pub then_terminator: IRTerminator,
}

/// Outcome of lowering a `cond ? then_expr : else_expr` ternary.
/// Shape 2 (same shape as [`IRIfElse`]) but each arm is a single
/// expression rather than a statement body, so lowering can fully
/// instructionize both arms -- no AST stubs survive into the IR.
///
/// Five blocks (same skeleton as [`IRIfElse`]):
///
/// - `entry_block` -- `entry_instructions` + `entry_terminator`
///   (`CondBranch { cond, then: then_block, otherwise: else_block }`).
/// - `then_block` -- `then_instructions` produce `then_value`,
///   followed by `then_terminator` = `Branch(merge_block)`.
/// - `else_block` -- mirror of then.
/// - `merge_block` -- always holds exactly one
///   [`IRInstruction::Phi`] in `merge_instructions` whose dest is
///   `merge_value` and whose incomings are
///   `[(then_block, then_value), (else_block, else_value)]`.
///   Ternary always produces a value (typecheck rejects arms whose
///   types don't unify), so unlike [`IRIfElse`] the phi is
///   unconditional.
///
/// Distinct from [`IRIfElse`] per invariant 4 ("direct construct
/// names over premature unification"): structurally the two share
/// the entry/then/else/merge skeleton but differ on the arm-body
/// representation (statements vs instructions) and on whether the
/// merge is conditional. Both dissolve into the same shape in
/// Phase 4g.
pub struct IRTernary {
    pub else_block: IRBlockId,
    pub else_instructions: Vec<IRInstruction>,
    pub else_terminator: IRTerminator,
    pub else_value: IROperand,
    pub entry_block: IRBlockId,
    pub entry_instructions: Vec<IRInstruction>,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
    pub merge_instructions: Vec<IRInstruction>,
    pub merge_value: IRValueId,
    pub then_block: IRBlockId,
    pub then_instructions: Vec<IRInstruction>,
    pub then_terminator: IRTerminator,
    pub then_value: IROperand,
}
