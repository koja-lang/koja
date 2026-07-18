use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::function::{IRBlockId, IRFunction, IRInstruction, IRSymbol, ReceiveArm, ReceiveTag};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::struct_decl::StructFieldInit;
use crate::types::{IRType, ValueId};

use super::{find_enum, find_struct};

const OPTION_NONE_VARIANT: &str = "None";

pub(super) struct BusinessEnvelope {
    pub body: IRBlockId,
    pub message_type: IRType,
    pub none_tag: IRVariantTag,
    pub option_symbol: IRSymbol,
    pub pair_symbol: IRSymbol,
    pub payload_local: IRLocalId,
}

pub(super) fn append_delivery_arm(
    function: &mut IRFunction,
    block_index: usize,
    receive_index: usize,
    arm: ReceiveArm,
) {
    function.blocks[0]
        .instructions
        .push(IRInstruction::LocalDecl {
            local: arm.payload_local,
            ty: arm.payload_type.clone(),
        });
    let IRInstruction::Receive { arms, .. } =
        &mut function.blocks[block_index].instructions[receive_index]
    else {
        panic!("IR elaborate: planned receive vanished before delivery-arm synthesis");
    };
    arms.push(arm);
}

pub(super) fn envelope_instructions(
    message: ValueId,
    reply_none: ValueId,
    envelope: ValueId,
    business: &BusinessEnvelope,
) -> [IRInstruction; 3] {
    [
        IRInstruction::EnumConstruct {
            dest: reply_none,
            payload: EnumPayloadInit::Unit,
            tag: business.none_tag,
            ty: business.option_symbol.clone(),
        },
        IRInstruction::StructInit {
            dest: envelope,
            fields: vec![
                StructFieldInit {
                    index: 0,
                    value: message,
                },
                StructFieldInit {
                    index: 1,
                    value: reply_none,
                },
            ],
            ty: business.pair_symbol.clone(),
        },
        IRInstruction::LocalWrite {
            local: business.payload_local,
            value: envelope,
        },
    ]
}

pub(super) fn resolve_business_envelope(
    packages: &[IRPackage],
    arms: &[ReceiveArm],
    synthesized_tag: ReceiveTag,
) -> Option<BusinessEnvelope> {
    if arms.iter().any(|arm| arm.tag == synthesized_tag) {
        return None;
    }
    let business = arms.iter().find(|arm| arm.tag == ReceiveTag::Business)?;
    let IRType::Struct(pair_symbol) = &business.payload_type else {
        return None;
    };
    let [message_field, reply_field] = find_struct(packages, pair_symbol)?.fields.as_slice() else {
        return None;
    };
    let IRType::Enum(option_symbol) = &reply_field.ir_type else {
        return None;
    };
    let none_tag = find_enum(packages, option_symbol)?
        .variants
        .iter()
        .find(|variant| variant.name == OPTION_NONE_VARIANT)?
        .tag;
    Some(BusinessEnvelope {
        body: business.body,
        message_type: message_field.ir_type.clone(),
        none_tag,
        option_symbol: option_symbol.clone(),
        pair_symbol: pair_symbol.clone(),
        payload_local: business.payload_local,
    })
}
