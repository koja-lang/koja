//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use std::borrow::Borrow;
use std::fmt;

use expo_ast::identifier::Identifier;

use crate::local::IRLocalId;
use crate::struct_decl::StructFieldInit;
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
/// re-querying the typecheck registry; `local_id` is the slot the
/// param is promoted into at function entry — the lower pass emits a
/// matching [`IRInstruction::LocalDecl`] + [`IRInstruction::LocalWrite`]
/// pair so body references read through the same `LocalRead` path
/// body-declared locals use, and so reassignment of params works
/// uniformly with reassignment of any other local.
///
/// Distinct from v1's `expo_ir::IRParam` enum — same crate-namespace
/// concept, different shape. Renaming this struct here keeps cross-crate
/// readers from being confused by two `IRParam`s with different
/// vocabularies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IRFunctionParam {
    pub id: ValueId,
    pub local_id: IRLocalId,
    pub ty: IRType,
}

/// Function-unique handle for an [`IRBasicBlock`]. Block ids are
/// minted from a per-function counter on `FnLowerCtx`; the same
/// `IRBlockId` value has no meaning across functions. Display renders
/// as `bb<n>`, mirroring the SIL-style convention LLVM debug viewers
/// use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IRBlockId(pub u32);

impl fmt::Display for IRBlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

/// How a function's body is materialized at backend emission time.
///
/// `Regular` carries a non-empty `IRFunction.blocks`; the backend
/// walks the basic blocks and emits the IR instructions.
///
/// `Intrinsic` carries empty `IRFunction.blocks`; the backend looks
/// the function up by [`IRSymbol::mangled`] in its per-backend
/// `intrinsics/` dispatch table and synthesizes the body from a
/// hand-written emitter. Compile-time analogue of `@extern "C"`'s
/// "external symbol resolves at link time" — only the synthesis
/// happens inside the compiler instead.
///
/// Per-kind body shape is enforced by the seal pass: see
/// `seal::function::seal_function`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Intrinsic,
    Regular,
}

/// A lowered function. Body is a list of basic blocks; `blocks[0]` is
/// the entry block. Multi-block bodies appear once control-flow
/// constructs (`if` / `unless`, future loops/match) lower through the
/// `CFGBuilder` lowering path.
///
/// `kind` distinguishes regular fns from `@intrinsic`-annotated ones.
/// `Intrinsic` always pairs with empty `blocks`; the backend looks up
/// `symbol.mangled()` in its `intrinsics/` dispatch table and emits a
/// hand-written body. See [`FunctionKind`].
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
    pub kind: FunctionKind,
    pub params: Vec<IRFunctionParam>,
    pub return_type: IRType,
    pub symbol: IRSymbol,
}

/// A straight-line sequence of [`IRInstruction`]s that ends in exactly
/// one [`IRTerminator`]. `id` is the function-unique handle every
/// terminator targeting this block carries; `label` is a short human
/// hint (`"entry"`, `"if_then"`, `"if_merge"`) the IR text format and
/// LLVM block names borrow.
#[derive(Debug, Clone)]
pub struct IRBasicBlock {
    pub id: IRBlockId,
    pub label: String,
    pub instructions: Vec<IRInstruction>,
    pub terminator: IRTerminator,
}

