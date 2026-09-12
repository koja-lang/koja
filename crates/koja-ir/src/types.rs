//! Small value types used throughout the IR vocabulary: value handles,
//! constant payloads, binary-op kinds, and the IR type lattice.

use crate::function::IRSymbol;
use crate::local::IRLocalId;

/// Identifier of an SSA value within a single function. Values are
/// numbered in definition order starting from 0, and the same `ValueId`
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
/// 1:1. Width and signedness (or precision) are part of the variant
/// identity, not separate fields. `Float32` / `Float64` are IEEE 754
/// payloads (copy types per `LANGUAGE.md`). `String` carries raw
/// UTF-8, which backends materialize per [`IRType::String`].
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    /// Empty / literal-only `Binary` payload: exactly `bytes.len()`
    /// payload bytes, header `bit_length = bytes.len() * 8`. No
    /// trailing NUL. Segment-built `Binary` values flow through
    /// [`crate::IRInstruction::BinaryConstruct`] instead, since
    /// runtime segment values can't be folded into a `ConstValue`.
    Binary(Vec<u8>),
    /// Empty / literal-only `Bits` payload, where `bit_length` may be a
    /// non-multiple of 8. Backends materialize `ceil(bit_length / 8)`
    /// payload bytes, and trailing bits in the last byte must be
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
/// equality, and ordering comparisons. Surface `and` / `or` lower
/// directly to control flow, so eager logical operators cannot reach
/// the sealed IR.
///
/// **Fault contract**: arithmetic traps as an `ArithmeticError`
/// panic on both backends, at the operand type's width and
/// signedness. Integer `Add`/`Sub`/`Mul` trap on overflow. `Div`/
/// `Mod` trap on a zero divisor and on `MIN / -1`. Float arithmetic
/// traps when the IEEE result is non-finite, upholding the
/// finite-only `Float` invariant. Comparisons never trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRBinOp {
    Add,
    Div,
    Eq,
    Gt,
    GtEq,
    Lt,
    LtEq,
    Mod,
    Mul,
    NotEq,
    Sub,
}

/// Panic messages that depend on the operator. Operator-independent
/// messages live in [`crate::panics`].
impl IRBinOp {
    /// Panic message for a zero divisor (`Div` / `Mod` only).
    pub fn division_by_zero_message(&self) -> String {
        format!("division by zero in {}", self.surface_symbol())
    }

    /// Panic message for a non-finite float result.
    pub fn non_finite_message(&self) -> String {
        format!("non-finite float result in {}", self.surface_symbol())
    }

    /// Panic message for an integer overflow fault (including
    /// `MIN / -1` and `MIN % -1`).
    pub fn overflow_message(&self) -> String {
        format!("integer overflow in {}", self.surface_symbol())
    }

    /// Koja surface spelling, used in `ArithmeticError` panics.
    pub fn surface_symbol(&self) -> &'static str {
        match self {
            IRBinOp::Add => "+",
            IRBinOp::Div => "/",
            IRBinOp::Eq => "==",
            IRBinOp::Gt => ">",
            IRBinOp::GtEq => ">=",
            IRBinOp::Lt => "<",
            IRBinOp::LtEq => "<=",
            IRBinOp::Mod => "%",
            IRBinOp::Mul => "*",
            IRBinOp::NotEq => "!=",
            IRBinOp::Sub => "-",
        }
    }
}

/// Unary operators the IR supports: boolean negation and numeric
/// negation. `Neg` on an integer traps as an `ArithmeticError`
/// panic when the operand is the type's minimum (two's-complement
/// `-MIN` overflows). Float negation never traps (every finite
/// float has a representable negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRUnaryOp {
    Neg,
    Not,
}

/// The kind of `<>` concatenation. Mirrors the heap-payload family
/// 1:1. The lowerer picks a variant from the operands' resolved
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
/// types. `Big` matches network byte order, the language default
/// when no `big`/`little` modifier is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryEndian {
    Big,
    Little,
}

/// Signedness modifier on an integer binary segment. Mirrors the
/// AST [`koja_ast::ast::BinarySignedness`] one-for-one. Does not
/// affect packing, which always takes the low `width` bits of the
/// already-evaluated value. Binary patterns read it to pick sign or
/// zero extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinarySign {
    Signed,
    Unsigned,
}

