//! Resolved types for constant initializers: literals, unit enum variants,
//! and struct constants. The `resolve_const` function itself stays in codegen
//! because it depends on `parse_int_literal`.

use expo_ast::ast::FieldInit;
use expo_ast::types::Type;

/// The semantic kind of a constant initializer, determined without touching
/// any backend.
pub enum ResolvedConst {
    /// A boolean literal (`true` / `false`).
    Bool(bool),
    /// A unit enum variant used as a constant (e.g. `Color.Red`).
    EnumVariant { enum_name: String, variant: String },
    /// A floating-point literal.
    Float(f64),
    /// An integer literal.
    Int(i64),
    /// A string literal (after interpolation parts are joined).
    String(String),
    /// A struct literal used as a constant initializer.
    Struct {
        fields: Vec<FieldInit>,
        struct_name: String,
    },
}

/// Resolved metadata for a constant enum variant.
pub struct ResolvedConstEnum {
    /// The discriminant tag value for this variant.
    pub tag: u8,
}

/// Resolved metadata for a constant struct initializer.
pub struct ResolvedConstStruct {
    /// The struct's fields in declaration order, each with name and type.
    pub field_types: Vec<(String, Type)>,
}
