//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use std::borrow::Borrow;
use std::fmt;

use expo_ast::identifier::Identifier;

use crate::types::{ConstValue, IRBinOp, IRType, IRUnaryOp, ValueId};

/// The IR's stable, backend-facing handle for a callable. Stamped at
/// lower time and used as the key on [`crate::IRPackage::functions`]
/// and the callee field on [`IRInstruction::Call`]. Backends index by
/// this directly (per the northstar's
/// "consumer-builds-its-own-indices" contract); they never reach back
/// into `expo-ast` to derive it from an [`Identifier`] themselves.
///
/// Construction is `pub(crate)` on purpose. The IR crate stamps each
/// `IRSymbol` exactly once during `lower_package` from the AST-layer
/// [`Identifier`]; downstream consumers (eval, codegen, tests) only
/// read. That single seam is where the mangling rule lives, so the
/// next slice that introduces e.g. monomorphization suffixes or
/// extern-symbol overrides changes the encoding in one well-defined
/// place without any consumer noticing.
///
/// Today the inner string is the [`Identifier::qualified_name`] of
/// the function. Backends consume it via [`Self::mangled`] (or via
/// [`AsRef<str>`] / [`Borrow<str>`] — both implemented for ergonomic
/// `BTreeMap<IRSymbol, _>::get(&str)` lookups). The crate-private
/// inner field forbids ad-hoc re-derivation in any other crate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRSymbol(String);

impl IRSymbol {
    /// Mint an `IRSymbol` from the canonical AST identifier of a
    /// declaration. Crate-private: only the alpha lowering pipeline
    /// stamps symbols.
    pub(crate) fn from_identifier(identifier: &Identifier) -> Self {
        Self(identifier.qualified_name())
    }

    /// The mangled symbol name as a borrowed `&str`. Backends pass
    /// this directly to LLVM (`module.get_function`,
    /// `module.add_function`) or to any other linker-aware lookup.
    /// Cheap — just borrows the inner string. Named for the role,
    /// not the type, so future readers see "mangled name" at every
    /// call site rather than "stringified symbol".
    pub fn mangled(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for IRSymbol {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for IRSymbol {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IRSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A single positional parameter of an [`IRFunction`]. `id` is the
/// pre-allocated [`ValueId`] body lowering binds the param under (the
/// first ids of the function are reserved for params, in declaration
/// order); `ty` is the parameter's static [`IRType`] so backends can
/// size the LLVM signature and the seeded SSA value map without
/// re-querying the typecheck registry.
///
/// Distinct from v1's `expo_ir::IRParam` enum — same crate-namespace
/// concept, different shape. Renaming this struct here keeps cross-crate
/// readers from being confused by two `IRParam`s with different
/// vocabularies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IRFunctionParam {
    pub id: ValueId,
    pub ty: IRType,
}

/// A lowered function. Body is a list of basic blocks; `blocks[0]` is
/// the entry block. Today's scope emits a single block per function;
/// multi-block lowering lands with control-flow constructs.
///
/// `symbol` is the function's stable, backend-facing handle (see
/// [`IRSymbol`]). It's the lookup key on [`crate::IRPackage::functions`]
/// and the value [`IRInstruction::Call::callee`] points at. Backends
/// consume the [`IRSymbol::mangled`] view to declare / look up the
/// LLVM function; the entry point is the one exception (it's
/// exported under the host-runtime name, e.g. `main` on Unix).
///
/// `params` lists the [`IRFunctionParam`] bound to each positional
/// parameter, in declaration order. The carried `ValueId`s are the
/// first ones allocated for the function, so `function.params` always
/// holds a prefix of its defined `ValueId`s. Body references to
/// parameters are not yet lowered (see alpha typecheck's "identifier
/// references in function bodies" diagnostic); the allocation shape
/// is in place so the next slice can drop in a `Local` read
/// instruction without reshuffling.
///
/// `return_type` is the static type of the function's return value.
/// Backends consume this directly — LLVM codegen reads it to pick the
/// function signature and `ret iN` width without re-querying the
/// typecheck registry.
#[derive(Debug, Clone)]
pub struct IRFunction {
    pub blocks: Vec<IRBasicBlock>,
    pub params: Vec<IRFunctionParam>,
    pub return_type: IRType,
    pub symbol: IRSymbol,
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
    /// `dest = callee(args)`. The callee is identified by its stable
    /// [`IRSymbol`]; the interpreter / codegen dereference that
    /// through the enclosing `IRProgram` (or `IRScript`) to reach the
    /// target function. No AST [`Identifier`] survives into the IR
    /// vocabulary — the symbol is the contract.
    Call {
        dest: ValueId,
        callee: IRSymbol,
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
