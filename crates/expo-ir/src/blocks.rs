//! Instruction-level IR scaffolding: block identifiers, basic blocks, and
//! terminators.
//!
//! The vocabulary is construct-agnostic:
//!
//! - [`IRBlockId`] is a function-scoped identifier minted by
//!   [`crate::FnLowerState::next_block_id`]. It is opaque to lowering and
//!   emission alike; emission keys a `HashMap<IRBlockId, BasicBlock<'ctx>>`
//!   off it.
//! - [`IRTerminator`] names every way a basic block can finish. The
//!   `CondBranch` variant carries the condition as an [`IROperand`];
//!   lowering produces literal operands directly and bridges
//!   non-literal expressions through [`crate::values::IRInstruction::Stub`].
//! - [`IRBasicBlock`] pairs an id, a debug label, an instruction
//!   sequence, and a single terminator. The instruction sequence is
//!   the destination for lowered expressions; the terminator names
//!   the block's successor(s) (if any).
//!
//! ## Architectural invariant: canonicalized branch-target ordering
//!
//! Control-flow negation is canonicalized into branch-target ordering.
//! There is no `Not` operator and no `negated` flag in this module;
//! constructs that conceptually negate a branch condition (such as
//! `unless`) instead swap the `then` and `otherwise` slots of an
//! [`IRTerminator::CondBranch`]. This keeps the cond-branch shape
//! uniform across every conditional construct in the language so
//! backends implement one cond-branch lowering and reuse it everywhere.

use crate::values::{IRInstruction, IROperand};

/// Function-scoped basic block identifier. Minted by
/// [`crate::FnLowerState::next_block_id`]. Per-function counters reset
/// at function entry, so ids are only meaningful within their owning
/// function's lowering/emission context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IRBlockId(pub u32);

/// How a basic block finishes. A block has exactly one terminator; the
/// terminator names the successor block(s) (if any).
pub enum IRTerminator {
    /// Unconditional jump to `target`.
    Branch(IRBlockId),
    /// Two-target conditional jump. The `cond` operand is coerced to
    /// an i1 selector by emission; control transfers to `then` when
    /// truthy and `otherwise` when falsy. Lowering produces literal
    /// [`IROperand`] variants directly and bridges non-literal
    /// expressions through [`crate::values::IRInstruction::Stub`]
    /// instructions on the owning block.
    CondBranch {
        /// Condition operand. Either a literal constant or a
        /// reference to a value produced by an earlier instruction
        /// in the same function.
        cond: IROperand,
        /// Target taken when `cond` is truthy.
        then: IRBlockId,
        /// Target taken when `cond` is falsy.
        otherwise: IRBlockId,
    },
    /// Block always diverges (e.g. via a `panic` call). Backends emit
    /// LLVM's `unreachable` or the equivalent.
    Unreachable,
}

/// A basic block: an id, a human-readable debug label, an instruction
/// sequence, and a single terminator.
///
/// Today the type is defined for forward-compat: constructs still
/// store parallel `IRBlockId` + `Vec<IRInstruction>` + `IRTerminator`
/// fields directly on their `IR*` value (see
/// [`crate::resolved::conditionals::IRUnless`]). When a second
/// construct lifts (slice 2, `compile_if` no else), the duplication
/// motivates promoting `IRBasicBlock` to first-class: constructs hold
/// `IRBasicBlock` values and the parallel-field shape goes away.
pub struct IRBasicBlock {
    pub id: IRBlockId,
    pub instructions: Vec<IRInstruction>,
    pub label: String,
    pub terminator: IRTerminator,
}
