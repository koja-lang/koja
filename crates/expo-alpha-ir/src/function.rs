//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use expo_ast::identifier::Identifier;

use crate::types::{ConstValue, IRBinOp, IRType, IRUnaryOp, ValueId};

/// A lowered function. Body is a list of basic blocks; `blocks[0]` is
/// the entry block. Today's scope emits a single block per function;
/// multi-block lowering lands with control-flow constructs.
///
/// `params` lists the `ValueId` bound to each positional parameter,
/// in declaration order. These ids are the first ones allocated, so
/// `function.params` always holds a prefix of the function's defined
/// `ValueId`s. Body references to parameters are not yet lowered (see
/// alpha typecheck's "identifier references in function bodies"
/// diagnostic); the allocation shape is in place so the next slice
/// can drop in a `Local` read instruction without reshuffling.
///
/// `return_type` is the static type of the function's return value.
/// Backends consume this directly — LLVM codegen reads it to pick the
/// function signature and `ret iN` width without re-querying the
/// typecheck registry. Per-param `IRType` entries are intentionally
/// not carried yet — they land alongside the `Local` reads slice.
#[derive(Debug, Clone)]
pub struct IRFunction {
    pub blocks: Vec<IRBasicBlock>,
    pub identifier: Identifier,
    pub params: Vec<ValueId>,
    pub return_type: IRType,
}

impl IRFunction {
    /// Stable per-function symbol name. Backends use this as their
    /// own lookup key — the northstar's "consumer-builds-its-own-indices"
    /// contract says backends index by mangled name (a `String`),
    /// never by decomposing the [`Identifier`] internals. The entry
    /// point may be exported under a host-runtime symbol (`main` on
    /// Unix); every other function is exported under this name.
    pub fn mangled_name(&self) -> String {
        self.identifier.qualified_name()
    }
}

/// A straight-line sequence of [`IRInstruction`]s that ends in exactly
/// one [`IRTerminator`].
#[derive(Debug, Clone)]
pub struct IRBasicBlock {
    pub instructions: Vec<IRInstruction>,
    pub terminator: IRTerminator,
}

/// A single SSA-style instruction. Every variant defines a fresh value
/// (`dest: ValueId`) and references operands by their `ValueId`.
#[derive(Debug, Clone, PartialEq)]
pub enum IRInstruction {
    /// `dest = lhs <op> rhs`.
    BinaryOp {
        dest: ValueId,
        lhs: ValueId,
        op: IRBinOp,
        rhs: ValueId,
    },
    /// `dest = callee(args)`. The callee is identified by its
    /// canonical [`Identifier`] -- the interpreter / codegen
    /// dereference that through the enclosing `IRProgram` to reach
    /// the target function.
    Call {
        dest: ValueId,
        callee: Identifier,
        args: Vec<ValueId>,
    },
    /// `dest = <constant>`.
    Const { dest: ValueId, value: ConstValue },
    /// `dest = <op> operand`.
    UnaryOp {
        dest: ValueId,
        op: IRUnaryOp,
        operand: ValueId,
    },
}

impl IRInstruction {
    /// The `ValueId` this instruction defines.
    pub fn dest(&self) -> ValueId {
        match self {
            IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::Const { dest, .. }
            | IRInstruction::UnaryOp { dest, .. } => *dest,
        }
    }
}

/// How a basic block ends. Today only `Return` is emitted; branch
/// terminators land with control flow.
#[derive(Debug, Clone, PartialEq)]
pub enum IRTerminator {
    Return { value: Option<ValueId> },
}
