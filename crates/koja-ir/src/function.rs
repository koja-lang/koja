//! IR shape **inside** a single function: basic blocks, instructions,
//! and terminators. Top-level structure (packages, programs) lives in
//! [`crate::package`] and [`crate::program`].

use std::borrow::Borrow;
use std::fmt;
use std::path::PathBuf;

use koja_ast::identifier::{Identifier, LocalId};

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
/// once at lower time from the AST [`Identifier`], and downstream
/// consumers read only via [`Self::mangled`]. Used as the key on
/// [`crate::IRPackage::functions`] and the callee field on
/// [`IRInstruction::Call`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRSymbol(String);

impl IRSymbol {
    /// Mint an `IRSymbol` from a declaration's canonical AST
    /// identifier. This is the only path that introduces a new symbol
    /// root. Every other `IRSymbol` is [`Self::derived`] off one of these.
    pub fn from_identifier(identifier: &Identifier) -> Self {
        Self(identifier.qualified_name())
    }

    /// Build a new symbol that extends `self`'s mangled name with
    /// `suffix`. Reserved for [`crate::mangling`]. The resulting
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
    /// surface-AST identifier root. Used for synthesized types
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
    /// (e.g. `TestApp.cosf` -> `cosf`). Falls back to the full
    /// mangled name when no `.` is present (root identifiers,
    /// derived monomorphization suffixes that don't contain a
    /// path separator). Used by the LLVM backend when it needs a
    /// human-readable C-symbol-style name for an `@extern "C"`
    /// declaration whose `@link "lib"` payload didn't supply one.
    pub fn last_segment(&self) -> &str {
        let segment = self.0.rsplit('.').next().unwrap_or(self.0.as_str());
        strip_arity_suffix(segment)
    }
}

/// Drop a trailing `/N` arity suffix (`greet/1` -> `greet`). A `/`
/// followed by anything other than digits is kept.
fn strip_arity_suffix(segment: &str) -> &str {
    let Some(slash) = segment.rfind('/') else {
        return segment;
    };
    let after = &segment[slash + 1..];
    let arity_len = after.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if arity_len == 0 {
        return segment;
    }
    &segment[..slash]
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
/// pre-allocated [`ValueId`] body lowering binds the param under, and
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

/// How a function's body is materialized. The seal pass enforces the
/// per-kind body shape. Kinds with empty `blocks` get their body
/// from the backend.
///
/// The `$clone$` / `$drop$` / `$deep_copy$` glue kinds are
/// synthesized by [`crate::elaborate`], which also explains when a
/// composite `Clone` / `Drop` / `DeepCopy` becomes a glue `Call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    /// Per-type clone glue (`<T>.$clone$`) for a heap-managed
    /// composite. Takes and returns `params[0].ty`. Aggregates carry
    /// a synthesized CFG. Collections carry empty `blocks`.
    CloneGlue,
    /// A closure body. The backend prepends an implicit `env_ptr`
    /// parameter to a heap env laid out per `env_layout`. The body
    /// reads captures via [`IRInstruction::LoadCapture`], and
    /// [`IRInstruction::MakeClosure`] is the only writer.
    Closure { env_layout: Vec<IRType> },
    /// Per-closure-body env deep-copy glue (`<body>.$copy_env$`).
    /// Stamped into the env header's `copy_fn` word by
    /// [`IRInstruction::MakeClosure`] and called by the runtime when
    /// the closure crosses a process boundary. Returns a raw env
    /// pointer, which has no IR type, so `blocks` is always empty.
    /// Registered for every closure that captures.
    CopyClosureGlue { env_layout: Vec<IRType> },
    /// Per-type deep-copy glue (`<T>.$deep_copy$`) for a heap-managed
    /// composite reachable from an [`IRInstruction::DeepCopy`] site or
    /// a [`Self::CopyClosureGlue`] env layout. Takes and returns
    /// `params[0].ty` and shares no storage with its argument. Same
    /// body shapes as [`Self::CloneGlue`].
    DeepCopyGlue,
    /// Per-closure-body capture-release glue (`<body>.$drop_env$`).
    /// Stamped into the env header by [`IRInstruction::MakeClosure`]
    /// and called by the runtime when the env's refcount reaches zero.
    /// Closure-shaped (implicit `env_ptr`, no user params, returns
    /// `Unit`). Real IR, so `elaborate` rewrites its composite
    /// `DropValue`s like any body. Registered for every closure with
    /// a heap-managed capture.
    DropClosureGlue { env_layout: Vec<IRType> },
    /// Per-closure-body capture-equality glue (`<body>.$eq_env$`).
    /// Stamped into the env header's `eq_fn` word by
    /// [`IRInstruction::MakeClosure`] and called by
    /// [`IRInstruction::ClosureEquals`] when both sides share a
    /// `site_id`. Closure-shaped with one user param, the other
    /// closure, and a `Bool` return. Real IR. Captureless bodies
    /// register none.
    EqClosureGlue { env_layout: Vec<IRType> },
    /// Per-type drop glue (`<T>.$drop$`) for a heap-managed composite.
    /// Releases every heap-managed constituent of `params[0].ty` and
    /// returns `Unit`. Same body shapes as [`Self::CloneGlue`], with
    /// `Indirect` on the empty-`blocks` side.
    DropGlue,
    /// An FFI declaration with empty `blocks`. The backend declares
    /// the C symbol named by [`IRExternAttrs::link_name`] (or the
    /// function's last path segment when `None`) and emits no body.
    Extern(IRExternAttrs),
    /// A builtin with empty `blocks`. Backends match exhaustively on
    /// the [`IRIntrinsicId`] to synthesize the body. The id is
    /// independent of [`IRSymbol::mangled`] so monomorphized symbols
    /// can share one emitter.
    Intrinsic(IRIntrinsicId),
    /// Project-mode entry thunk, minted when `koja.toml`'s `entry`
    /// names a `Process<C, M, R>` type. Same shape as
    /// [`Self::SpawnWrapper`], and the backend also stores the
    /// resulting exit code where the host `main` can return it. One
    /// per program.
    ProcessEntryWrapper { state: IRType },
    /// User-written body with non-empty `blocks`.
    Regular,
    /// The thunk a spawned process runs. Takes one raw config
    /// pointer, calls `state.start(config)`, and on `Ok` chains into
    /// `state.run()`. Content-addressed by `state`, so every spawn of
    /// the same monomorphized state shares one wrapper.
    SpawnWrapper { state: IRType },
}

