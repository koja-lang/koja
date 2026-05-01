//! Composite [`Value`] construction and projection for the
//! [`crate::Interp`] backend.
//!
//! Houses the helpers that build [`StructValue`] / [`EnumValue`]
//! payloads from `IRInstruction` operands and the field-walk used by
//! both `FieldChain` (named-local root) and `FieldLoad` (operand
//! root). All helpers are pure functions over `&Frame` + IR data --
//! they never need a `&mut Interp` because they don't recurse into
//! `call`.

use std::rc::Rc;

use expo_ir::resolved::fields::ResolvedFieldStep;
use expo_ir::{EnumPayload, MonomorphizedTypeIdentifier, StructFieldInit};

use crate::error::RuntimeError;
use crate::frame::Frame;
use crate::value::{EnumValue, StructValue, Value, VariantPayload};

/// Build an `EnumConstruct` result by materializing the variant's
/// payload (struct fields, tuple elements, or unit) and packaging
/// the tag + variant name with it.
pub(crate) fn build_enum_value(
    frame: &Frame,
    mangled: &MonomorphizedTypeIdentifier,
    tag: u8,
    variant: &str,
    payload: &EnumPayload,
) -> Result<Value, RuntimeError> {
    let payload = build_variant_payload(frame, payload)?;
    Ok(Value::Enum(Rc::new(EnumValue {
        mangled: mangled.clone(),
        variant: variant.to_string(),
        tag,
        payload,
    })))
}

/// Build a `StructConstruct` result by materializing each field's
/// operand and pairing it with the source-level field name.
pub(crate) fn build_struct_value(
    frame: &Frame,
    mangled: &MonomorphizedTypeIdentifier,
    fields: &[StructFieldInit],
) -> Result<Value, RuntimeError> {
    let materialized = materialize_named_fields(frame, fields)?;
    Ok(Value::Struct(Rc::new(StructValue {
        mangled: mangled.clone(),
        fields: materialized,
    })))
}

/// Materialize an [`EnumPayload`] into the runtime [`VariantPayload`]
/// shape. Routes struct payloads through [`materialize_named_fields`]
/// so they share the same field-build path as struct construction.
pub(crate) fn build_variant_payload(
    frame: &Frame,
    payload: &EnumPayload,
) -> Result<VariantPayload, RuntimeError> {
    match payload {
        EnumPayload::Struct(fields) => {
            let materialized = materialize_named_fields(frame, fields)?;
            Ok(VariantPayload::Struct(materialized))
        }
        EnumPayload::Tuple(elements) => {
            let mut materialized = Vec::with_capacity(elements.len());
            for element in elements {
                materialized.push(frame.materialize(&element.value)?);
            }
            Ok(VariantPayload::Tuple(materialized))
        }
        EnumPayload::Unit => Ok(VariantPayload::Unit),
    }
}

/// Walk a `StructFieldInit` slice, materializing each operand and
/// pairing it with the source-level field name. Shared between
/// [`build_struct_value`] and the [`EnumPayload::Struct`] arm of
/// [`build_variant_payload`] because both consume the identical
/// shape (`Vec<StructFieldInit>` -> `Vec<(String, Value)>`).
pub(crate) fn materialize_named_fields(
    frame: &Frame,
    fields: &[StructFieldInit],
) -> Result<Vec<(String, Value)>, RuntimeError> {
    let mut materialized = Vec::with_capacity(fields.len());
    for field in fields {
        let value = frame.materialize(&field.value)?;
        materialized.push((field.name.clone(), value));
    }
    Ok(materialized)
}

/// Walk a sequence of [`ResolvedFieldStep`]s through a struct value,
/// projecting each hop by its `field_index`. Shared between
/// `IRInstruction::FieldChain` (root is a named local) and
/// `IRInstruction::FieldLoad` (root is an arbitrary operand) since
/// both reduce to "starting from a value, GEP through these
/// indices."
pub(crate) fn walk_field_steps(
    mut value: Value,
    steps: &[ResolvedFieldStep],
) -> Result<Value, RuntimeError> {
    for step in steps {
        let Value::Struct(struct_value) = value else {
            return Err(RuntimeError::TypeMismatch(format!(
                "field access expects struct receiver, got {value:?}"
            )));
        };
        let index = step.field_index as usize;
        let (_, field_value) = struct_value.fields.get(index).ok_or_else(|| {
            RuntimeError::Unsupported(format!(
                "field index {index} out of bounds for {} (has {} fields)",
                struct_value.mangled,
                struct_value.fields.len()
            ))
        })?;
        value = field_value.clone();
    }
    Ok(value)
}
