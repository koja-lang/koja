//! Lowering-decision form of a fully-resolved package-level constant.
//! Produced by [`crate::lower::constants::resolve_const`] and
//! consumed only by the bridge in
//! [`crate::lower::constants::populate_constants`]; backends never
//! see this -- they get [`crate::IRConstantValue`] / [`crate::IROperand`].

use expo_ast::identifier::TypeIdentifier;

#[derive(Clone, Debug)]
pub enum ResolvedConst {
    Bool(bool),
    EnumVariant {
        enum_id: TypeIdentifier,
        variant: String,
        tag: u8,
    },
    Float(f64),
    Int(i64),
    String(String),
    Struct {
        struct_id: TypeIdentifier,
        /// Declared-order fields. Each value is a primitive-only
        /// `ResolvedConst` -- nested compounds aren't supported.
        fields: Vec<(String, ResolvedConst)>,
    },
}