/// Source-definition location of a callable, captured at lower time
/// for function-granular DWARF emission. `file` is the source path as
/// the compiler saw it (may be relative), and `line` is the 1-based
/// line the declaration opened on. Only the LLVM backend consumes it
/// (to stamp a `DISubprogram`). The interpreter ignores it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IRSourceDef {
    pub file: PathBuf,
    pub line: u32,
}

/// A lowered function. `blocks[0]` is the entry block, and `params`
/// occupy the first `ValueId`s allocated for the function. `kind`
/// distinguishes regular fns from `@intrinsic`-annotated ones (see
/// [`FunctionKind`]).
///
/// `def_location` carries the surface-source origin for DWARF. It is
/// `Some` for user-declared functions (top-level, methods, and their
/// monomorphizations) and `None` for synthesized callables (glue
/// (`*Glue`), closure bodies, and spawn/entry wrappers), which have
/// no single source line to attribute.
#[derive(Debug, Clone)]
pub struct IRFunction {
    pub blocks: Vec<IRBasicBlock>,
    pub def_location: Option<IRSourceDef>,
    pub kind: FunctionKind,
    pub params: Vec<IRFunctionParam>,
    pub return_type: IRType,
    pub symbol: IRSymbol,
}

impl IRFunction {
    pub(crate) fn next_block_id(&self) -> IRBlockId {
        let max = self.blocks.iter().map(|block| block.id.0).max();
        IRBlockId(max.map_or(0, |id| id + 1))
    }

    /// One past the highest local id in use. Receive payload locals
    /// are scanned directly because the arm can be planned before
    /// its `LocalDecl` is appended.
    pub(crate) fn next_local_id(&self) -> IRLocalId {
        let mut max = 0;
        for param in &self.params {
            max = max.max(param.local_id.as_u32());
        }
        for block in &self.blocks {
            for instruction in &block.instructions {
                match instruction {
                    IRInstruction::ConsumeLocal { local }
                    | IRInstruction::DropLocal { local, .. }
                    | IRInstruction::LocalDecl { local, .. }
                    | IRInstruction::LocalRead { local, .. }
                    | IRInstruction::LocalWrite { local, .. } => max = max.max(local.as_u32()),
                    IRInstruction::Receive { arms, .. } => {
                        for arm in arms {
                            max = max.max(arm.payload_local.as_u32());
                        }
                    }
                    _ => {}
                }
            }
        }
        IRLocalId::from_local_id(LocalId::new(max + 1))
    }

