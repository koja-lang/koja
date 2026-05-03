//! Small value types used throughout the IR vocabulary: value handles,
//! constant payloads, binary-op kinds, and the IR type lattice.

/// Identifier of an SSA value within a single function. Values are
/// numbered in definition order starting from 0; the same `ValueId`
/// has no meaning across functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValueId(pub u32);

impl std::fmt::Display for ValueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Compile-time-known constant payload that an [`crate::IRInstruction::Const`]
/// loads into a fresh `ValueId`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Bool(bool),
    Int(i64),
    Unit,
}

/// Binary arithmetic operators. The POC scope ships only the integer
/// arithmetic set; comparison / logical / concat lands as features grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRBinOp {
    Add,
    Div,
    Mod,
    Mul,
    Sub,
}

/// The IR type lattice. Defined here so the vocabulary has a stable
/// place for type annotations (return types on `IRFunction`, parameter
/// types on future `Call` instructions, etc.) but **not yet wired into
/// `IRFunction`** for the POC — eval reads the runtime type off the
/// returned [`ConstValue`] and codegen has not been rewired to v2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IRType {
    Bool,
    Int,
    Unit,
}
