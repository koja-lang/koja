//! Operand model for ExpoIR: SSA value identifiers, operands, and the
//! instruction enum.
//!
//! ## Design
//!
//! - [`IRValueId`] is the SSA-style handle for a value produced inside
//!   a function body. Function-scoped, opaque to lowering and emission
//!   alike. Minted by [`crate::FnLowerState::next_value_id`].
//! - [`IROperand`] is what instruction and terminator slots hold when
//!   they want to refer to a value. Either a previously-produced
//!   [`IRValueId`] (via `Local`) or an inline literal constant.
//!   Literals do not need an instruction to produce them.
//! - [`IRInstruction`] is the per-block instruction enum. It carries
//!   typed variants for each [`expo_ast::ast::ExprKind`] that has
//!   learned to lower, plus a transitional [`IRInstruction::Stub`]
//!   that bridges to AST-level expression emission for kinds that
//!   haven't lifted yet. Each future Expr kind retires `Stub` for
//!   that kind by introducing a typed variant and replacing `Stub` at
//!   its lowering site. When the last consumer is gone, `Stub` is
//!   deleted in one PR.
//!
//! ## Why a transitional `Stub` variant
//!
//! The same rationale that justified Wave 11's AST-stub bodies on
//! [`crate::resolved::conditionals::IRUnless`] applies one level
//! finer: the IR scaffolding lands ahead of the full instruction set
//! so each construct can lift in isolation. The alternative -- block
//! every operand-shaped slot until the entire instruction set is
//! defined -- would force a single mega-slice that designs the IR
//! against speculation rather than real consumers.
//!
//! Side tables were considered (and rejected) for the bridge: they
//! divorce execution order from the instruction stream and require
//! the consumer to consult two stores. A first-class `Stub` variant
//! keeps the stream single-source-of-truth and gives the migration a
//! clear, greppable retirement marker.

use expo_ast::ast::Expr;

use crate::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};

/// Function-scoped SSA value identifier. Minted by
/// [`crate::FnLowerState::next_value_id`]. Per-function counters
/// reset at function entry, so ids are only meaningful within their
/// owning function's lowering/emission context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IRValueId(pub u32);

/// What an instruction or terminator references when it wants a
/// value. Either a previously-produced [`IRValueId`] or an inline
/// literal constant.
///
/// Constants do not require an instruction to produce them. Lowering
/// emits the literal variants directly; emission materializes them
/// to backend constants on demand.
#[derive(Clone, Debug)]
pub enum IROperand {
    /// Boolean literal. Emitted by lowering when [`crate::lower::values::lower_expr_to_operand`]
    /// recognizes a `true` / `false` literal in operand position.
    ConstBool(bool),
    /// Floating-point literal.
    ConstFloat(f64),
    /// Integer literal.
    ConstInt(i64),
    /// String literal.
    ConstStr(String),
    /// Reference to a value produced earlier in the same function by
    /// an [`IRInstruction`].
    Local(IRValueId),
    /// The unit value. Backends materialize this however their unit
    /// representation requires (a zero-sized struct, an `i8 0`, etc.).
    Unit,
}

/// A single instruction in a basic block's instruction sequence.
///
/// Variants are alpha-sorted. The transitional [`IRInstruction::Stub`]
/// variant bridges to AST-level expression emission for kinds that
/// haven't lifted yet; each future Expr kind that learns to lower
/// replaces its `Stub` site with a typed instruction variant. When
/// the last consumer is gone, `Stub` is deleted.
#[derive(Clone, Debug)]
pub enum IRInstruction {
    /// Binary arithmetic, comparison, or logical operation. The
    /// [`ResolvedBinaryOp`] variant fully encodes both operand kind
    /// (Int vs Float vs String) and result kind (comparisons -> Bool,
    /// arithmetic -> operand kind), so emission needs no further
    /// decision logic.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::Binary`]. Concat
    /// (multi-block memcpy) and `EnumStructEqual` (multi-block
    /// per-variant equality) are not handled by this variant -- they
    /// fall through to [`IRInstruction::Stub`] until they get
    /// dedicated instruction variants.
    BinaryOp {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Resolved operation -- maps 1:1 to a single LLVM builder call.
        op: ResolvedBinaryOp,
        /// Left-hand operand.
        lhs: IROperand,
        /// Right-hand operand.
        rhs: IROperand,
    },
    /// **Transitional.** Bridges to AST-level expression emission
    /// while the rest of the instruction set fills in. The emission
    /// walker computes the LLVM value for `expr` via
    /// `compile_expr` and registers it under `dest` in the per-block
    /// value map. Subsequent operands referencing `IROperand::Local(dest)`
    /// resolve via the same map.
    ///
    /// Retirement: as each [`expo_ast::ast::ExprKind`] learns to
    /// lower, replace its `Stub` site with a typed `IRInstruction`
    /// variant. When the last consumer is gone, this variant is
    /// deleted. Greppable on the symbol `IRInstruction::Stub`.
    Stub {
        /// SSA destination this instruction produces. Subsequent
        /// operands reference it via [`IROperand::Local`].
        dest: IRValueId,
        /// AST expression to evaluate at emission time. Boxed
        /// because [`Expr`] is large (~280 bytes) and would
        /// otherwise dominate the enum's discriminant size.
        expr: Box<Expr>,
    },
    /// Unary negation or logical-not. The [`ResolvedUnaryOp`] variant
    /// encodes both the operand kind (Int vs Float) and which LLVM
    /// builder call to issue.
    UnaryOp {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Resolved operation -- maps 1:1 to a single LLVM builder call.
        op: ResolvedUnaryOp,
        /// Operand to apply the unary op to.
        operand: IROperand,
    },
}

impl IRInstruction {
    /// SSA destination this instruction writes. Useful for emission
    /// walkers populating a `HashMap<IRValueId, _>`.
    pub fn dest(&self) -> IRValueId {
        match self {
            IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Stub { dest, .. }
            | IRInstruction::UnaryOp { dest, .. } => *dest,
        }
    }
}