    pub(crate) fn next_value_id(&self) -> u32 {
        let mut max = self
            .params
            .iter()
            .map(|param| param.id.0)
            .max()
            .unwrap_or(0);
        for block in &self.blocks {
            for param in &block.params {
                max = max.max(param.dest.0);
            }
            for instruction in &block.instructions {
                if let Some(dest) = instruction.dest() {
                    max = max.max(dest.0);
                }
            }
        }
        max + 1
    }
}

/// A straight-line sequence of [`IRInstruction`]s ending in exactly
/// one [`IRTerminator`]. `label` is a short human hint (`"entry"`,
/// `"if_then"`) borrowed by the IR text format and LLVM block names.
///
/// `params` is the block's typed entry-arg signature. Each predecessor
/// branching into this block must pass exactly that many `ValueId`s
/// of matching types in its terminator's [`BranchTarget::args`]. Each
/// [`BlockParam::dest`] is a fresh SSA value, defined-on-entry to the
/// block, available to every instruction in the block. The seal pass
/// asserts the per-edge count and type match. Most blocks declare no
/// params (entry / straight-line bodies), while merge blocks of value-
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
/// phi node + `add_incoming` calls at emission time, and the
/// interpreter binds args to params on edge traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockParam {
    pub dest: ValueId,
    pub ty: IRType,
}

/// A branch terminator's per-edge payload: the target [`IRBlockId`]
/// plus the operand list passed as the target block's
/// [`BlockParam`] values. `args.len()` must equal the target's
/// `params.len()`, and arg types must match the corresponding params.
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

/// Declaration slot that owns a cycle-breaking `Indirect` box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IRIndirectSlot {
    /// A payload slot in one enum variant.
    EnumPayload {
        payload_index: u32,
        tag: IRVariantTag,
        ty: IRSymbol,
    },
    /// A field slot in a struct.
    StructField {
        field_index: u32,
        struct_symbol: IRSymbol,
    },
}

