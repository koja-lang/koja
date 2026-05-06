//! Small value types used throughout the IR vocabulary: value handles,
//! constant payloads, binary-op kinds, and the IR type lattice.

use crate::function::IRSymbol;

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
/// Integer + float variants mirror Expo's stdlib primitive structs
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

/// The IR type lattice. Mirrors [`ConstValue`] one-for-one on the
/// integer + float side: each Expo stdlib `Int{N}` / `UInt{N}` /
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
/// `String` is the first member of the bit-length-header family
/// shared with `Binary` / `Bits` (future variants). Layout matches
/// `expo-codegen`: the LLVM value is a single `i8*` whose pointee is
/// `[i64 bit_length][payload bytes]`, with the `i64` placed 8 bytes
/// **before** the pointer. `String`-specific rules: UTF-8 payload,
/// trailing `NUL`, `bit_length = byte_length * 8`. Move type per
/// `LANGUAGE.md`; this slice only emits unowned literal globals.
/// `CString` is a struct, not a member of this family.
///
/// `Struct(symbol)` names a user-declared (non-generic) struct by
/// the same mangled [`IRSymbol`] used as the key on
/// [`crate::IRPackage::structs`]. Field layout is recovered through
/// the matching [`crate::IRStructDecl`]; backends that need the
/// per-field width / offset thread that lookup directly. Generic
/// instantiations get a richer key in the follow-up generics slice.
///
/// `Enum(symbol)` names a user-declared (non-generic) enum by the
/// same mangled [`IRSymbol`] used as the key on
/// [`crate::IRPackage::enums`]. Variant layout is recovered through
/// the matching [`crate::IREnumDecl`]; the LLVM backend lays it out
/// as an outer opaque blob with per-variant complete + payload
/// structs (see [`crate::IREnumDecl`]'s module-level docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IRType {
    Bool,
    Enum(IRSymbol),
    Float32,
    Float64,
    Int8,
    Int16,
    Int32,
    Int64,
    String,
    Struct(IRSymbol),
    UInt8,
    UInt16,
    UInt32,
    UInt64,
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
