use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::function::{IRBlockId, IRFunction, IRInstruction, IRSymbol, ReceiveArm, ReceiveTag};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

use super::find_enum;

const OPTION_NONE_VARIANT: &str = "None";

pub(super) struct BusinessEnvelope {
    pub body: IRBlockId,
    pub envelope_elements: Vec<IRType>,
    pub message_type: IRType,
    pub none_tag: IRVariantTag,
    pub option_symbol: IRSymbol,
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
        IRInstruction::TupleInit {
            dest: envelope,
            elements: vec![message, reply_none],
            ty: business.envelope_elements.clone(),
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
    let IRType::Tuple(envelope_elements) = &business.payload_type else {
        return None;
    };
    let [message_type, reply_type] = envelope_elements.as_slice() else {
        return None;
    };
    let IRType::Enum(option_symbol) = reply_type else {
        return None;
    };
    let none_tag = find_enum(packages, option_symbol)?
        .variants
        .iter()
        .find(|variant| variant.name == OPTION_NONE_VARIANT)?
        .tag;
    Some(BusinessEnvelope {
        body: business.body,
        envelope_elements: envelope_elements.clone(),
        message_type: message_type.clone(),
        none_tag,
        option_symbol: option_symbol.clone(),
        payload_local: business.payload_local,
    })
}