/// A single SSA-style instruction. Most variants define a fresh
/// `dest: ValueId`, while the local-slot variants ([`IRInstruction::LocalDecl`] /
/// [`IRInstruction::LocalWrite`]) name a storage slot via
/// [`IRLocalId`] and produce no value (see [`IRInstruction::dest`]).
#[derive(Debug, Clone, PartialEq)]
pub enum IRInstruction {
    /// `dest = lhs <op> rhs`. `operand_ty` is the shared type of
    /// both operands (typecheck guarantees they agree), carried so
    /// backends pick width- and signedness-correct instructions and
    /// arithmetic fault guards.
    BinaryOp {
        dest: ValueId,
        lhs: ValueId,
        op: IRBinOp,
        operand_ty: IRType,
        rhs: ValueId,
    },
    /// `dest = <<segments>>`: assemble a `Binary` (when
    /// `layout.byte_aligned`) or `Bits` (otherwise) value from
    /// already-evaluated segment SSA values. `layout` cached at
    /// lower time so backends mint the destination buffer without
    /// re-summing widths. `segments` is in source order, each
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
    /// `dest: Bool = match subject against segments`. The subject's
    /// bit length must equal `layout.fixed_bits`, or be at least that
    /// when `layout.has_greedy_tail`, and every literal segment must
    /// match at its `bit_offset`. On success each
    /// [`LoweredBinaryPattern::BindInt`] and
    /// [`LoweredBinaryPattern::GreedyTail`] segment writes its slice
    /// of the subject into the local slot named on the segment.
    /// Lowering declares those slots in the entry block.
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
    /// `dest = callee(args)`: indirect call through a closure
    /// fat pointer (`callee.ty == IRType::Function`). The backend
    /// prepends `env_ptr` to `args` before dispatch and builds the
    /// call signature from `param_types`.
    CallClosure {
        args: Vec<ValueId>,
        callee: ValueId,
        dest: ValueId,
        param_types: Vec<IRType>,
        result_ty: IRType,
    },
    /// `dest = clone(source)`. Acquires a new owner of `source`
    /// (statically typed `ty`) so each owner can drop at scope exit
    /// without releasing another owner's storage. Lowering emits one
    /// at every ownership acquisition (binding, parameter promotion,
    /// field or element store, return). The source stays live.
    /// Backends handle leaf types inline. Heap composites are
    /// rewritten by [`crate::elaborate`] into a glue `Call` before
    /// they reach a backend.
    Clone {
        dest: ValueId,
        source: ValueId,
        ty: IRType,
    },
    /// `dest: Bool = lhs == rhs` for two closure values of the same
    /// [`IRType::Function`] `ty`. Two closures are equal when they
    /// were built from the same function or closure expression and
    /// their captures are equal. Backends compare the `site_id`
    /// header words first, then call the env's `eq_fn`
    /// ([`FunctionKind::EqClosureGlue`]) with `lhs`'s env and `rhs`.
    /// A null `eq_fn` (captureless body) means equal sites suffice.
    ClosureEquals {
        dest: ValueId,
        lhs: ValueId,
        rhs: ValueId,
        ty: IRType,
    },
    /// `dest = lhs <> rhs` for the heap-payload family (`String`,
    /// `Binary`, `Bits`). The result is a fresh heap value of the
    /// same `kind`. Lowering always emits `consumes_lhs: false`, where
    /// both operands flow through unchanged. Consume fusion sets
    /// `consumes_lhs` when `lhs` provably dies here, which lets the
    /// backend grow `lhs`'s storage in place when it is uniquely held
    /// and otherwise copy and release `lhs` itself.
    Concat {
        consumes_lhs: bool,
        dest: ValueId,
        kind: ConcatKind,
        lhs: ValueId,
        rhs: ValueId,
    },
    /// `dest = <constant>`.
    Const { dest: ValueId, value: ConstValue },
    /// Hand the storage held by `local`'s slot to the consuming site
    /// that follows. Consume fusion emits this in place of the death
    /// it fused away. Afterwards the slot holds a dead value that
    /// nothing reads before a `LocalWrite` or the frame exit.
    /// Produces no value.
    ConsumeLocal { local: IRLocalId },
    /// `dest = deep_copy(source)`. A process-boundary copy that
    /// shares no heap storage with `source`, transitively. Lowering
    /// emits one at send and spawn sites, since rc bookkeeping is
    /// unsynchronized and sharing across processes is unsound. The
    /// source stays live. Backends handle leaf types inline. Heap
    /// composites are rewritten by [`crate::elaborate`] into a glue
    /// `Call` before they reach a backend.
    DeepCopy {
        dest: ValueId,
        source: ValueId,
        ty: IRType,
    },
    /// `dest = <ty>.<variant>(<payload>)`. `tag` is the variant's
    /// 0-based position in [`crate::IREnumDecl::variants`] (also
    /// the wire byte of the LLVM tag field), and `payload` carries the
    /// already-lowered init values for the variant's payload fields
    /// (Unit/Tuple/Struct shapes, with struct-variant inits
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
    /// gate. Seal validates `tag` / `payload_index` / `field_type`
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
    /// the [`crate::IRStructDecl`] at lower time), and `struct_symbol`
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
    /// the rebuild in their own way: eval clones the field vec and
    /// swaps one slot, while LLVM `alloca`s the receiver, GEP-stores the
    /// new field, and reloads. Heap-typed leaf overwrites are the
    /// IR-lowerer's responsibility. It must emit a synthetic
    /// `DropLocal`-style free of the previous payload before the
    /// `FieldSet` (mirrors the local-reassignment overwrite drop in
    /// body lowering) so the new write does not leak.
    FieldSet {
        base: ValueId,
        dest: ValueId,
        field_index: u32,
        field_type: IRType,
        struct_symbol: IRSymbol,
        value: ValueId,
    },
    /// `dest = base.<indirect_slot> != null`. Synthesized drop glue
    /// uses this guard because a declared but never-written local
    /// carries an all-zero aggregate value.
    IndirectPresent {
        base: ValueId,
        dest: ValueId,
        slot: IRIndirectSlot,
    },
    /// Release the ownership held by `local`'s slot. Lowering emits
    /// one at every function exit for slots whose [`IRType`] is
    /// heap-managed. Backends handle leaf types inline. Heap
    /// composites are rewritten by [`crate::elaborate`] into a glue
    /// `Call`. Produces no value.
    DropLocal { local: IRLocalId, ty: IRType },
    /// Release the ownership held by `value`. Register-keyed analog
    /// of [`Self::DropLocal`], emitted where an owner dies without a
    /// slot, such as the old payload of a [`Self::FieldSet`] or a
    /// dead temporary. Produces no value.
    DropValue { value: ValueId, ty: IRType },
    /// Declare a local-variable storage slot. Emitted exactly once
    /// per [`IRLocalId`] per function in the entry block (LLVM hoists
    /// the `alloca`, and eval inserts a fresh hashmap entry). The LLVM
    /// backend zero-initializes the slot at the decl site, so a
    /// `DropLocal` on a path that never wrote the slot (an untaken
    /// `receive` arm's payload local, say) releases nothing, because the
    /// runtime rc primitives treat null as a no-op. Produces no
    /// value.
    LocalDecl { local: IRLocalId, ty: IRType },
    /// Read the current contents of `local` into a fresh `ValueId`.
    /// `ty` matches the declaring `LocalDecl`'s `ty`. LLVM lowers to
    /// `load`, and eval clones the hashmap entry.
    LocalRead {
        dest: ValueId,
        local: IRLocalId,
        ty: IRType,
    },
    /// Write `value` into the slot named by `local`. Used for surface
    /// assignments and for parameter promotion (one `LocalWrite` per
    /// param at function entry). LLVM lowers to `store`. Produces no
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
    /// [`FunctionKind::Closure`] body, where `capture_index` keys into
    /// that kind's `env_layout`. No `StoreCapture` counterpart, as
    /// captures are structurally read-only inside the body.
    LoadCapture {
        capture_index: u32,
        dest: ValueId,
        ty: IRType,
    },
    /// `dest = closure.env.<capture_index>`: read a capture out of
    /// another closure value's env. Only valid inside a
    /// [`FunctionKind::EqClosureGlue`] body, whose `other` parameter
    /// is a closure built from the same body as the glue's own env,
    /// so `capture_index` keys into the same `env_layout`.
    LoadCaptureOf {
        capture_index: u32,
        closure: ValueId,
        dest: ValueId,
        ty: IRType,
    },
    /// `dest = <pool[const_id]>`: load a pooled compound constant.
    /// `const_id` keys an entry on [`crate::IRPackage::constants`], and
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
    /// `dest = base.<index>`. Tuple analog of [`Self::FieldGet`],
    /// index-addressed because tuples are structural and have no
    /// decl to name fields against. `element_type` is the projected
    /// element's [`IRType`] (cached from the tuple shape at lower
    /// time).
    TupleGet {
        base: ValueId,
        dest: ValueId,
        element_type: IRType,
        index: u32,
    },
    /// `dest = (e0, e1, ...)`. Tuple analog of [`Self::StructInit`]:
    /// `elements` are in positional order and `ty` is the tuple's
    /// element type vector. Backends materialize as alloca +
    /// per-element store + load.
    TupleInit {
        dest: ValueId,
        elements: Vec<ValueId>,
        ty: Vec<IRType>,
    },
    /// `dest = <op> operand`. `operand_ty` mirrors
    /// [`IRInstruction::BinaryOp`]'s field. Backends key negation
    /// width, signedness, and the `-MIN` overflow guard on it.
    UnaryOp {
        dest: ValueId,
        op: IRUnaryOp,
        operand: ValueId,
        operand_ty: IRType,
    },
    /// `dest = spawn wrapper(config)`. Materialize a new process
    /// running `wrapper` with `config` as its `i8*` payload. The
    /// LLVM backend serializes `config`'s bytes into a fresh
    /// allocation and calls `koja_rt_spawn(wrapper_fn_ptr, &bytes,
    /// sizeof)`, while eval declines (no scheduler). `dest` is the
    /// returned `Ref<M, R>` (by-value struct wrapping the pid).
    Spawn {
        config: ValueId,
        config_type: IRType,
        dest: ValueId,
        ref_type: IRSymbol,
        wrapper: IRSymbol,
    },
    /// `process_exit(reason)`: record the terminating process's exit
    /// reason on its control block. `reason` is an `Int64` SSA value
    /// holding the wire code (0=Normal, 1=Shutdown, ...), emitted in the
    /// process-body tail from the process's own `StopReason`. LLVM lowers
    /// it to `koja_rt_process_exit(i64)`, and eval routes it to
    /// `scheduler::process_exit`. Produces no value.
    ProcessExit { reason: ValueId },
    /// `set_priority(tag)`: hand the current process's scheduling
    /// weight to the runtime. `tag` is an `Int64` SSA value holding the
    /// wire weight (0=Low, 1=Normal, 2=High), emitted once per process
    /// body after `start` succeeds (see
    /// `lower::process::emit_apply_priority`). LLVM lowers it to
    /// `koja_rt_set_priority(i64)`, and eval routes it to
    /// `scheduler::set_priority`. Produces no value.
    SetPriority { tag: ValueId },
    /// `yield_check()`: a cooperative preemption point inserted by the
    /// `yield_checks` pass at loop back-edges and before each tail call.
    /// Spends one reduction from the running process's budget, and when it
    /// hits zero the process re-queues. LLVM lowers it to
    /// `koja_rt_yield_check()`, and eval routes it to `scheduler::reduce`.
    /// No operands, no value.
    YieldCheck,
    /// `dest = receive arms after?`. Block on the current process's
    /// mailbox. On message arrival, dispatch to the matching arm
    /// based on the envelope tag (business vs lifecycle), and on
    /// `after` timeout, run the after-body. Each arm binds a
    /// payload local from the message buffer. `result_type` is the
    /// joined type of every arm tail.
    Receive {
        after: Option<ReceiveAfter>,
        arms: Vec<ReceiveArm>,
        dest: ValueId,
        result_type: IRType,
    },
    /// `dest = widen(value)`: losslessly extend a sized numeric
    /// `value` (typed `from`) into its hub type `to`: sign-extend
    /// signed integer sources, zero-extend unsigned sources, `fpext`
    /// a `Float32` into `Float64`. Lowered from the typecheck-stamped
    /// [`koja_ast::coercion::Coercion::NumericWiden`] at every
    /// sized-numeric -> hub flow site (assignments, struct fields,
    /// args, returns, enum payloads, consts).
    NumericWiden {
        dest: ValueId,
        from: IRType,
        to: IRType,
        value: ValueId,
    },
    /// `dest = <ty>.wrap(value)`: box `value` (typed `member_type`,
    /// statically a member of `ty`) into a tagged union value of
    /// type `ty`. `member_index` is the 0-based offset of
    /// `member_type` in the union's canonical (sorted) member list,
    /// used as the runtime tag byte. Lowered from the typecheck-
    /// stamped [`koja_ast::coercion::Coercion::UnionWiden`] at every
    /// member->union flow site (assignments, struct fields, args,
    /// returns).
    UnionWrap {
        dest: ValueId,
        member_index: u8,
        member_type: IRType,
        ty: IRType,
        value: ValueId,
    },
    /// `dest = <value>.tag` (`Int8`). Match-arm CFG compares this
    /// against the constant member-index for each union arm. The
    /// counterpart of [`Self::EnumTagGet`] for the union family.
    UnionTagGet {
        dest: ValueId,
        ty: IRType,
        value: ValueId,
    },
    /// `dest = <value>.payload as <member_type>`. Only well-defined
    /// on the success edge of a preceding tag-eq gate. Seal
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
/// envelope shape the arm matches, `payload_local` is the local
/// slot the payload binds into (declared with `payload_type` in
/// the same function), and `body` is the basic block the arm runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveArm {
    pub body: IRBlockId,
    pub payload_local: IRLocalId,
    pub payload_type: IRType,
    pub tag: ReceiveTag,
}

