//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use expo_ast::identifier::Identifier;

use crate::types::{ConstValue, IRBinOp, IRUnaryOp, ValueId};

/// A lowered function. Body is a list of basic blocks; `blocks[0]` is
/// the entry block. The POC scope only ever emits a single block per
/// function; multi-block lowering lands with control-flow constructs.
///
/// `params` lists the `ValueId` bound to each positional parameter,
/// in declaration order. These ids are the first ones allocated by
/// the function's lowering, so `function.params` always holds a
/// prefix of the function's defined `ValueId`s. The POC does not
/// yet lower body references to parameters (see alpha typecheck's
/// "identifier references in function bodies" diagnostic), but the
/// allocation shape is in place so the next slice can drop in a
/// `Local` read instruction without reshuffling.
#[derive(Debug, Clone)]
pub struct IRFunction {
    pub blocks: Vec<IRBasicBlock>,
    pub identifier: Identifier,
    pub params: Vec<ValueId>,
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

/// How a basic block ends. The POC scope only emits `Return`; branch
/// terminators land with control flow.
#[derive(Debug, Clone, PartialEq)]
pub enum IRTerminator {
    Return { value: Option<ValueId> },
}
