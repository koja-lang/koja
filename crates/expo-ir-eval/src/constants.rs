//! Runtime materialization of [`expo_ir::IRProgram::constants`] pool
//! entries into the interpreter's [`Value`] vocabulary. Called once
//! at [`crate::Interp`] construction; the `LoadConst` executor then
//! indexes that cache by [`expo_ir::IRConstId`].

use std::rc::Rc;

use expo_ir::{IRConstantValue, IROperand, MonomorphizedTypeIdentifier};

use crate::value::{EnumValue, StructValue, Value, VariantPayload};

/// Convert one pool entry to its runtime [`Value`].
pub(crate) fn materialize_ir_constant_value(value: &IRConstantValue) -> Value {
    match value {
        IRConstantValue::EnumVariant {
            enum_id,
            variant,
            tag,
        } => Value::Enum(Rc::new(EnumValue {
            mangled: MonomorphizedTypeIdentifier::new(enum_id.qualified_name()),
            variant: variant.clone(),
            tag: *tag,
            payload: VariantPayload::Unit,
        })),
        IRConstantValue::String(s) => Value::String(Rc::new(s.clone())),
        IRConstantValue::Struct { struct_id, fields } => {
            let materialized = fields
                .iter()
                .map(|(name, operand)| (name.clone(), operand_to_value(operand)))
                .collect();
            Value::Struct(Rc::new(StructValue {
                mangled: MonomorphizedTypeIdentifier::new(struct_id.qualified_name()),
                fields: materialized,
            }))
        }
    }
}

/// Inline [`IROperand`] -> runtime [`Value`]. Only the four `Const*`
/// arms reach the constant pool; other arms are produced inside
/// function bodies.
fn operand_to_value(operand: &IROperand) -> Value {
    match operand {
        IROperand::ConstBool(b) => Value::Bool(*b),
        IROperand::ConstFloat(v) => Value::Float(*v),
        IROperand::ConstInt(v) => Value::Int(*v),
        IROperand::ConstStr(s) => Value::String(Rc::new(s.clone())),
        IROperand::Local(_) | IROperand::Unit => {
            unreachable!("non-const IROperand reached operand_to_value")
        }
    }
}
