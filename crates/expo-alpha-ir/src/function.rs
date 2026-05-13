//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use std::borrow::Borrow;
use std::fmt;

use expo_ast::identifier::Identifier;

use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::extern_attrs::IRExternAttrs;
use crate::intrinsic_id::IRIntrinsicId;
use crate::local::IRLocalId;
use crate::ownership::Ownership;
use crate::struct_decl::StructFieldInit;
use crate::types::{
    ConcatKind, ConstValue, IRBinOp, IRType, IRUnaryOp, LoweredBinarySegment, ResolvedBinaryLayout,
    ValueId,
};

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
/// - `Intrinsic(id)` carries empty blocks and a typed
///   [`IRIntrinsicId`]. Both backends `match` exhaustively on that
///   enum to synthesize the body, so adding an intrinsic is a
///   compile-time wiring requirement on every consumer. The id is
///   decoupled from [`IRSymbol::mangled`] so monomorphized symbols
///   can share an emitter without per-mangling table entries.
/// - `Extern(attrs)` carries empty blocks and an FFI-linked
///   declaration only — the backend declares the function under
///   the C symbol named by [`IRExternAttrs::link_name`] (or the
///   function's bare last-segment when `None`) and emits no body;
///   call sites resolve through an `IRSymbol`-keyed function
///   index built at declare time.
/// - `Closure { env_layout }` carries non-empty blocks like
///   `Regular`. The backend prepends an implicit `env_ptr`
///   parameter pointing at a heap struct laid out per `env_layout`;
///   body code reads captures via [`IRInstruction::LoadCapture`]
///   indexed into that layout, and [`IRInstruction::MakeClosure`]
///   is the only writer.
/// - `SpawnWrapper { state }` is the entrypoint thunk a spawned
///   process executes. Single `i8*` config parameter; body calls
///   `state.start(config)` (which returns `Result<state, StopReason>`)
///   and on `Ok` chains into `state.run()`. Minted by the spawn-
///   wrapper monomorphization planner — content-addressed by
///   `state` so every `spawn S.start(...)` site for the same
///   monomorphized state cell shares one wrapper symbol; distinct
///   instantiations get distinct wrappers exactly like generic
///   structs do.
///
/// Per-kind body shape is enforced by the seal pass. The
/// `Extern`, `Intrinsic`, and `SpawnWrapper` variants carry data,
/// which is why this enum is not `Copy` — `Clone` callers compose
/// the per-fn metadata without ambient interior mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    Closure { env_layout: Vec<IRType> },
    Extern(IRExternAttrs),
    Intrinsic(IRIntrinsicId),
    Regular,
    SpawnWrapper { state: IRType },
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
///
/// `params` is the block's typed entry-arg signature: each predecessor
/// branching into this block must pass exactly that many `ValueId`s
/// of matching types in its terminator's [`BranchTarget::args`]. Each
/// [`BlockParam::dest`] is a fresh SSA value, defined-on-entry to the
/// block, available to every instruction in the block. The seal pass
/// asserts the per-edge count and type match. Most blocks declare no
/// params (entry / straight-line bodies); merge blocks of value-
/// producing `if`/`else`/`cond` are the typical sites that do.
#[derive(Debug, Clone)]
pub struct IRBasicBlock {
    pub id: IRBlockId,
    pub label: String,
    pub params: Vec<BlockParam>,
    pub instructions: Vec<IRInstruction>,
    pub terminator: IRTerminator,
}

/// A typed entry-argument of an [`IRBasicBlock`]. Block parameters
/// are the SSA join model alpha-IR uses in place of phi nodes:
/// values flow into a block along its incoming edges via the
/// terminating [`BranchTarget::args`] at each predecessor, and the
/// block's body sees the joined value as a normal `ValueId`. The
/// LLVM backend translates the block-param/branch-args pair to a
/// phi node + `add_incoming` calls at emission time; the
/// interpreter binds args to params on edge traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockParam {
    pub dest: ValueId,
    pub ty: IRType,
}

/// A branch terminator's per-edge payload: the target [`IRBlockId`]
/// plus the operand list passed as the target block's
/// [`BlockParam`] values. `args.len()` must equal the target's
/// `params.len()`; arg types must match the corresponding params.
/// `args` is empty for the common no-param case, so most existing
/// terminator construction sites pass `BranchTarget::to(block)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchTarget {
    pub args: Vec<ValueId>,
    pub block: IRBlockId,
}

