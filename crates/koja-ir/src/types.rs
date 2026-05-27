//! Small value types used throughout the IR vocabulary: value handles,
//! constant payloads, binary-op kinds, and the IR type lattice.

use crate::function::IRSymbol;
use crate::local::IRLocalId;

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
///
/// Integer + float variants mirror Koja's stdlib primitive structs
/// 1:1 — width and signedness (or precision) are part of the variant
/// identity, not separate fields. `Float32` / `Float64` are IEEE 754
/// payloads (copy types per `LANGUAGE.md`). `String` carries raw
/// UTF-8; backends materialize per [`IRType::String`].
///
/// **Transient invariant**: the seal pass currently asserts only
/// `Int64` / `Float64` flow through. The other width variants exist
/// in the vocabulary so future stdlib stub expansion + literal width
/// inference can stamp them without reshuffling the IR shape.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    /// Empty / literal-only `Binary` payload — exactly `bytes.len()`
    /// payload bytes, header `bit_length = bytes.len() * 8`. No
    /// trailing NUL. Segment-built `Binary` values flow through
    /// [`crate::IRInstruction::BinaryConstruct`] instead, since
    /// runtime segment values can't be folded into a `ConstValue`.
    Binary(Vec<u8>),
    /// Empty / literal-only `Bits` payload — `bit_length` may be a
    /// non-multiple of 8. Backends materialize `ceil(bit_length / 8)`
    /// payload bytes; trailing bits in the last byte must be
    /// zero-padded by the producer (the lowerer / typecheck layer)
    /// so the on-wire bytes match the on-disk constant pool.
    Bits {
        bytes: Vec<u8>,
        bit_length: u64,
    },
    Bool(bool),
    Float32(f32),
    Float64(f64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    String(String),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),
    Unit,
}

/// Binary operators the IR supports. Covers integer arithmetic,
/// boolean conjunction / disjunction, and equality / ordering
/// comparisons. All operators are eager — short-circuit lowering
/// lands with control-flow constructs.
///
/// **Overflow contract**: integer arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Mod`)
/// wraps on overflow (two's-complement). The interpreter currently
/// flags overflow as a `RuntimeError::IntegerOverflow` (transient
/// safety net); native LLVM emission uses plain `add`/`sub`/`mul`
/// without `nsw`/`nuw` flags — wrapping semantics. Aligning the
/// interpreter to wrap-on-overflow is a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRBinOp {
    Add,
    And,
    Div,
    Eq,
    Gt,
    GtEq,
    Lt,
    LtEq,
    Mod,
    Mul,
    NotEq,
    Or,
    Sub,
}

/// Unary operators the IR supports: boolean negation and integer
/// negation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRUnaryOp {
    Neg,
    Not,
}

/// The kind of `<>` concatenation. Mirrors the heap-payload family
/// 1:1 — the lowerer picks a variant from the operands' resolved
/// type and the LLVM backend keys on it to choose between inline
/// `memcpy` (byte-aligned `String` / `Binary`) and the runtime
/// `__koja_concat_bits` helper (`Bits`'s sub-byte alignment).
/// Eval keys on it to pick the matching `Value` constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcatKind {
    Binary,
    Bits,
    String,
}

/// Endianness modifier on integer / float binary segments. Mirrors
/// the AST [`koja_ast::ast::BinaryEndianness`] one-for-one but lives
/// in the IR vocabulary so the LLVM backend doesn't import AST
/// types. `Big` matches network byte order — the language default
/// when no `big`/`little` modifier is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryEndian {
    Big,
    Little,
}

/// Signedness modifier on an integer binary segment. Mirrors the
/// AST [`koja_ast::ast::BinarySignedness`] one-for-one. Does not
/// affect packing (we always pack the low `width` bits of the
/// already-evaluated value); kept on the IR for round-trip with
/// future binary patterns where signed vs unsigned changes the
/// extraction shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinarySign {
    Signed,
    Unsigned,
}

/// Layout pre-computed by the IR lowering layer for a single
/// `<<segments>>` literal. `total_bits` is the sum of every
/// segment's resolved bit width (typecheck rejects unresolvable —
/// e.g. dynamic — widths). `byte_aligned` is the convenience
/// `total_bits % 8 == 0` result, also used by the typecheck layer
/// to pick between [`IRType::Binary`] (aligned) and [`IRType::Bits`]
/// (not). Backends consume both fields directly so they don't need
/// to redo the arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBinaryLayout {
    pub total_bits: u64,
    pub byte_aligned: bool,
}

