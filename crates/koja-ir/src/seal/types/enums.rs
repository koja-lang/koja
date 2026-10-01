//! Operand types for the enum family: `EnumConstruct`,
//! `EnumPayloadFieldGet`, and `EnumTagGet`.

use crate::enum_decl::{EnumPayloadInit, IRVariantPayload, IRVariantTag};
use crate::function::IRSymbol;
use crate::seal::seal_panic;
use crate::types::{IRType, ValueId};

use super::{TypeScope, registered};

/// Each payload value must fit the slot its variant declares.
/// Out-of-range tags panic in `crate::seal::enums::seal_enum_ops`,
/// so this pass only types the payload of a declared variant.
pub(super) fn seal_enum_construct(
    payload: &EnumPayloadInit,
    tag: IRVariantTag,
    ty: &IRSymbol,
    scope: &TypeScope<'_>,
) {
    let declaration = registered(scope.declarations.enum_decl(ty), "enum", ty);
    let Some(variant) = declaration.variants.get(tag.0 as usize) else {
        return;
    };
    seal_enum_payload_types(payload, &variant.payload, scope);
}

/// `EnumPayloadFieldGet` and `EnumTagGet` both read an enum value
/// of the named declaration.
pub(super) fn seal_enum_projection(value: ValueId, ty: &IRSymbol, scope: &TypeScope<'_>) {
    scope.require_value_type(value, &IRType::Enum(ty.clone()), "enum projection");
}

fn seal_enum_payload_types(
    actual: &EnumPayloadInit,
    expected: &IRVariantPayload,
    scope: &TypeScope<'_>,
) {
    match (actual, expected) {
        (EnumPayloadInit::Struct(actual), IRVariantPayload::Struct(expected)) => {
            for field in actual {
                if let Some(expected) = expected.get(field.index as usize) {
                    scope.require_slot_value_type(
                        field.value,
                        &expected.ir_type,
                        "EnumConstruct field",
                    );
                }
            }
        }
        (EnumPayloadInit::Tuple(actual), IRVariantPayload::Tuple(expected)) => {
            if actual.len() != expected.len() {
                seal_panic(&format!(
                    "{} EnumConstruct payload passes {} value(s), expected {}",
                    scope.owner,
                    actual.len(),
                    expected.len()
                ));
            }
            for (value, declared) in actual.iter().zip(expected) {
                scope.require_slot_value_type(*value, declared, "EnumConstruct payload");
            }
        }
        (EnumPayloadInit::Unit, IRVariantPayload::Unit) => {}
        // Payload shape mismatches panic in
        // `crate::seal::enums::seal_enum_ops`.
        _ => {}
    }
}