/// A single SSA-style instruction. Most variants define a fresh
/// value (`dest: ValueId`) and reference operands by their `ValueId`;
/// the local-slot variants ([`IRInstruction::LocalDecl`] /
/// [`IRInstruction::LocalWrite`]) instead carry an [`IRLocalId`]
/// naming a storage slot and produce no value of their own — see
/// [`IRInstruction::dest`].
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
    /// `dest = base.<field_index>`. Backends emit this as GEP + load
    /// over the struct value; `field_type` is the static [`IRType`]
    /// of the projected field, recovered from the matching
    /// [`crate::IRStructDecl`] at lower time so emit doesn't have to
    /// re-walk the decl table per access. `struct_symbol` names the
    /// receiver's [`crate::IRStructDecl`] so seal can validate the
    /// index/type pair against the decl without re-deriving the
    /// receiver's type from `base`.
    FieldGet {
        base: ValueId,
        dest: ValueId,
        field_index: u32,
        field_type: IRType,
        struct_symbol: IRSymbol,
    },
    /// Declare a local-variable storage slot. Emitted exactly once
    /// per [`IRLocalId`] per function, always in the entry block, so
    /// LLVM can hoist a single `alloca` regardless of where the
    /// surface-syntax declaration lives. Backends pick the strategy:
    /// LLVM allocates a stack slot of `ty`; eval inserts a fresh
    /// hashmap entry. Produces no value (the slot is identified by
    /// `local`, not a `ValueId`) — see [`IRInstruction::dest`].
    ///
    /// Seal pins one `LocalDecl` per `local` per function (multiple
    /// would imply two slots claiming the same identity).
    LocalDecl { local: IRLocalId, ty: IRType },
    /// Read the current contents of `local` into a fresh `ValueId`.
    /// `ty` matches the declaring `LocalDecl`'s `ty`; carrying it
    /// here lets the SSA value-flow checker thread the read's type
    /// without consulting the (slot-keyed) decl table on every read.
    /// LLVM lowers to `load`; eval clones the hashmap entry.
    LocalRead {
        dest: ValueId,
        local: IRLocalId,
        ty: IRType,
    },
    /// Write `value` into the slot named by `local`. Used both for
    /// surface-level assignments (`x = expr` after lowering its rhs)
    /// and for parameter promotion at function entry (one `LocalWrite`
    /// per [`IRFunctionParam`] mirroring its `id` into its `local_id`
    /// slot). LLVM lowers to `store`; eval inserts the cloned value
    /// into the hashmap. Produces no value — see [`IRInstruction::dest`].
    LocalWrite { local: IRLocalId, value: ValueId },
    /// `dest = <ty>{<fields>}`. `ty` names the [`crate::IRStructDecl`]
    /// being constructed (mangled symbol); `fields` are pre-canonicalized
    /// to declaration order and carry one [`StructFieldInit`] per
    /// declared field. Backends materialize as alloca + per-field
    /// store + load, mirroring v1 codegen.
    StructInit {
        dest: ValueId,
        fields: Vec<StructFieldInit>,
        ty: IRSymbol,
    },
    /// `dest = <op> operand`.
    UnaryOp {
        dest: ValueId,
        op: IRUnaryOp,
        operand: ValueId,
    },
}

impl IRInstruction {
    /// The `ValueId` this instruction defines, if any.
    ///
    /// Most variants produce one ([`IRInstruction::BinaryOp`],
    /// [`IRInstruction::Call`], [`IRInstruction::Const`],
    /// [`IRInstruction::FieldGet`], [`IRInstruction::LocalRead`],
    /// [`IRInstruction::StructInit`], [`IRInstruction::UnaryOp`]).
    /// [`IRInstruction::LocalDecl`] and [`IRInstruction::LocalWrite`]
    /// touch slots rather than the SSA value-flow, so they return
    /// `None` — the slot is identified by [`IRLocalId`], no fresh
    /// `ValueId` is minted.
    pub fn dest(&self) -> Option<ValueId> {
        match self {
            IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::Const { dest, .. }
            | IRInstruction::FieldGet { dest, .. }
            | IRInstruction::LocalRead { dest, .. }
            | IRInstruction::StructInit { dest, .. }
            | IRInstruction::UnaryOp { dest, .. } => Some(*dest),
            IRInstruction::LocalDecl { .. } | IRInstruction::LocalWrite { .. } => None,
        }
    }
}

/// How a basic block ends. Three variants today: `Return` exits the
/// function; `Branch` jumps unconditionally to another block in the
/// same function; `CondBranch` picks one of two blocks based on a
/// `Bool`-typed value. The seal pass guarantees every targeted
/// `IRBlockId` resolves to a block in the enclosing function.
#[derive(Debug, Clone, PartialEq)]
pub enum IRTerminator {
    /// Unconditional jump to `target`. Emitted at the tail of an
    /// `if` / `unless` arm to reach the merge block.
    Branch(IRBlockId),
    /// `cond` is a `Bool`-typed [`ValueId`]; flow continues at
    /// `then_block` when `cond` is `true`, at `else_block` otherwise.
    CondBranch {
        cond: ValueId,
        then_block: IRBlockId,
        else_block: IRBlockId,
    },
    /// Exit the function with `value` (or `Unit` semantics when
    /// `value` is `None`).
    Return { value: Option<ValueId> },
}
