//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use std::borrow::Borrow;
use std::fmt;

use koja_ast::identifier::Identifier;

use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::extern_attrs::IRExternAttrs;
use crate::intrinsic_id::IRIntrinsicId;
use crate::local::IRLocalId;
use crate::struct_decl::StructFieldInit;
use crate::types::{
    ConcatKind, ConstValue, IRBinOp, IRType, IRUnaryOp, LoweredBinaryMatchLayout,
    LoweredBinaryPattern, LoweredBinarySegment, ResolvedBinaryLayout, ValueId,
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
    pub fn from_identifier(identifier: &Identifier) -> Self {
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

    /// Mint an `IRSymbol` from a fully mangled name that has no
    /// surface-AST identifier root — used for synthesized types
    /// like `IRType::Union` whose mangled symbol is computed from
    /// the canonical member set.
    pub(crate) fn synthetic(mangled: String) -> Self {
        Self(mangled)
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
/// - `ProcessEntryWrapper { state }` is the project-mode entry
///   thunk minted when `koja.toml`'s `entry` names a PascalCase
///   `Process<C, M, R>` type. Same `void(i8*)` shape and `start →
///   run` dispatch as `SpawnWrapper`, but the LLVM emit pass also
///   funnels the resulting `StopReason` through `ExitStatus.code()`
///   and stores it in the module-level `__koja_exit_code` global
///   that the synthesized `main` trampoline returns from. One per
///   program (the entry can't be generic — `koja.toml` names a
///   single concrete state type).
///
/// Per-kind body shape is enforced by the seal pass. The
/// `Extern`, `Intrinsic`, `SpawnWrapper`, and `ProcessEntryWrapper`
/// variants carry data, which is why this enum is not `Copy` —
/// `Clone` callers compose the per-fn metadata without ambient
/// interior mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    /// Synthesized per-type clone glue (`<T>.$clone$`). Registered by
    /// the `elaborate` sub-pass for every heap-managed composite type
    /// (`List` / `Map` / `Set` / heap-owning structs, enums, unions,
    /// `Indirect` boxes). The operand type is `params[0].ty`; the
    /// return type matches it. Lowering's composite `IRInstruction::Clone`
    /// is rewritten into a `call` to this glue, so backends only ever
    /// see leaf `Clone`s inline and a uniform `call` for composites.
    ///
    /// Its body is born one of two ways:
    /// - **Aggregates** (`Struct` / `Enum` / `Union`): `elaborate::synth`
    ///   builds a full CFG (project each field / payload, acquire it,
    ///   rebuild). `blocks` is non-empty and the LLVM backend walks it
    ///   like a [`Self::Regular`] body.
    /// - **Collections / `Indirect`**: a runtime-shaped deep-copy the
    ///   LLVM backend synthesizes from the operand type at emit time,
    ///   so `blocks` lowers empty like [`Self::Intrinsic`].
    ///
    /// Eval reclaims via its host GC and never invokes the glue.
    CloneGlue,
    Closure {
        env_layout: Vec<IRType>,
    },
    /// Synthesized per-closure-body capture-release glue
    /// (`<body>.$drop_env$`). A closure env is type-erased behind the
    /// structural [`IRType::Function`], so a `Drop` at a closure-typed
    /// slot can't statically pick the right capture-release routine.
    /// Each closure that owns at least one heap-managed capture gets
    /// this sibling function; its address is stamped into the env
    /// block's header by [`IRInstruction::MakeClosure`], and the
    /// runtime calls it (with the env pointer) when the env's refcount
    /// hits zero, before freeing the block.
    ///
    /// Shape is closure-like — an implicit `env_ptr` parameter at LLVM
    /// position 0 and a body that reads each heap-managed capture via
    /// [`IRInstruction::LoadCapture`] (indexed into `env_layout`) and
    /// [`IRInstruction::DropValue`]s it, returning `Unit`. Unlike
    /// [`Self::Closure`] it carries no user-visible params and is never
    /// the target of a `MakeClosure`. Born as real IR during lowering
    /// so [`crate::elaborate`] discovers any composite capture's
    /// `drop_T` and rewrites the composite `DropValue`s into glue
    /// calls, exactly as for a `Regular` body. Eval reclaims via its
    /// host GC and never invokes it.
    DropClosureGlue {
        env_layout: Vec<IRType>,
    },
    /// Synthesized per-type drop glue (`<T>.$drop$`). The drop analog
    /// of [`Self::CloneGlue`] — releases every heap-managed field /
    /// payload / element of `params[0].ty`, then frees any collection
    /// backing buffer. Returns `Unit`. Lowering's composite
    /// `DropLocal` / `DropValue` is rewritten into a `call` to this
    /// glue. Same two body shapes as [`Self::CloneGlue`]: an
    /// `elaborate`-synthesized CFG for aggregates, an emit-time
    /// backend body (empty `blocks`) for collections / `Indirect`.
    /// Eval reclaims via its host GC and never invokes it.
    DropGlue,
    Extern(IRExternAttrs),
    Intrinsic(IRIntrinsicId),
    ProcessEntryWrapper {
        state: IRType,
    },
    Regular,
    SpawnWrapper {
        state: IRType,
    },
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
/// are the SSA join model IR uses in place of phi nodes:
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
    /// Result is freshly-allocated heap storage with the shared
    /// bit-length-header layout (`[i64 bit_length][payload]`).
    ///
    /// LLVM emission keys on the per-segment `width % 8` /
    /// `bit_offset % 8` to choose between inline byte-aligned
    /// packing (fast path: integer byte-shift loop, float bit-cast
    /// then byte-shift, string `memcpy`) and the runtime
    /// `__koja_pack_bits` helper (sub-byte shape).
    BinaryConstruct {
        dest: ValueId,
        layout: ResolvedBinaryLayout,
        segments: Vec<LoweredBinarySegment>,
    },
    /// `dest = match <subject> against <segments>` — assemble a
    /// `Bool` (`i1`) success value from a runtime test against
    /// `subject`'s `Binary`/`Bits` payload at the bit offsets
    /// recorded in `segments`. As a side effect, every
    /// [`LoweredBinaryPattern::BindInt`] / [`LoweredBinaryPattern::GreedyTail`]
    /// segment extracts its slice of the subject into the
    /// pre-declared local slot named on the segment — the
    /// lowering layer emits the matching [`IRInstruction::LocalDecl`]
    /// in the function's entry block, so seal's
    /// "every-write-is-dominated-by-a-decl" rule still holds.
    ///
    /// LLVM emission is:
    ///
    /// 1. Compare the subject's runtime bit length against
    ///    `layout.fixed_bits` (equality when
    ///    `!layout.has_greedy_tail`, unsigned-greater-or-equal
    ///    when there is a greedy tail).
    /// 2. For each segment, extract its slice at `bit_offset` and
    ///    AND its per-segment success bit into the running result.
    /// 3. For [`LoweredBinaryPattern::BindInt`] segments, store
    ///    the extracted (and sign-extended when `sign == Signed`)
    ///    integer into the slot. For
    ///    [`LoweredBinaryPattern::GreedyTail`] segments, allocate
    ///    a fresh `[i64 bit_length][payload]` value, copy the
    ///    remaining bytes, and store the payload pointer.
    BinaryMatch {
        dest: ValueId,
        layout: LoweredBinaryMatchLayout,
        segments: Vec<LoweredBinaryPattern>,
        subject: ValueId,
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
    /// `dest = clone(source)` — a value-semantics acquisition that
    /// yields an independent owner of `source` (statically typed `ty`).
    /// The drop-glue lowering emits this at every *ownership
    /// acquisition* of a heap value (binding, parameter promotion,
    /// field/element store, return) so each owner can drop at scope
    /// exit without aliasing another owner. Under reference counting
    /// the independence is logical, not physical: immutable blocks are
    /// shared and the bookkeeping is an `rc++`. The source stays live.
    ///
    /// Backend lowering by `ty`:
    /// - Leaf heap (`String` / `Binary` / `Bits`): an `rc_inc` on the
    ///   block base (`payload - HEADER_BYTES`); `dest` re-binds the
    ///   same payload pointer. Eval relies on its host GC and treats
    ///   the looked-up value as already independent.
    /// - Stack/`Copy` leaf types (`Int`, `Float`, `Bool`, …): a plain
    ///   register copy — `dest` aliases the same immutable SSA value.
    /// - Closure (`Function`): an `rc_inc` on the captured env block,
    ///   aliasing the same `{fn_ptr, env_ptr}` fat pointer.
    /// - No-glue aggregates (`Struct` / `Enum` / `Union` whose fields
    ///   are all `Copy`): a register copy, like the scalar leaves.
    /// - Heap composites (`List` / `Map` / `Set` / `Indirect` and
    ///   heap-owning structs and enums): rewritten by the `elaborate`
    ///   sub-pass into a `Call` to a synthesized per-type `clone_T`, so
    ///   the backend never recurses inline. One surviving to a backend
    ///   is a lowering bug.
    Clone {
        dest: ValueId,
        source: ValueId,
        ty: IRType,
    },
    /// `dest = lhs <> rhs` for the heap-payload family (`String`,
    /// `Binary`, `Bits`). Separate from [`Self::BinaryOp`] because
    /// the LLVM emission shape differs:
    ///
    /// - `String` / `Binary`: inline `malloc` + two `memcpy`s.
    /// - `Bits`: extern `__koja_concat_bits` runtime helper
    ///   (sub-byte alignment is far cleaner in Rust than LLVM IR).
    ///
    /// Result is freshly-allocated heap storage with the same
    /// `[i64 bit_length][payload]` layout as the operands. Both
    /// operands flow through unchanged — `<>` does
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
    /// `dest = base with field_index <- value`. SSA-pure: produces a
    /// new struct value identical to `base` except the field at
    /// `field_index` is replaced by `value`. Backends materialize
    /// the rebuild in their own way — eval clones the field vec and
    /// swaps one slot; LLVM `alloca`s the receiver, GEP-stores the
    /// new field, and reloads. Heap-typed leaf overwrites are the
    /// IR-lowerer's responsibility: it must emit a synthetic
    /// `DropLocal`-style free of the previous payload before the
    /// `FieldSet` (mirrors the local-reassignment overwrite drop in
    /// [`crate::lower::body`]) so the new write doesn't leak.
    FieldSet {
        base: ValueId,
        dest: ValueId,
        field_index: u32,
        field_type: IRType,
        struct_symbol: IRSymbol,
        value: ValueId,
    },
    /// Free the heap storage currently held by `local`'s slot. Emitted
    /// by the lowering layer at function exits (return, fall-through)
    /// for slots whose [`IRType`] is heap-allocated. Reads the slot's
    /// current pointer, computes `payload - 8` to recover the allocator
    /// block base, and calls extern `free`. A `DropLocal` reaching a
    /// backend always indicates a slot the backend must free. Produces
    /// no value.
    DropLocal { local: IRLocalId, ty: IRType },
    /// Free the heap storage held by `value`. Value-keyed analog of
    /// [`Self::DropLocal`], used by [`Self::FieldSet`] lowering when
    /// the leaf field is heap-typed: the field-write reads the old
    /// payload via [`Self::FieldGet`] into an SSA value, drops it
    /// with this instruction, then `FieldSet`s the new payload in.
    /// Same `payload - 8` GEP + extern `free` shape as `DropLocal`,
    /// just sourced from a register instead of a slot. Eval is a
    /// no-op (the host GC reclaims). Produces no value.
    DropValue { value: ValueId, ty: IRType },
    /// Declare a local-variable storage slot. Emitted exactly once
    /// per [`IRLocalId`] per function in the entry block (LLVM hoists
    /// the `alloca`; eval inserts a fresh hashmap entry). The LLVM
    /// backend zero-initializes the slot at the decl site, so a
    /// `DropLocal` on a path that never wrote the slot (an untaken
    /// `receive` arm's payload local, say) releases nothing — the
    /// runtime rc primitives treat null as a no-op. Produces no
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
    /// `dest = (fn_ptr -> body, env_ptr)` where `env_ptr` points
    /// at a freshly allocated heap struct laid out per `body`'s
    /// [`FunctionKind::Closure::env_layout`]. `captures[i]` fills
    /// field `i`.
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
    /// allocation and calls `koja_rt_spawn(wrapper_fn_ptr, &bytes,
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
    /// `dest = <ty>.wrap(value)` — box `value` (typed `member_type`,
    /// statically a member of `ty`) into a tagged union value of
    /// type `ty`. `member_index` is the 0-based offset of
    /// `member_type` in the union's canonical (sorted) member list,
    /// used as the runtime tag byte. Lowered from the typecheck-
    /// stamped [`koja_ast::coercion::Coercion::UnionWiden`] at every
    /// member→union flow site (assignments, struct fields, args,
    /// returns).
    UnionWrap {
        dest: ValueId,
        member_index: u8,
        member_type: IRType,
        ty: IRType,
        value: ValueId,
    },
    /// `dest = <value>.tag` (`Int8`). Match-arm CFG compares this
    /// against the constant member-index for each union arm —
    /// counterpart of [`Self::EnumTagGet`] for the union family.
    UnionTagGet {
        dest: ValueId,
        ty: IRType,
        value: ValueId,
    },
    /// `dest = <value>.payload as <member_type>`. Only well-defined
    /// on the success edge of a preceding tag-eq gate; seal
    /// validates `member_index`/`member_type` against the union
    /// decl. Counterpart of [`Self::EnumPayloadFieldGet`] for the
    /// union family.
    UnionPayloadGet {
        dest: ValueId,
        member_index: u8,
        member_type: IRType,
        ty: IRType,
        value: ValueId,
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
/// message with a single byte at offset 0 and places the payload at
/// offset 8; `Business == 0`, `Lifecycle == 1`. (`IORead == 2` is
/// reserved for the future I/O fast path; the pipeline does not yet
/// emit it.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveTag {
    Business,
    Lifecycle,
}

impl ReceiveTag {
    /// Wire byte the runtime stamps in the envelope's tag header.
    ///
    /// These values conform to the envelope wire ABI, whose
    /// authoritative definition is `koja-runtime/src/wire.rs`
    /// (`TAG_BUSINESS` / `TAG_LIFECYCLE`). They mirror it by spec, not
    /// via a shared type.
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
    /// return `None`; everything else defines a destination.
    pub fn dest(&self) -> Option<ValueId> {
        match self {
            IRInstruction::BinaryConstruct { dest, .. }
            | IRInstruction::BinaryMatch { dest, .. }
            | IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::CallClosure { dest, .. }
            | IRInstruction::Clone { dest, .. }
            | IRInstruction::Concat { dest, .. }
            | IRInstruction::Const { dest, .. }
            | IRInstruction::EnumConstruct { dest, .. }
            | IRInstruction::EnumPayloadFieldGet { dest, .. }
            | IRInstruction::EnumTagGet { dest, .. }
            | IRInstruction::FieldGet { dest, .. }
            | IRInstruction::FieldSet { dest, .. }
            | IRInstruction::LoadCapture { dest, .. }
            | IRInstruction::LoadConst { dest, .. }
            | IRInstruction::LocalRead { dest, .. }
            | IRInstruction::MakeClosure { dest, .. }
            | IRInstruction::Receive { dest, .. }
            | IRInstruction::Spawn { dest, .. }
            | IRInstruction::StructInit { dest, .. }
            | IRInstruction::UnaryOp { dest, .. }
            | IRInstruction::UnionPayloadGet { dest, .. }
            | IRInstruction::UnionTagGet { dest, .. }
            | IRInstruction::UnionWrap { dest, .. } => Some(*dest),
            IRInstruction::DropLocal { .. }
            | IRInstruction::DropValue { .. }
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
    /// Reinvoke `callee` with `args`, reusing the current frame's
    /// stack. Stamped by the post-merge [`crate::lower::tail_calls`]
    /// pass on call-then-return shapes where `callee` matches the
    /// enclosing function's symbol — i.e. self-recursive tail calls.
    /// Backends turn this into in-frame state rebinding plus a jump:
    /// LLVM stores each `arg` into the matching parameter slot and
    /// branches to the function's loop header; the interpreter
    /// signals its trampoline to restart the body with `args` as the
    /// new bindings. Cross-function tail calls aren't admitted yet —
    /// extending here is a one-line drop of the self-callee check
    /// in the rewrite pass plus a backend musttail emit.
    TailCall {
        args: Vec<ValueId>,
        callee: IRSymbol,
    },
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
