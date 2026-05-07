//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use std::borrow::Borrow;
use std::fmt;

use expo_ast::identifier::Identifier;

use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::extern_attrs::IRExternAttrs;
use crate::local::IRLocalId;
use crate::struct_decl::StructFieldInit;
use crate::types::{ConstValue, IRBinOp, IRType, IRUnaryOp, ValueId};

/// The IR's stable, backend-facing handle for a callable. Stamped
/// once at lower time from the AST [`Identifier`]; downstream
/// consumers read only via [`Self::mangled`]. Used as the key on
/// [`crate::IRPackage::functions`] and the callee field on
/// [`IRInstruction::Call`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRSymbol(String);

impl IRSymbol {
    /// Mint an `IRSymbol` from a declaration's canonical AST
    /// identifier. The only path that introduces a new symbol root —
    /// every other `IRSymbol` is [`Self::derived`] off one of these.
    pub(crate) fn from_identifier(identifier: &Identifier) -> Self {
        Self(identifier.qualified_name())
    }

    /// Build a new symbol that extends `self`'s mangled name with
    /// `suffix`. Reserved for [`crate::mangling`]; the resulting
    /// symbol is rooted at the same AST identifier as `self`,
    /// disambiguated by a monomorphization suffix
    /// (e.g. `_$Int.TestApp.String$`).
    pub(crate) fn derived(&self, suffix: &str) -> Self {
        let mut name = String::with_capacity(self.0.len() + suffix.len());
        name.push_str(&self.0);
        name.push_str(suffix);
        Self(name)
    }

    /// The mangled symbol name. Backends pass this directly to LLVM
    /// or to any other linker-aware lookup.
    pub fn mangled(&self) -> &str {
        &self.0
    }

    /// The bare last segment of the underlying AST identifier path
    /// (e.g. `TestApp.cosf` → `cosf`). Falls back to the full
    /// mangled name when no `.` is present (root identifiers,
    /// derived monomorphization suffixes that don't contain a
    /// path separator). Used by the LLVM backend when it needs a
    /// human-readable C-symbol-style name for an `@extern "C"`
    /// declaration whose `@link "lib"` payload didn't supply one.
    pub fn last_segment(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(self.0.as_str())
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
/// pre-allocated [`ValueId`] body lowering binds the param under;
/// `local_id` is the slot the param is promoted into at function
/// entry (a matching [`IRInstruction::LocalDecl`] +
/// [`IRInstruction::LocalWrite`] are emitted in the entry block so
/// body references read through the same `LocalRead` path body
/// locals use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IRFunctionParam {
    pub id: ValueId,
    pub local_id: IRLocalId,
    pub ty: IRType,
}

/// Function-unique handle for an [`IRBasicBlock`]. Same value has no
/// meaning across functions. Display renders as `bb<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IRBlockId(pub u32);

impl fmt::Display for IRBlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

/// How a function's body is materialized at emission time.
///
/// - `Regular` carries non-empty blocks the backend walks.
/// - `Intrinsic` carries empty blocks and the backend synthesizes
///   a body from a per-backend dispatch table keyed by
///   [`IRSymbol::mangled`].
/// - `Extern(attrs)` carries empty blocks and an FFI-linked
///   declaration only — the backend declares the function under
///   the C symbol named by [`IRExternAttrs::link_name`] (or the
///   function's bare last-segment when `None`) and emits no body;
///   call sites resolve through an `IRSymbol`-keyed function
///   index built at declare time.
///
/// Per-kind body shape is enforced by the seal pass. The
/// `Extern` variant carries data, which is why this enum is no
/// longer `Copy` — `Clone` callers compose the per-fn metadata
/// without ambient interior mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    Intrinsic,
    Regular,
    Extern(IRExternAttrs),
}

/// A lowered function. `blocks[0]` is the entry block; `params`
/// occupy the first `ValueId`s allocated for the function. `kind`
/// distinguishes regular fns from `@intrinsic`-annotated ones (see
/// [`FunctionKind`]).
#[derive(Debug, Clone)]
pub struct IRFunction {
    pub blocks: Vec<IRBasicBlock>,
    pub kind: FunctionKind,
    pub params: Vec<IRFunctionParam>,
    pub return_type: IRType,
    pub symbol: IRSymbol,
}

/// A straight-line sequence of [`IRInstruction`]s ending in exactly
/// one [`IRTerminator`]. `label` is a short human hint (`"entry"`,
/// `"if_then"`) borrowed by the IR text format and LLVM block names.
#[derive(Debug, Clone)]
pub struct IRBasicBlock {
    pub id: IRBlockId,
    pub label: String,
    pub instructions: Vec<IRInstruction>,
    pub terminator: IRTerminator,
}

