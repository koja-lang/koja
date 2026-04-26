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
//!   `CondBranch` variant carries the condition expression as an AST
//!   stub today; once expression lowering arrives, the stub is replaced
//!   with an instruction-level value.
//! - [`IRBasicBlock`] pairs an id, a debug label, and a terminator. It
//!   is the cell that an instruction sequence will eventually populate.
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

use expo_ast::ast::Expr;

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
    /// Two-target conditional jump. The `cond` expression is evaluated
    /// by emission and used as the i1 selector; control transfers to
    /// `then` when truthy and `otherwise` when falsy.
    CondBranch {
        /// Condition expression. Held as an AST stub until expression
        /// lowering produces an instruction-level value. Boxed because
        /// [`Expr`] is large (~280 bytes) and would otherwise dominate
        /// the enum's discriminant size.
        cond: Box<Expr>,
        /// Target taken when `cond` is truthy.
        then: IRBlockId,
        /// Target taken when `cond` is falsy.
        otherwise: IRBlockId,
    },
    /// Block always diverges (e.g. via a `panic` call). Backends emit
    /// LLVM's `unreachable` or the equivalent.
    Unreachable,
}

/// A basic block: an id, a human-readable debug label, and a single
/// terminator. The instruction sequence between block entry and the
/// terminator is currently empty; condition expressions and statement
/// bodies are walked by `expo-codegen`'s existing emission paths
/// against AST stubs carried on the surrounding construct.
pub struct IRBasicBlock {
    pub id: IRBlockId,
    pub label: String,
    pub terminator: IRTerminator,
}