/// A single `<<segments>>` segment after IR lowering: the producer
/// SSA value plus everything the LLVM / eval backends need to pack
/// it into the result buffer at the right offset.
///
/// `bit_offset` is this segment's starting position **in bits**
/// within the result payload (segments are laid out in source
/// order; the lowering layer accumulates a running bit position).
/// `width` is the segment's bit width. For byte-aligned literals
/// the offset will always be a multiple of 8 — backends can fast-
/// path on `bit_offset % 8 == 0 && width % 8 == 0` to use inline
/// `memcpy` / byte-shift loops. For sub-byte segments either field
/// may be a non-multiple of 8 and the backend must call
/// `__koja_pack_bits`.
///
/// `value` is the SSA `ValueId` produced by lowering the
/// segment's AST `seg.value` expression — its `IRType` is whatever
/// the typecheck layer resolved (`Int64` for plain `::N`-sized
/// integer literals, `Float32`/`Float64` for floats, `String` for
/// string segments).
#[derive(Debug, Clone, PartialEq)]
pub enum LoweredBinarySegment {
    /// Integer-typed segment. `width` is in bits (`::N` or `::N
    /// byte` AST forms collapse to the same field). `endian`
    /// defaults to [`BinaryEndian::Big`] when no modifier is given.
    /// Sub-byte widths are valid only when the segment also lives at
    /// a sub-byte `bit_offset` (i.e. inside a non-byte-aligned
    /// literal); the byte-aligned shape rejects them.
    Integer {
        value: ValueId,
        width: u64,
        sign: BinarySign,
        endian: BinaryEndian,
        bit_offset: u64,
    },
    /// Float-typed segment. `width` is one of `32` (Float32) or
    /// `64` (Float64) — typecheck enforces. Always byte-aligned by
    /// language semantics so backends can skip the bit-pack path.
    Float {
        value: ValueId,
        width: u64,
        endian: BinaryEndian,
        bit_offset: u64,
    },
    /// String-typed segment. The SSA value is a `String`-typed
    /// payload pointer (the same pointer family `<>` operates on);
    /// backends `memcpy` the payload bytes into the result at
    /// `bit_offset / 8`. `byte_length` is the source-byte count of
    /// the string literal at typecheck time — we trust the typecheck
    /// layer to have stamped a constant width because dynamic-width
    /// segments are gated.
    String {
        value: ValueId,
        byte_length: u64,
        bit_offset: u64,
    },
}

impl LoweredBinarySegment {
    /// Bit offset of this segment's first bit within the result
    /// payload. Convenience for backends that don't need to match
    /// on the variant.
    pub fn bit_offset(&self) -> u64 {
        match self {
            Self::Integer { bit_offset, .. }
            | Self::Float { bit_offset, .. }
            | Self::String { bit_offset, .. } => *bit_offset,
        }
    }

    /// Bit width of this segment. For [`Self::String`] it's
    /// `byte_length * 8`.
    pub fn width(&self) -> u64 {
        match self {
            Self::Integer { width, .. } | Self::Float { width, .. } => *width,
            Self::String { byte_length, .. } => byte_length * 8,
        }
    }

    /// The SSA value the lowering layer minted for this segment.
    pub fn value(&self) -> ValueId {
        match self {
            Self::Integer { value, .. }
            | Self::Float { value, .. }
            | Self::String { value, .. } => *value,
        }
    }
}

/// One segment of a `<<segments>>` binary pattern after IR
/// lowering. Each variant carries the bit width and bit-offset
/// the LLVM emit phase needs to extract / compare / bind the
/// segment at the right position in the subject payload. Bindings
/// reference a pre-declared [`crate::local::IRLocalId`] slot —
/// the emit phase stamps the extracted value into the slot at the
/// matching `LocalWrite`-equivalent position so the arm body's
/// `LocalRead`s find it.
///
/// Pairs with [`LoweredBinaryMatchLayout`] (carries the running
/// `fixed_bits` total + a `has_greedy_tail` flag the length-check
/// emission keys on).
#[derive(Debug, Clone, PartialEq)]
pub enum LoweredBinaryPattern {
    /// Compare the segment at `bit_offset..bit_offset + width`
    /// against the constant `value` (sign-interpreted per
    /// `sign`). The arm fires only when every test in the
    /// segment list succeeds.
    LiteralInt {
        bit_offset: u64,
        endian: BinaryEndian,
        sign: BinarySign,
        value: i128,
        width: u64,
    },
    /// Compare the byte run at `bit_offset / 8` against the
    /// literal `bytes`. `bit_offset` is always byte-aligned —
    /// the typecheck layer rejects byte-misaligned string
    /// segments.
    LiteralBytes { bit_offset: u64, bytes: Vec<u8> },
    /// Extract an integer segment and bind it into the local
    /// slot `local`. Sign-extend when `sign == Signed` (fixes a
    /// v1 codegen bug where the modifier was ignored).
    BindInt {
        bit_offset: u64,
        endian: BinaryEndian,
        local: IRLocalId,
        sign: BinarySign,
        ty: IRType,
        width: u64,
    },
    /// Skip `width` bits without binding anything. Carried so the
    /// running `bit_offset` accumulator stays correct for the
    /// segments after a `_::N` discard.
    Discard { bit_offset: u64, width: u64 },
    /// Bind the remaining bits / bytes from `bit_offset` to the
    /// end of the subject into `local` (when `Some`). `ty` is
    /// [`IRType::Binary`] or [`IRType::Bits`] per the source
    /// annotation — typecheck has already ensured the segment is
    /// last and that the `Binary` variant has a byte-aligned
    /// prefix. `local: None` is the `_: Binary` / `_: Bits` shape
    /// (consume-the-rest discard, no SSA slot to write).
    GreedyTail {
        bit_offset: u64,
        local: Option<IRLocalId>,
        ty: IRType,
    },
}