/// A single SSA-style instruction. Most variants define a fresh
/// `dest: ValueId`; the local-slot variants ([`IRInstruction::LocalDecl`] /
/// [`IRInstruction::LocalWrite`]) name a storage slot via
/// [`IRLocalId`] and produce no value (see [`IRInstruction::dest`]).
#[derive(Debug, Clone, PartialEq)]
pub enum IRInstruction {
    /// `dest = lhs <op> rhs`.
    BinaryOp {
        dest: ValueId,
        lhs: ValueId,
        op: IRBinOp,
        rhs: ValueId,
    },
    /// `dest = callee(args)`. The callee resolves through the
    /// enclosing `IRProgram` / `IRScript` by [`IRSymbol`].
    Call {
        dest: ValueId,
        callee: IRSymbol,
        args: Vec<ValueId>,
    },
    /// `dest = <constant>`.
    Const { dest: ValueId, value: ConstValue },
    /// `dest = <ty>.<variant>(<payload>)`. `tag` is the variant's
    /// 0-based position in [`crate::IREnumDecl::variants`] (also
    /// the wire byte of the LLVM tag field); `payload` carries the
    /// already-lowered init values for the variant's payload fields
    /// (Unit/Tuple/Struct shapes; struct-variant inits are
    /// canonicalized to declaration order, mirroring
    /// [`Self::StructInit`]).
    ///
    /// Seal asserts:
    /// - `ty` resolves to a registered enum.
    /// - `tag.0 < variants.len()`.
    /// - `payload`'s shape matches the variant's
    ///   [`crate::IRVariantPayload`] (Unit ↔ Unit, Tuple arity match,
    ///   Struct len + canonicalization match).
    EnumConstruct {
        dest: ValueId,
        payload: EnumPayloadInit,
        tag: IRVariantTag,
        ty: IRSymbol,
    },
    /// `dest = base.<field_index>`. Backends emit GEP + load.
    /// `field_type` is the projected field's [`IRType`] (cached from
    /// the [`crate::IRStructDecl`] at lower time); `struct_symbol`
    /// names the receiver's struct so seal can validate the
    /// index/type pair without re-deriving from `base`.
    FieldGet {
        base: ValueId,
        dest: ValueId,
        field_index: u32,
        field_type: IRType,
        struct_symbol: IRSymbol,
    },
    /// Declare a local-variable storage slot. Emitted exactly once
    /// per [`IRLocalId`] per function in the entry block (LLVM hoists
    /// the `alloca`; eval inserts a fresh hashmap entry). Produces no
    /// value.
    LocalDecl { local: IRLocalId, ty: IRType },
    /// Read the current contents of `local` into a fresh `ValueId`.
    /// `ty` matches the declaring `LocalDecl`'s `ty`. LLVM lowers to
    /// `load`; eval clones the hashmap entry.
    LocalRead {
        dest: ValueId,
        local: IRLocalId,
        ty: IRType,
    },
    /// Write `value` into the slot named by `local`. Used for surface
    /// assignments and for parameter promotion (one `LocalWrite` per
    /// param at function entry). LLVM lowers to `store`; produces no
    /// value.
    LocalWrite { local: IRLocalId, value: ValueId },
    /// `dest = <pool[const_id]>` — load a pooled compound constant.
    /// `const_id` keys an entry on [`crate::IRPackage::constants`];
    /// `ty` cached at lower time so backends mint the dest slot
    /// without a pool lookup. Seal asserts every emitted `LoadConst`
    /// resolves through some package's pool.
    LoadConst {
        const_id: IRSymbol,
        dest: ValueId,
        ty: IRType,
    },
    /// `dest = <ty>{<fields>}`. `fields` are canonicalized to
    /// declaration order with one [`StructFieldInit`] per declared
    /// field. Backends materialize as alloca + per-field store + load.
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
    /// The `ValueId` this instruction defines, if any. Local-slot
    /// variants ([`IRInstruction::LocalDecl`] /
    /// [`IRInstruction::LocalWrite`]) return `None`.
    pub fn dest(&self) -> Option<ValueId> {
        match self {
            IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::Const { dest, .. }
            | IRInstruction::EnumConstruct { dest, .. }
            | IRInstruction::FieldGet { dest, .. }
            | IRInstruction::LoadConst { dest, .. }
            | IRInstruction::LocalRead { dest, .. }
            | IRInstruction::StructInit { dest, .. }
            | IRInstruction::UnaryOp { dest, .. } => Some(*dest),
            IRInstruction::LocalDecl { .. } | IRInstruction::LocalWrite { .. } => None,
        }
    }
}

/// How a basic block ends. The seal pass guarantees every targeted
/// `IRBlockId` resolves in the enclosing function.
#[derive(Debug, Clone, PartialEq)]
pub enum IRTerminator {
    Branch(IRBlockId),
    CondBranch {
        cond: ValueId,
        then_block: IRBlockId,
        else_block: IRBlockId,
    },
    /// Exit the function with `value` (or `Unit` when `None`).
    Return {
        value: Option<ValueId>,
    },
}
