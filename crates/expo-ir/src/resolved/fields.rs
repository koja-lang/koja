//! Resolved types for struct field access, field paths, and struct name
//! resolution. These carry enough information to drive LLVM emission without
//! any backend dependency.

use expo_ast::identifier::TypeIdentifier;
use expo_ast::types::Type;

/// One step in a resolved field path: the field index and its Expo type.
pub struct ResolvedFieldStep {
    /// The zero-based index of this field within its parent struct's layout.
    pub field_index: u32,
    /// The resolved Expo type of this field.
    pub field_type: Type,
}

/// A resolved chain of field accesses from a base variable (e.g.
/// `self.span.start.line` resolves to base `"self"` with three steps).
pub struct ResolvedChain {
    /// The name of the root variable (e.g. `"self"`, `"point"`).
    pub base_name: String,
    /// The Expo type of the root variable.
    pub base_type: Type,
    /// Each successive field access in the chain, in order.
    pub steps: Vec<ResolvedFieldStep>,
}

/// A resolved struct field for construction: its type, layout index, and name.
pub struct ResolvedStructField {
    /// The resolved type of this field.
    pub field_type: Type,
    /// The zero-based index of this field in the struct layout.
    pub index: u32,
    /// The source-level field name.
    pub name: String,
}

/// The resolved struct name for a method call receiver, carrying the base
/// name, mangled name, and type args so callers never need to re-parse.
pub struct ResolvedStructName {
    /// The unmangled base type name (e.g. `"List"`, `"Point"`).
    pub base: String,
    /// The package-qualified identifier, if known.
    pub identifier: Option<TypeIdentifier>,
    /// The mangled name used for type registry lookup (e.g. `"List_$Int32$"`).
    pub mangled: String,
    /// The concrete type arguments (empty for non-generic types).
    pub type_args: Vec<Type>,
}

/// The decision a union-wrap operation makes about how to box a value
/// into its surrounding `Type::Union`: the discriminant tag (= the
/// member's position in the union) and the mangled name of the union
/// type so emission can look up its LLVM `StructType`.
pub struct ResolvedUnionMember {
    /// Discriminant tag = the source type's position within the
    /// union's member list, computed at lowering time.
    pub tag: u64,
    /// Mangled name of the union type, the key into `LLVMTypeCache`'s
    /// monomorphized table.
    pub union_mangled: String,
}
