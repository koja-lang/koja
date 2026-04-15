//! Resolved types for enum equality comparison. These describe the shape of
//! each variant so the backend can generate field-by-field comparison code
//! without re-querying the type context.

use expo_ast::types::Type;

/// The resolved field structure of an enum variant for construction.
pub enum ResolvedVariantFields {
    /// Named fields with name, layout index, and type.
    Struct { fields: Vec<(String, u32, Type)> },
    /// Positional fields with their types.
    Tuple { element_types: Vec<Type> },
    /// No payload.
    Unit,
}

/// The resolved field structure of an enum variant for equality comparison.
pub enum ResolvedVariantEq {
    /// Named-field variant: compare each field by type.
    Struct { field_types: Vec<Type> },
    /// Tuple variant: compare each element by type.
    Tuple { field_types: Vec<Type> },
    /// Unit variant: equal if tags match (no payload to compare).
    Unit,
}

/// All the information needed to emit structural equality for an enum type.
pub struct ResolvedEnumEq {
    /// The mangled enum type name (e.g. `"Option_$Int32$"`).
    pub mangled: String,
    /// Each variant's name and its equality-comparison shape.
    pub variants: Vec<(String, ResolvedVariantEq)>,
}
