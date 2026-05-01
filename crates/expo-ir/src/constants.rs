//! IR-native value type for entries in [`crate::IRProgram::constants`].
//!
//! Restricted to "compounds with backing storage" -- primitives
//! (`Bool` / `Float` / `Int`) inline as [`IROperand`] at use sites
//! and are unrepresentable here. The bridge from
//! [`crate::resolved::constants::ResolvedConst`] runs once in
//! [`crate::lower::constants::populate_constants`].

use expo_ast::identifier::TypeIdentifier;

use crate::values::IROperand;

/// Backend-facing value of one [`crate::IRProgram::constants`] entry.
#[derive(Clone, Debug)]
pub enum IRConstantValue {
    /// Unit-payload enum variant with the discriminant tag
    /// pre-resolved at lower time.
    EnumVariant {
        enum_id: TypeIdentifier,
        variant: String,
        tag: u8,
    },
    /// Pooled string constant. Anonymous string literals still flow
    /// through [`IROperand::ConstStr`] inline.
    String(String),
    /// Pooled struct constant. `fields` is in declared order; values
    /// are always primitive [`IROperand`]s.
    Struct {
        struct_id: TypeIdentifier,
        fields: Vec<(String, IROperand)>,
    },
}