impl BranchTarget {
    /// Branch to `block` with no args. Convenience for the common
    /// case where the target declares zero block params.
    pub fn to(block: IRBlockId) -> Self {
        Self {
            args: Vec::new(),
            block,
        }
    }

    /// Branch to `block` carrying `args`. Caller is responsible for
    /// arg/param count and type match (seal will reject mismatches).
    pub fn with_args(block: IRBlockId, args: Vec<ValueId>) -> Self {
        Self { args, block }
    }
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
    /// `dest = <<segments>>` — assemble a `Binary` (when
    /// `layout.byte_aligned`) or `Bits` (otherwise) value from
    /// already-evaluated segment SSA values. `layout` cached at
    /// lower time so backends mint the destination buffer without
    /// re-summing widths; `segments` is in source order, each
    /// carrying its own `bit_offset` from the same lower-time pass.
    ///
    /// Result is freshly-allocated owned heap storage with the
    /// shared bit-length-header layout (`[i64 bit_length][payload]`),
    /// so the lowering layer stamps [`Ownership::Owned`] on the
    /// corresponding `LocalWrite` and the slot auto-drops at fn
    /// exit.
    ///
    /// LLVM emission keys on the per-segment `width % 8` /
    /// `bit_offset % 8` to choose between inline byte-aligned
    /// packing (fast path: integer byte-shift loop, float bit-cast
    /// then byte-shift, string `memcpy`) and the runtime
    /// `__expo_alpha_pack_bits` helper (sub-byte shape).
    BinaryConstruct {
        dest: ValueId,
        layout: ResolvedBinaryLayout,
        segments: Vec<LoweredBinarySegment>,
    },
    /// `dest = callee(args)`. The callee resolves through the
    /// enclosing `IRProgram` / `IRScript` by [`IRSymbol`].
    Call {
        dest: ValueId,
        callee: IRSymbol,
        args: Vec<ValueId>,
    },
    /// `dest = callee(args)` — indirect call through a closure
    /// fat pointer (`callee.ty == IRType::Function`). The backend
    /// prepends `env_ptr` to `args` before dispatch.
    CallClosure {
        args: Vec<ValueId>,
        callee: ValueId,
        dest: ValueId,
        result_ty: IRType,
    },
    /// `dest = lhs <> rhs` for the heap-payload family (`String`,
    /// `Binary`, `Bits`). Separate from [`Self::BinaryOp`] because
    /// the LLVM emission shape differs:
    ///
    /// - `String` / `Binary`: inline `malloc` + two `memcpy`s.
    /// - `Bits`: extern `__expo_alpha_concat_bits` runtime helper
    ///   (sub-byte alignment is far cleaner in Rust than LLVM IR).
    ///
    /// Result is freshly-allocated owned heap storage with the same
    /// `[i64 bit_length][payload]` layout as the operands. The
    /// lowering layer stamps [`Ownership::Owned`] on the
    /// corresponding `LocalWrite`, so the slot is auto-dropped at
    /// fn exit. Both operands flow through unchanged — `<>` does
    /// **not** consume them at the IR level (consumption is a
    /// surface-language concept; at the IR layer the result is a
    /// fresh value and the operands' lifetimes are managed by their
    /// own slots).
    Concat {
        dest: ValueId,
        kind: ConcatKind,
        lhs: ValueId,
        rhs: ValueId,
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
    /// `dest = <value>.tag` (`Int8`). Match-arm CFG compares this
    /// against the constant variant tag.
    EnumTagGet {
        dest: ValueId,
        value: ValueId,
        ty: IRSymbol,
    },
    /// `dest = <value>.<variant>.payload.<payload_index>`. Only
    /// well-defined on the success edge of a preceding tag-eq
    /// gate; seal validates `tag` / `payload_index` / `field_type`
    /// against the decl.
    EnumPayloadFieldGet {
        dest: ValueId,
        value: ValueId,
        tag: IRVariantTag,
        payload_index: u32,
        field_type: IRType,
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
    /// Free the heap storage currently held by `local`'s slot, if
    /// any. Emitted by the lowering layer at function exits (return,
    /// fall-through) for slots whose most-recent [`Self::LocalWrite`]
    /// stamped [`Ownership::Owned`] and whose [`IRType`] is heap-
    /// allocated (today: [`IRType::String`]; future: `Binary`,
    /// `Bits`, `List`, owned closure environments). Reads the slot's
    /// current pointer, computes `payload - 8` to recover the
    /// allocator block base, and calls extern `free`. No-op when the
    /// slot's stamped ownership is [`Ownership::Unowned`] — the
    /// lowering layer doesn't emit `DropLocal` in that case, so a
    /// `DropLocal` reaching a backend always indicates a slot the
    /// backend must free. Produces no value.
    DropLocal { local: IRLocalId, ty: IRType },
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
    /// Write `value` into the slot named by `local` with the given
    /// `ownership`. Used for surface assignments and for parameter
    /// promotion (one `LocalWrite` per param at function entry).
    /// `ownership` records whether the value being stored is heap
    /// storage the slot owns ([`Ownership::Owned`]) or a borrowed
    /// pointer / primitive copy ([`Ownership::Unowned`]); the
    /// lowering layer reads it back at scope-exit drop emission to
    /// decide which slots need a `free`. LLVM lowers to `store`;
    /// produces no value.
    LocalWrite {
        local: IRLocalId,
        ownership: Ownership,
        value: ValueId,
    },
    /// Move the current contents of `local` out of its slot into a
    /// fresh `ValueId`. Used at sites that consume a local's
    /// ownership — today only `return <local>` for an
    /// [`Ownership::Owned`] slot (the returned value transfers to
    /// the caller; the lowering layer suppresses the slot's
    /// fn-exit `DropLocal`). Future sites: cross-local moves
    /// (`t = s` for an Owned `s`), `move`-arg passing. LLVM lowers
    /// identically to [`Self::LocalRead`] (the moved-vs-borrowed
    /// distinction is purely IR-level — the slot's stack memory
    /// remains valid until the function returns); eval removes the
    /// frame entry so subsequent reads panic with use-after-move.
    MoveOutLocal {
        dest: ValueId,
        local: IRLocalId,
        ty: IRType,
    },
    /// `dest = (fn_ptr -> body, env_ptr)` where `env_ptr` points
    /// at a freshly allocated heap struct laid out per `body`'s
    /// [`FunctionKind::Closure::env_layout`]. `captures[i]` fills
    /// field `i`. The closure value is `Ownership::Owned`;
    /// `DropLocal { ty: Function { .. } }` recursively drops
    /// captures before freeing the env.
    MakeClosure {
        body: IRSymbol,
        captures: Vec<ValueId>,
        dest: ValueId,
        ty: IRType,
    },
    /// `dest = env.<capture_index>`. Only valid inside a
    /// [`FunctionKind::Closure`] body; `capture_index` keys into
    /// that kind's `env_layout`. No `StoreCapture` counterpart —
    /// captures are structurally read-only inside the body.
    LoadCapture {
        capture_index: u32,
        dest: ValueId,
        ty: IRType,
    },
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
    /// `dest = spawn wrapper(config)`. Materialize a new process
    /// running `wrapper` with `config` as its `i8*` payload. The
    /// LLVM backend serializes `config`'s bytes into a fresh
    /// allocation and calls `expo_rt_spawn(wrapper_fn_ptr, &bytes,
    /// sizeof)`; eval declines (no scheduler). `dest` is the
    /// returned `Ref<M, R>` (by-value struct wrapping the pid).
    Spawn {
        config: ValueId,
        config_type: IRType,
        dest: ValueId,
        ref_type: IRSymbol,
        wrapper: IRSymbol,
    },
    /// `dest = receive arms after?`. Block on the current process's
    /// mailbox; on message arrival, dispatch to the matching arm
    /// based on the envelope tag (business vs lifecycle); on
    /// `after` timeout, run the after-body. Each arm binds a
    /// payload local from the message buffer. `result_type` is the
    /// joined type of every arm tail.
    Receive {
        after: Option<ReceiveAfter>,
        arms: Vec<ReceiveArm>,
        dest: ValueId,
        result_type: IRType,
    },
}

/// One arm of an [`IRInstruction::Receive`]. `tag` selects which
/// envelope shape the arm matches; `payload_local` is the local
/// slot the payload binds into (declared with `payload_type` in
/// the same function); `body` is the basic block the arm runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveArm {
    pub body: IRBlockId,
    pub payload_local: IRLocalId,
    pub payload_type: IRType,
    pub tag: ReceiveTag,
}

/// Envelope kind a receive arm matches. The runtime tags every
/// message with a single byte at offset 0 and places the payload
/// at offset 8; `Business == 0`, `Lifecycle == 1`. (`IORead == 2`
/// is reserved for the future I/O fast path; alpha doesn't emit
/// it.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveTag {
    Business,
    Lifecycle,
}