/// Envelope kind a receive arm matches. `IOReady` and `ExitSignal`
/// arms aren't written by source lowering. The `elaborate` delivery
/// sub-passes synthesize them for a `Process` whose message type `M`
/// includes `IOReady` / `Process.ExitSignal`, so runtime-delivered
/// events reach the business `handle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveTag {
    Business,
    ExitSignal,
    IOReady,
    Lifecycle,
}

impl ReceiveTag {
    /// Wire byte the runtime stamps in the envelope tag header. Mirrors
    /// `koja-runtime-core/src/wire.rs` (`TAG_*`) by spec, not a shared
    /// type, and a unit test pins the two against each other.
    pub fn wire_byte(self) -> u8 {
        match self {
            Self::Business => 0,
            Self::ExitSignal => 4,
            Self::IOReady => 2,
            Self::Lifecycle => 1,
        }
    }
}

/// `after timeout body` clause on an [`IRInstruction::Receive`].
/// `timeout` is an `Int64`-typed SSA value (milliseconds), and
/// `body` is the basic block the timeout path runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveAfter {
    pub body: IRBlockId,
    pub timeout: ValueId,
}

impl IRInstruction {
    /// The `ValueId` this instruction defines, if any. Storage-slot
    /// and ownership side-effect variants return `None`, and
    /// everything else defines a destination.
    pub fn dest(&self) -> Option<ValueId> {
        match self {
            IRInstruction::BinaryConstruct { dest, .. }
            | IRInstruction::BinaryMatch { dest, .. }
            | IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::CallClosure { dest, .. }
            | IRInstruction::Clone { dest, .. }
            | IRInstruction::ClosureEquals { dest, .. }
            | IRInstruction::Concat { dest, .. }
            | IRInstruction::Const { dest, .. }
            | IRInstruction::DeepCopy { dest, .. }
            | IRInstruction::EnumConstruct { dest, .. }
            | IRInstruction::EnumPayloadFieldGet { dest, .. }
            | IRInstruction::EnumTagGet { dest, .. }
            | IRInstruction::FieldGet { dest, .. }
            | IRInstruction::FieldSet { dest, .. }
            | IRInstruction::IndirectPresent { dest, .. }
            | IRInstruction::LoadCapture { dest, .. }
            | IRInstruction::LoadCaptureOf { dest, .. }
            | IRInstruction::LoadConst { dest, .. }
            | IRInstruction::LocalRead { dest, .. }
            | IRInstruction::MakeClosure { dest, .. }
            | IRInstruction::NumericWiden { dest, .. }
            | IRInstruction::Receive { dest, .. }
            | IRInstruction::Spawn { dest, .. }
            | IRInstruction::StructInit { dest, .. }
            | IRInstruction::TupleGet { dest, .. }
            | IRInstruction::TupleInit { dest, .. }
            | IRInstruction::UnaryOp { dest, .. }
            | IRInstruction::UnionPayloadGet { dest, .. }
            | IRInstruction::UnionTagGet { dest, .. }
            | IRInstruction::UnionWrap { dest, .. } => Some(*dest),
            IRInstruction::ConsumeLocal { .. }
            | IRInstruction::DropLocal { .. }
            | IRInstruction::DropValue { .. }
            | IRInstruction::LocalDecl { .. }
            | IRInstruction::LocalWrite { .. }
            | IRInstruction::ProcessExit { .. }
            | IRInstruction::SetPriority { .. }
            | IRInstruction::YieldCheck => None,
        }
    }

