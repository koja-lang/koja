//! Resolved metadata for the auto-synthesized `*_format` functions.
//!
//! Lowering walks the type context to collect the variants/fields and the
//! synthesized symbol name; emission consumes these and builds the LLVM
//! switch / GEP / snprintf scaffolding.

use expo_ast::types::Type;
use expo_typecheck::context::VariantInfo;

/// Resolved metadata for synthesizing an enum format function.
pub struct ResolvedEnumFormatInfo {
    pub function_name: String,
    pub variants: Vec<VariantInfo>,
}

/// Which formatting strategy to use for a given type.
pub enum ResolvedFormatKind {
    Enum,
    PrimitiveIntrinsic,
    Struct,
}

/// Resolved metadata for synthesizing a struct format function.
pub struct ResolvedStructFormatInfo {
    pub fields: Vec<(String, Type)>,
    pub function_name: String,
}