impl ReceiveTag {
    /// Wire byte the runtime stamps in the envelope's tag header.
    pub fn wire_byte(self) -> u8 {
        match self {
            Self::Business => 0,
            Self::Lifecycle => 1,
        }
    }
}

/// `after timeout body` clause on an [`IRInstruction::Receive`].
/// `timeout` is an `Int64`-typed SSA value (milliseconds);
/// `body` is the basic block the timeout path runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveAfter {
    pub body: IRBlockId,
    pub timeout: ValueId,
}

impl IRInstruction {
    /// The `ValueId` this instruction defines, if any. Storage-slot
    /// side-effect variants ([`IRInstruction::DropLocal`] /
    /// [`IRInstruction::LocalDecl`] / [`IRInstruction::LocalWrite`])
    /// return `None`; everything else (including
    /// [`IRInstruction::MoveOutLocal`], which moves the slot's
    /// contents into a fresh value) defines a destination.
    pub fn dest(&self) -> Option<ValueId> {
        match self {
            IRInstruction::BinaryConstruct { dest, .. }
            | IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::CallClosure { dest, .. }
            | IRInstruction::Concat { dest, .. }
            | IRInstruction::Const { dest, .. }
            | IRInstruction::EnumConstruct { dest, .. }
            | IRInstruction::EnumPayloadFieldGet { dest, .. }
            | IRInstruction::EnumTagGet { dest, .. }
            | IRInstruction::FieldGet { dest, .. }
            | IRInstruction::LoadCapture { dest, .. }
            | IRInstruction::LoadConst { dest, .. }
            | IRInstruction::LocalRead { dest, .. }
            | IRInstruction::MakeClosure { dest, .. }
            | IRInstruction::MoveOutLocal { dest, .. }
            | IRInstruction::Receive { dest, .. }
            | IRInstruction::Spawn { dest, .. }
            | IRInstruction::StructInit { dest, .. }
            | IRInstruction::UnaryOp { dest, .. } => Some(*dest),
            IRInstruction::DropLocal { .. }
            | IRInstruction::LocalDecl { .. }
            | IRInstruction::LocalWrite { .. } => None,
        }
    }
}