    /// Whether this instruction reads `value` as an operand.
    pub fn uses_value(&self, value: ValueId) -> bool {
        match self {
            IRInstruction::BinaryConstruct { segments, .. } => {
                segments.iter().any(|segment| match segment {
                    LoweredBinarySegment::Float { value: v, .. }
                    | LoweredBinarySegment::Integer { value: v, .. }
                    | LoweredBinarySegment::String { value: v, .. } => *v == value,
                })
            }
            IRInstruction::BinaryMatch { subject, .. } => *subject == value,
            IRInstruction::BinaryOp { lhs, rhs, .. }
            | IRInstruction::ClosureEquals { lhs, rhs, .. }
            | IRInstruction::Concat { lhs, rhs, .. } => *lhs == value || *rhs == value,
            IRInstruction::Call { args, .. } => args.contains(&value),
            IRInstruction::CallClosure { args, callee, .. } => {
                *callee == value || args.contains(&value)
            }
            IRInstruction::Clone { source, .. } | IRInstruction::DeepCopy { source, .. } => {
                *source == value
            }
            IRInstruction::Const { .. }
            | IRInstruction::ConsumeLocal { .. }
            | IRInstruction::DropLocal { .. }
            | IRInstruction::LoadCapture { .. }
            | IRInstruction::LoadConst { .. }
            | IRInstruction::LocalDecl { .. }
            | IRInstruction::LocalRead { .. }
            | IRInstruction::YieldCheck => false,
            IRInstruction::DropValue { value: v, .. } => *v == value,
            IRInstruction::EnumConstruct { payload, .. } => match payload {
                EnumPayloadInit::Struct(fields) => fields.iter().any(|field| field.value == value),
                EnumPayloadInit::Tuple(values) => values.contains(&value),
                EnumPayloadInit::Unit => false,
            },
            IRInstruction::EnumPayloadFieldGet { value: v, .. }
            | IRInstruction::EnumTagGet { value: v, .. }
            | IRInstruction::LocalWrite { value: v, .. }
            | IRInstruction::NumericWiden { value: v, .. }
            | IRInstruction::UnionPayloadGet { value: v, .. }
            | IRInstruction::UnionTagGet { value: v, .. }
            | IRInstruction::UnionWrap { value: v, .. } => *v == value,
            IRInstruction::FieldGet { base, .. }
            | IRInstruction::IndirectPresent { base, .. }
            | IRInstruction::TupleGet { base, .. } => *base == value,
            IRInstruction::FieldSet { base, value: v, .. } => *base == value || *v == value,
            IRInstruction::LoadCaptureOf { closure, .. } => *closure == value,
            IRInstruction::MakeClosure { captures, .. } => captures.contains(&value),
            IRInstruction::ProcessExit { reason } => *reason == value,
            IRInstruction::Receive { after, .. } => {
                after.as_ref().is_some_and(|after| after.timeout == value)
            }
            IRInstruction::SetPriority { tag } => *tag == value,
            IRInstruction::Spawn { config, .. } => *config == value,
            IRInstruction::StructInit { fields, .. } => {
                fields.iter().any(|field| field.value == value)
            }
            IRInstruction::TupleInit { elements, .. } => elements.contains(&value),
            IRInstruction::UnaryOp { operand, .. } => *operand == value,
        }
    }

