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

use expo_typecheck::types::Type;

use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator};
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
/// Structurally identical to [`IRIf`]; the only difference is which
/// slot the body lands on (`otherwise` here, `then` for `IRIf`). Both
/// dissolve into a flat `Vec<IRBasicBlock>` in Slice 3.
pub struct IRUnless {
    pub body: IRBasicBlock,
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
/// Both [`IRUnless`] and `IRIf` dissolve in Slice 3 when
/// `IRFunction.blocks: Vec<IRBasicBlock>` lands. Until then, the
/// duplication is the cost of direct construct names; the truly
/// construct-agnostic emission mechanic (`execute_instructions`) is
/// shared at the `expo-codegen` seam.
pub struct IRIf {
    pub body: IRBasicBlock,
    pub entry_block: IRBlockId,
    pub entry_instructions: Vec<IRInstruction>,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
}

/// Outcome of lowering an `if cond ... else ... end` expression.
/// Shape 2 -- two body blocks plus a value merge. Distinct from
/// [`IRIf`] because the with-else form can flow back as a value via
/// `merge_instructions` (a pre-staged [`IRInstruction::Phi`]).
///
/// Five blocks:
///
/// - `entry_block` -- holds `entry_instructions` (the lowered cond)
///   followed by `entry_terminator`
///   (`CondBranch { cond, then: then.id, otherwise: else_arm.id }`).
/// - `then` -- runs when `cond` is truthy. Full IR block.
/// - `else_arm` -- runs when `cond` is falsy. Full IR block.
/// - `merge_block` -- landing point. Holds `merge_instructions`
///   (the pre-staged Phi when both arms produced values; empty
///   otherwise -- the construct is statement-shaped).
///
/// The Phi's incomings reference the *nominal* arm block ids; the
/// emit walker remaps those to the actual end-of-arm `BasicBlock`s
/// before running `merge_instructions` (same idiom as `IRTernary`).
/// `merge_value` is the SSA dest of the pre-staged phi when present.
pub struct IRIfElse {
    pub else_arm: IRBasicBlock,
    pub entry_block: IRBlockId,
    pub entry_instructions: Vec<IRInstruction>,
    pub entry_terminator: IRTerminator,
    pub merge_block: IRBlockId,
    pub merge_instructions: Vec<IRInstruction>,
    pub merge_value: Option<IRValueId>,
    pub result_ty: Type,
    pub then: IRBasicBlock,
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

/// One arm of an [`IRCond`]: a check (cond expression + canonicalized
/// branch) plus a body (statement list with declared exit terminator).
///
/// The arm's two blocks:
///
/// - `check_block` -- holds `check_instructions` (the lowered cond
///   expression) followed by `check_terminator`
///   (`CondBranch { cond, then: body_block, otherwise: <next> }`,
///   where `<next>` is the next arm's `check_block`, the cond's
///   `else_block` if this is the last arm and an else is present, or
///   the cond's `merge_block` if this is the last arm with no else).
/// - `body_block` -- runs when this arm's cond is truthy. Holds the
///   arm body's statements as an AST stub (`body_stmts`); declared
///   exit is `body_terminator` = `Branch(merge_block)`. Emission
///   honors the terminator only when the body has not already
///   self-terminated (e.g. via early `return` / `panic`).
///
/// The first arm in an `IRCond.arms` vec is structurally distinct in
/// emission: its `check_block` is the cond's *implicit entry*, and
/// `emit_cond` does not allocate a fresh LLVM block for it
/// (`check_instructions` execute at the call-site builder position,
/// matching the same convention used by [`IRUnless`] / [`IRIf`] /
/// [`IRIfElse`] / [`IRTernary`] for their `entry_*` slots). Arms
/// 1..N each get a fresh LLVM block.
pub struct IRCondArm {
    pub body: IRBasicBlock,
    pub check_block: IRBlockId,
    pub check_instructions: Vec<IRInstruction>,
    pub check_terminator: IRTerminator,
}

/// Outcome of lowering a `cond ... end` expression. N-arm
/// generalization of the shape-2 conditional pattern from
/// [`IRIfElse`]: arms chain via each arm's `check_terminator`
/// `otherwise` slot pointing at the next arm's `check_block`, with
/// the tail arm's `otherwise` pointing at `else_block` (when
/// present) or `merge_block` (otherwise). Eliminates the legacy
/// `compile_cond`'s `fallthrough_bb` artifact -- the no-else case
/// goes from the last arm's check directly to merge.
///
/// Blocks:
///
/// - `arms[0].check_block` -- implicit entry; no LLVM block
///   allocated (see [`IRCondArm`] doc).
/// - `arms[1..N].check_block` -- fresh LLVM blocks per arm.
/// - `arms[*].body` -- one fresh LLVM block per arm.
/// - `else_arm` -- present iff the source `cond` had an `else`
///   clause. Full IR block.
/// - `merge_block` -- landing point. Holds `merge_instructions`
///   (pre-staged Phi when every arm + else produced a value with
///   matching Expo type; empty otherwise -- the construct is
///   statement-shaped).
///
/// Value-merge contract is all-or-nothing (matches legacy
/// `compile_cond` semantics): either every reachable arm + the
/// else (when present) contributes to the phi, or the construct
/// returns `Ok(None)`. Typecheck catches mismatched arm types at
/// the source level.
///
/// `merge_value` is the SSA dest of the pre-staged phi when
/// present. The phi's incomings reference the *nominal* arm block
/// ids; emission remaps to actual end-of-arm `BasicBlock`s.
///
/// `arms` is non-empty by construction -- the parser produces a
/// `cond` with at least one arm, and the shim guards
/// `arms.is_empty() && else_body.is_none()` before lowering.
pub struct IRCond {
    pub arms: Vec<IRCondArm>,
    pub else_arm: Option<IRBasicBlock>,
    pub merge_block: IRBlockId,
    pub merge_instructions: Vec<IRInstruction>,
    pub merge_value: Option<IRValueId>,
    pub result_ty: Type,
}