/// How a basic block ends. The seal pass guarantees every targeted
/// `IRBlockId` resolves in the enclosing function and that every
/// [`BranchTarget`]'s `args` list matches the target block's
/// [`BlockParam`] signature in count and type.
#[derive(Debug, Clone, PartialEq)]
pub enum IRTerminator {
    /// Unconditional jump. Most existing call sites use [`Self::branch`]
    /// to construct one with no args.
    Branch(BranchTarget),
    /// Two-way branch on a `Bool`-typed `cond`. Each side carries its
    /// own [`BranchTarget`] so the two edges can pass distinct
    /// per-edge args (used by value-producing `if`/`else` whose merge
    /// block declares a result-typed [`BlockParam`]).
    CondBranch {
        cond: ValueId,
        else_target: BranchTarget,
        then_target: BranchTarget,
    },
    /// Exit the function with `value` (or `Unit` when `None`).
    Return { value: Option<ValueId> },
    /// Statically unreachable. Lowering emits this on the failure
    /// edge of an exhaustive `match` so the CFG stays well-formed
    /// even when typecheck has guaranteed every runtime value is
    /// covered. Eval treats it as a fatal panic; LLVM lowers to the
    /// `unreachable` instruction.
    Unreachable,
}

impl IRTerminator {
    /// Unconditional branch to `block` with no args. Convenience for
    /// the common case (most existing call sites have no per-edge
    /// args because their targets declare no [`BlockParam`]s).
    pub fn branch(block: IRBlockId) -> Self {
        Self::Branch(BranchTarget::to(block))
    }
}
