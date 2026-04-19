//! Resolved struct and enum construction: the lowered description of a
//! `StructName { ... }` or `EnumName.Variant(...)` literal.
//!
//! Lowering decides the mangled name (triggering monomorphization for
//! generics), the field/variant layout (indices and Expo `Type`s), and the
//! resulting Expo type. Emission consumes these and is purely mechanical:
//! `to_llvm_type`, GEP, store.

use expo_ast::ast::BinaryEndianness;
use expo_ast::types::Type;

use crate::resolved::enums::ResolvedVariantFields;
use crate::resolved::fields::ResolvedStructField;

/// A struct literal after lowering. Carries the mangled type name (post
/// monomorphization for generics), the resolved field layout in source
/// (init) order, and the resulting Expo type.
pub struct ResolvedStructConstruction {
    /// One entry per source field initializer, in init order, with the
    /// layout index and resolved field type.
    pub fields: Vec<ResolvedStructField>,
    /// True when the struct has type parameters (and `mangled_name` is the
    /// monomorphized form like `"List_$Int32$"`).
    pub is_generic: bool,
    /// The mangled LLVM type key (e.g. `"Point"` or `"List_$Int32$"`).
    pub mangled_name: String,
    /// The resulting Expo type of the construction expression.
    pub result_type: Type,
}

/// The fully resolved layout of a `<<segments...>>` binary literal.
/// Computed without LLVM by walking the AST segments and validating
/// byte-alignment; emission then packs values according to each
/// [`ResolvedBinarySegment::kind`].
pub struct ResolvedBinaryLayout {
    pub segments: Vec<ResolvedBinarySegment>,
    pub total_bits: u64,
}

/// A single resolved binary segment with its kind and byte-aligned bit
/// width.
pub struct ResolvedBinarySegment {
    pub bit_width: u64,
    pub kind: ResolvedBinarySegmentKind,
}

/// The resolved kind of a single binary segment, determined without LLVM.
pub enum ResolvedBinarySegmentKind {
    Float,
    Integer { endianness: BinaryEndianness },
    String,
}

/// An enum variant construction after lowering. Carries the mangled enum
/// name, the variant name and tag, and the resolved payload shape.
pub struct ResolvedEnumConstruction {
    /// True when the enum has type parameters (and `mangled_name` is the
    /// monomorphized form like `"Option_$Int32$"`).
    pub is_generic: bool,
    /// The mangled LLVM enum key (e.g. `"Status"` or `"Option_$Int32$"`).
    pub mangled_name: String,
    /// The resulting Expo type of the construction expression.
    pub result_type: Type,
    /// The tag byte for this variant.
    pub tag: u64,
    /// The resolved field layout for this variant (struct, tuple, or unit).
    pub variant_fields: ResolvedVariantFields,
    /// The unqualified variant name (e.g. `"Some"` for `Option.Some`).
    pub variant_name: String,
}