/// Layout pre-computed by the IR lowering layer for a single
/// `<<segments>>` literal. `total_bits` is the sum of every
/// segment's resolved bit width (typecheck rejects unresolvable,
/// e.g. dynamic, widths). `byte_aligned` is the convenience
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
/// order, and the lowering layer accumulates a running bit position).
/// `width` is the segment's bit width. For byte-aligned literals
/// the offset will always be a multiple of 8, so backends can fast-
/// path on `bit_offset % 8 == 0 && width % 8 == 0` to use inline
/// `memcpy` / byte-shift loops. For sub-byte segments either field
/// may be a non-multiple of 8 and the backend must call
/// `__koja_pack_bits`.
///
/// `value` is the SSA `ValueId` produced by lowering the
/// segment's AST `seg.value` expression. Its `IRType` is whatever
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
    /// literal). The byte-aligned shape rejects them.
    Integer {
        value: ValueId,
        width: u64,
        sign: BinarySign,
        endian: BinaryEndian,
        bit_offset: u64,
    },
    /// Float-typed segment. `width` is one of `32` (Float32) or
    /// `64` (Float64), which typecheck enforces. Always byte-aligned by
    /// language semantics so backends can skip the bit-pack path.
    Float {
        value: ValueId,
        width: u64,
        endian: BinaryEndian,
        bit_offset: u64,
    },
    /// String-typed segment. The SSA value is a `String`-typed
    /// payload pointer (the same pointer family `<>` operates on),
    /// and backends `memcpy` the payload bytes into the result at
    /// `bit_offset / 8`. `byte_length` is the source-byte count of
    /// the string literal at typecheck time. We trust the typecheck
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
/// reference a pre-declared [`crate::local::IRLocalId`] slot, and
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
    /// literal `bytes`. `bit_offset` is always byte-aligned, as
    /// the typecheck layer rejects byte-misaligned string
    /// segments.
    LiteralBytes { bit_offset: u64, bytes: Vec<u8> },
    /// Extract an integer segment and bind it into the local
    /// slot `local`. Sign-extend when `sign == Signed`.
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
    /// annotation. Typecheck has already ensured the segment is
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
/// (when present). The LLVM emit phase compares the subject's
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
    /// "result type matches operands" rule, where both `lhs` and `rhs`
    /// share this type by typecheck-time invariant.
    pub fn ir_type(&self) -> IRType {
        match self {
            ConcatKind::Binary => IRType::Binary,
            ConcatKind::Bits => IRType::Bits,
            ConcatKind::String => IRType::String,
        }
    }
}

/// The IR type lattice. Every variant names a fully monomorphized
/// type. Generic decl bodies are never lowered to `IRType`, since
/// [`crate::generics::instantiate`] substitutes concrete args first.
///
/// Width and signedness are part of each integer and float variant's
/// identity, mirroring [`ConstValue`]. `Struct` and `Enum` name a
/// user declaration by the mangled [`IRSymbol`] that keys
/// [`crate::IRPackage::structs`] and [`crate::IRPackage::enums`].
/// `CPtr`, `List`, `Map`, and `Set` are primitives with no decl.
/// Backends own the storage layout of every variant.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum IRType {
    /// Arbitrary bytes, `bit_length` a multiple of 8.
    Binary,
    /// Arbitrary bits. The payload occupies `ceil(bit_length / 8)`
    /// bytes and the trailing bits of the last byte are zero.
    Bits,
    Bool,
    /// FFI pointer. The pointee is kept for mangling and display, and
    /// `CPtr<CPtr<T>>` is a valid shape.
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
    /// UTF-8 bytes with a trailing `\0` for libc compatibility.
    String,
    Struct(IRSymbol),
    /// Anonymous tuple. Identity is the element shape, so the type
    /// carries its elements inline like [`IRType::Function`] and no
    /// decl ever materializes. Backends lay out `{T0, T1, ...}`
    /// directly from the elements.
    Tuple(Vec<IRType>),
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    /// Tagged union of two or more member types. `mangled` is the
    /// canonical symbol (`Union_<m1>_or_<m2>...`) shared by every
    /// surface union with the same member set, and `members` is that
    /// set in canonical order. Backends key their layout off
    /// `mangled` through the program-level `UnionDecl` registry.
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

    /// True for types lowering acquires on binding and releases at
    /// scope exit. This is structural and conservative, since every
    /// struct, enum, tuple, and union counts whether or not it owns
    /// heap. The precise question is answered after merge by
    /// [`crate::elaborate::needs_drop`], and an all-`Copy` composite
    /// gets no glue.
    pub fn is_heap_managed(&self) -> bool {
        match self {
            Self::Binary | Self::Bits | Self::String => true,
            Self::Enum(_)
            | Self::Function { .. }
            | Self::Indirect(_)
            | Self::List(_)
            | Self::Map { .. }
            | Self::Set(_)
            | Self::Struct(_)
            | Self::Tuple(_)
            | Self::Union { .. } => true,
            Self::Bool
            | Self::CPtr(_)
            | Self::Float32
            | Self::Float64
            | Self::Int8
            | Self::Int16
            | Self::Int32
            | Self::Int64
            | Self::UInt8
            | Self::UInt16
            | Self::UInt32
            | Self::UInt64
            | Self::Unit => false,
        }
    }

    /// Bit width of an integer-family variant, `None` otherwise.
    pub fn int_bit_width(&self) -> Option<u32> {
        match self {
            Self::Int8 | Self::UInt8 => Some(8),
            Self::Int16 | Self::UInt16 => Some(16),
            Self::Int32 | Self::UInt32 => Some(32),
            Self::Int64 | Self::UInt64 => Some(64),
            _ => None,
        }
    }

    /// Signedness of an integer-family variant, `None` otherwise.
    pub fn int_sign(&self) -> Option<BinarySign> {
        match self {
            Self::Int8 | Self::Int16 | Self::Int32 | Self::Int64 => Some(BinarySign::Signed),
            Self::UInt8 | Self::UInt16 | Self::UInt32 | Self::UInt64 => Some(BinarySign::Unsigned),
            _ => None,
        }
    }

    /// True when this type is one of the integer-family variants
    /// (`Int8`..`Int64`, `UInt8`..`UInt64`). Useful in places that
    /// want to handle "any integer" uniformly, e.g. typecheck
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