    /// Whether this instruction reads, writes, declares, or drops the
    /// storage slot `local`, including receive-arm payload binds and
    /// binary-match segment binds.
    pub fn touches_local(&self, local: IRLocalId) -> bool {
        match self {
            IRInstruction::ConsumeLocal { local: l }
            | IRInstruction::DropLocal { local: l, .. }
            | IRInstruction::LocalDecl { local: l, .. }
            | IRInstruction::LocalRead { local: l, .. }
            | IRInstruction::LocalWrite { local: l, .. } => *l == local,
            IRInstruction::BinaryMatch { segments, .. } => {
                segments.iter().any(|segment| match segment {
                    LoweredBinaryPattern::BindInt { local: l, .. } => *l == local,
                    LoweredBinaryPattern::GreedyTail { local: l, .. } => *l == Some(local),
                    LoweredBinaryPattern::Discard { .. }
                    | LoweredBinaryPattern::LiteralBytes { .. }
                    | LoweredBinaryPattern::LiteralInt { .. } => false,
                })
            }
            IRInstruction::Receive { arms, .. } => {
                arms.iter().any(|arm| arm.payload_local == local)
            }
            _ => false,
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
    /// Reinvoke `callee` with `args` in the current frame. Stamped by
    /// the post-merge [`crate::tail_calls`] pass on call-then-return
    /// shapes where `callee` is the enclosing function, so only
    /// self-recursive tail calls appear. Backends rebind the
    /// parameters to `args` and jump to the body's start.
    TailCall {
        args: Vec<ValueId>,
        callee: IRSymbol,
    },
    /// Statically unreachable. Lowering emits this on the failure
    /// edge of an exhaustive `match` so the CFG stays well-formed
    /// even when typecheck has guaranteed every runtime value is
    /// covered. Eval treats it as a fatal panic, and LLVM lowers to the
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

    /// Whether `value` flows out of the block through this terminator,
    /// as a branch edge arg, the returned value, or a tail-call arg.
    pub fn uses_value(&self, value: ValueId) -> bool {
        match self {
            IRTerminator::Branch(target) => target.args.contains(&value),
            IRTerminator::CondBranch {
                cond,
                else_target,
                then_target,
            } => {
                *cond == value
                    || else_target.args.contains(&value)
                    || then_target.args.contains(&value)
            }
            IRTerminator::Return { value: returned } => *returned == Some(value),
            IRTerminator::TailCall { args, .. } => args.contains(&value),
            IRTerminator::Unreachable => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use koja_runtime_core::wire;

    use super::ReceiveTag;

    /// `wire_byte` mirrors the runtime's tag constants by spec. This
    /// dev-dependency-only check turns drift into a test failure without
    /// coupling the compiled crate to the runtime.
    #[test]
    fn receive_tag_wire_bytes_match_the_runtime() {
        assert_eq!(ReceiveTag::Business.wire_byte(), wire::TAG_BUSINESS);
        assert_eq!(ReceiveTag::ExitSignal.wire_byte(), wire::TAG_EXIT_SIGNAL);
        assert_eq!(ReceiveTag::IOReady.wire_byte(), wire::TAG_IO_READY);
        assert_eq!(ReceiveTag::Lifecycle.wire_byte(), wire::TAG_LIFECYCLE);
    }
}