impl LoweredBinaryPattern {
    /// Bit offset of this segment within the subject payload.
    /// Mirrors [`LoweredBinarySegment::bit_offset`] for the
    /// pattern-side family.
    pub fn bit_offset(&self) -> u64 {
        match self {
            Self::LiteralInt { bit_offset, .. }
            | Self::LiteralBytes { bit_offset, .. }
            | Self::BindInt { bit_offset, .. }
            | Self::Discard { bit_offset, .. }
            | Self::GreedyTail { bit_offset, .. } => *bit_offset,
        }
    }
}

/// Pre-computed bookkeeping for a binary pattern match. `fixed_bits`
/// is the total bit width of every segment except the greedy tail
/// (when present); the LLVM emit phase compares the subject's
/// runtime bit length against this to decide whether the arm can
/// fire at all. `has_greedy_tail` switches the length check between
/// equality (`fixed_bits == subject_bits`) and unsigned-greater-or-
/// equal (`subject_bits >= fixed_bits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoweredBinaryMatchLayout {
    pub fixed_bits: u64,
    pub has_greedy_tail: bool,
}

impl ConcatKind {
    /// The [`IRType`] this concatenation produces. Reflects the
    /// "result type matches operands" rule — both `lhs` and `rhs`
    /// share this type by typecheck-time invariant.
    pub fn ir_type(&self) -> IRType {
        match self {
            ConcatKind::Binary => IRType::Binary,
            ConcatKind::Bits => IRType::Bits,
            ConcatKind::String => IRType::String,
        }
    }
}

