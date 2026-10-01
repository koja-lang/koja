//! Operand types for the struct family: `FieldGet`, `FieldSet`,
//! `IndirectPresent`, and `StructInit`.

use crate::function::{IRIndirectSlot, IRSymbol};
use crate::struct_decl::StructFieldInit;
use crate::types::{IRType, ValueId};

use super::{TypeScope, registered};

pub(super) fn seal_field_get(base: ValueId, struct_symbol: &IRSymbol, scope: &TypeScope<'_>) {
    scope.require_value_type(
        base,
        &IRType::Struct(struct_symbol.clone()),
        "FieldGet base",
    );
}

pub(super) fn seal_field_set(
    base: ValueId,
    field_type: &IRType,
    struct_symbol: &IRSymbol,
    value: ValueId,
    scope: &TypeScope<'_>,
) {
    scope.require_value_type(
        base,
        &IRType::Struct(struct_symbol.clone()),
        "FieldSet base",
    );
    scope.require_value_type(value, field_type, "FieldSet value");
}

/// The base must be a value of the declaration that owns `slot`.
pub(super) fn seal_indirect_present(base: ValueId, slot: &IRIndirectSlot, scope: &TypeScope<'_>) {
    let expected = match slot {
        IRIndirectSlot::EnumPayload { ty, .. } => IRType::Enum(ty.clone()),
        IRIndirectSlot::StructField { struct_symbol, .. } => IRType::Struct(struct_symbol.clone()),
    };
    scope.require_value_type(base, &expected, "IndirectPresent base");
}

/// Each field value must fit the slot its declaration names. Arity
/// and index-order violations panic in
/// `crate::seal::structs::seal_struct_ops`, so this pass only types
/// the in-range field values.
pub(super) fn seal_struct_init(fields: &[StructFieldInit], ty: &IRSymbol, scope: &TypeScope<'_>) {
    let declaration = registered(scope.declarations.struct_decl(ty), "struct", ty);
    for field in fields {
        if let Some(expected) = declaration.fields.get(field.index as usize) {
            scope.require_slot_value_type(field.value, &expected.ir_type, "StructInit field");
        }
    }
}