/// The IR type lattice. Mirrors [`ConstValue`] one-for-one on the
/// integer + float side: each Koja stdlib `Int{N}` / `UInt{N}` /
/// `Float{N}` primitive struct gets its own variant. Width and
/// signedness/precision are part of the variant identity, not
/// separate fields, so illegal states (e.g. `bits: 7`) are
/// unrepresentable.
///
/// `Float32` / `Float64` are IEEE 754 by-value primitives — **copy
/// types** per `LANGUAGE.md`, distinct from `String`'s move-type
/// status. `Float64` is the v1 alias for `Global.Float`; `Float32`
/// only enters via explicit annotations / casts (a future slice).
///
/// `String` / `Binary` / `Bits` are the bit-length-header family.
/// The LLVM value for each is a single default-AS pointer whose
/// pointee is `[i64 bit_length][payload bytes]`, with the `i64`
/// placed 8 bytes **before** the pointer. Per-type rules:
///
/// - `String`: UTF-8 payload, trailing `\0` (libc compat),
///   `bit_length = byte_length * 8`.
/// - `Binary`: arbitrary bytes, no terminator, `bit_length =
///   byte_length * 8` (always a multiple of 8).
/// - `Bits`: arbitrary bits, no terminator, `bit_length` may be a
///   non-multiple of 8; payload occupies `ceil(bit_length / 8)`
///   bytes and trailing bits in the last byte are zero-padded.
///
/// All three are move types per `LANGUAGE.md` — owned heap storage
/// freed at scope exit by [`crate::IRInstruction::DropLocal`]. The
/// `is_heap_type` classifier in
/// [`koja_ir::lower::ownership`] (module) treats them
/// uniformly. `CString` is a struct, not a member of this family.
///
/// `Struct(symbol)` names a user-declared (non-generic) struct by
/// the same mangled [`IRSymbol`] used as the key on
/// [`crate::IRPackage::structs`]. Field layout is recovered through
/// the matching [`crate::IRStructDecl`]; backends that need the
/// per-field width / offset thread that lookup directly. Generic
/// instantiations get a richer key in the follow-up generics slice.
///
/// `Enum(symbol)` names a user-declared enum by the same mangled
/// [`IRSymbol`] used as the key on [`crate::IRPackage::enums`].
/// Variant layout is recovered through the matching
/// [`crate::IREnumDecl`]; the LLVM backend lays it out as an outer
/// opaque blob with per-variant complete + payload structs (see
/// [`crate::IREnumDecl`]'s module-level docs).
///
/// `CPtr(pointee)` is the FFI pointer wrapper — at the LLVM layer
/// every `CPtr<T>` lowers to an opaque `ptr` (default address
/// space), regardless of `T`. The pointee is preserved here so
/// the IR carries enough type information for future safety checks
/// and for surfaces (mangling, debug printing) that distinguish
/// `CPtr<UInt8>` from `CPtr<Float32>`. Pointee variants are
/// themselves unrestricted — `CPtr<CPtr<T>>` is a valid shape.
///
/// `List(element)` is the heap-backed dynamic array. Layout is
/// `{ buf_ptr: i8*, length: i64, capacity: i64 }` regardless of
/// `T`; the element type is preserved so backends can compute
/// element size for indexed addressing. Like `CPtr`, `List` is
/// modeled as a primitive (no `IRStructDecl` ever materializes)
/// because all storage lives off-heap behind `buf_ptr`.
///
/// `Map(key, value)` and `Set(element)` are the heap-backed
/// hash-tables. Both share a common 4-field layout —
/// `{ entries_ptr: i8*, states_ptr: i8*, length: i64, capacity: i64 }` —
/// regardless of the inner types; backends specialize entry-stride
/// per `(K, V)` / `T` instantiation. Same primitive treatment as
/// `List`: no `IRStructDecl` materializes; storage lives off-heap.
///
/// **Concrete-only**: every variant of `IRType` names a fully
/// monomorphized type. There is no "generic parameter" variant —
/// generic-decl bodies are never lowered to `IRType`; instead
/// [`crate::generics::instantiate`] substitutes [`koja_ast::identifier::ResolvedType`]
/// templates against concrete args from the typecheck registry,
/// then lowers the substituted shape into concrete `IRType`s. This
/// is the IR vocabulary backends consume.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum IRType {
    Binary,
    Bits,
    Bool,
    CPtr(Box<IRType>),
    Enum(IRSymbol),
    Float32,
    Float64,
    /// First-class callable: a `{fn_ptr, env_ptr}` fat pointer.
    /// `params` excludes the implicit `env_ptr` slot, which
    /// [`crate::IRInstruction::CallClosure`] threads at call time.
    Function {
        params: Vec<IRType>,
        ret: Box<IRType>,
    },
    /// Heap-boxed `T`, stamped by [`crate::cycle::break_type_cycles`]
    /// on struct fields / enum payload slots that would otherwise
    /// be value-level recursive (`Tree.Branch(Tree, Tree)`,
    /// `Node.next: Option<Node>`). Backends lower as `ptr` and
    /// transparently box / unbox at construct / project sites.
    Indirect(Box<IRType>),
    Int8,
    Int16,
    Int32,
    Int64,
    List(Box<IRType>),
    Map {
        key: Box<IRType>,
        value: Box<IRType>,
    },
    Set(Box<IRType>),
    String,
    Struct(IRSymbol),
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    /// Tagged union of two or more member types. `mangled` is the
    /// canonical symbol (`Union_<m1>_or_<m2>...`) shared across
    /// every distinct surface union with the same canonical
    /// member set, so backends key their cached layout off it.
    /// `members` is the canonical (sorted) member type vector
    /// inherited from the surface `ResolvedType::Union`. Backends
    /// look up `mangled` in the program-level `UnionDecl` registry
    /// to discover `max_payload_size` for the `{ i8, [N x i8] }`
    /// LLVM struct layout.
    Union {
        mangled: IRSymbol,
        members: Vec<IRType>,
    },
    Unit,
}

impl IRType {
    /// True when this type is one of the float-family variants
    /// (`Float32`, `Float64`). Symmetrical with [`Self::is_int`] for
    /// uniform "any float" predicates.
    pub fn is_float(&self) -> bool {
        matches!(self, Self::Float32 | Self::Float64)
    }

    /// True when this type is one of the integer-family variants
    /// (`Int8`..`Int64`, `UInt8`..`UInt64`). Useful in places that
    /// want to handle "any integer" uniformly — e.g. typecheck
    /// "is this an integer expression" predicates.
    pub fn is_int(&self) -> bool {
        matches!(
            self,
            Self::Int8
                | Self::Int16
                | Self::Int32
                | Self::Int64
                | Self::UInt8
                | Self::UInt16
                | Self::UInt32
                | Self::UInt64
        )
    }
}
