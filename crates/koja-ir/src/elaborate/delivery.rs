//! The `elaborate` delivery sub-pass. The runtime hands a process loop
//! bare payloads on their own receive tags (`IOReady`, `ExitSignal`),
//! but source lowering only dispatches `Business` and `Lifecycle`
//! arms, because the default `Process.run` is generic over `M` and the
//! message shape is unknown until monomorphization. This pass runs
//! post-monomorphize, where `M` is concrete, and for each `Receive`
//! whose business arm consumes a message that includes the payload it
//! synthesizes an arm that reshapes the bare payload into the
//! `(M, Option.None)` envelope the business body already consumes,
//! then branches there. Every step reuses an existing
//! `IRInstruction`, so no backend gains a new emission shape.
//!
//! [`DeliveryKind`] names the payload. [`super::io_ready`] and
//! [`super::exit_signal`] each supply the one kind-specific step,
//! which is how the bare payload becomes `M` (see [`Injection`]).

use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::function::{
    BranchTarget, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRSymbol, IRTerminator,
    ReceiveArm, ReceiveTag,
};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

use super::{exit_signal, find_enum, io_ready};

const OPTION_NONE_VARIANT: &str = "None";

/// A located, fully-resolved synthesis request. Gathered under a shared
/// borrow (decls live in the same package set the pass later mutates),
/// then applied, so every field is owned, with no borrow into
/// `packages`.
struct ArmPlan {
    block_index: usize,
    function: IRSymbol,
    package_index: usize,
    receive_index: usize,
    synthesis: Synthesis,
}

struct BusinessEnvelope {
    body: IRBlockId,
    envelope_elements: Vec<IRType>,
    message_type: IRType,
    none_tag: IRVariantTag,
    option_symbol: IRSymbol,
    payload_local: IRLocalId,
}

/// The bare payload a delivery arm receives.
#[derive(Clone, Copy)]
pub(super) enum DeliveryKind {
    ExitSignal,
    IoReady,
}

impl DeliveryKind {
    /// Label of the synthesized body block.
    fn label(self) -> &'static str {
        match self {
            Self::ExitSignal => "receive_exit_signal",
            Self::IoReady => "receive_io_ready",
        }
    }

    /// The bare payload type inside the business message `M`, and how
    /// it becomes `M`. `None` when `M` does not include the payload.
    fn resolve(self, message_type: &IRType) -> Option<(IRType, Injection)> {
        match self {
            Self::ExitSignal => exit_signal::resolve(message_type),
            Self::IoReady => io_ready::resolve(message_type),
        }
    }

    /// Receive tag of the synthesized arm.
    fn tag(self) -> ReceiveTag {
        match self {
            Self::ExitSignal => ReceiveTag::ExitSignal,
            Self::IoReady => ReceiveTag::IOReady,
        }
    }
}

/// How the bare payload becomes the business arm's message `M`:
/// wrapped into `M`'s union at `member_index`, or used directly when
/// `M` *is* the payload type.
pub(super) enum Injection {
    Direct,
    UnionWrap {
        member_index: u8,
        union_type: IRType,
    },
}

/// The synthesis inputs for one `Receive`, or `None` when it is not a
/// business loop whose message includes the payload (or it already has
/// the delivery arm).
struct Synthesis {
    business: BusinessEnvelope,
    injection: Injection,
    payload_type: IRType,
}

/// Add a synthesized `receive` arm and declare its payload local in
/// the entry block, so every arm local has a `LocalDecl` like the
/// lowered ones. A `receive` written as the first statement of a
/// method lives in the entry block itself, and the emitter walks
/// instructions in order, so in that case the decl goes right before
/// the `receive` rather than at the end of the block. It cannot go at
/// the front, because the entry block opens with the parameter
/// promotion prefix the emitter checks.
fn append_delivery_arm(
    function: &mut IRFunction,
    block_index: usize,
    receive_index: usize,
    arm: ReceiveArm,
) {
    let decl = IRInstruction::LocalDecl {
        local: arm.payload_local,
        ty: arm.payload_type.clone(),
    };
    let receive_index = if block_index == 0 {
        function.blocks[0].instructions.insert(receive_index, decl);
        receive_index + 1
    } else {
        function.blocks[0].instructions.push(decl);
        receive_index
    };
    let IRInstruction::Receive { arms, .. } =
        &mut function.blocks[block_index].instructions[receive_index]
    else {
        panic!("IR elaborate: planned receive vanished before delivery-arm synthesis");
    };
    arms.push(arm);
}

/// Splice the synthesized arm into one located `Receive`: declare the
/// payload slot, build the reshape body block, and append the arm.
/// Fresh ids are minted one past the function's current max, so
/// applying multiple plans to the same function stays collision-free.
fn apply(packages: &mut [IRPackage], kind: DeliveryKind, plan: ArmPlan) {
    let function = packages[plan.package_index]
        .functions
        .get_mut(plan.function.mangled())
        .expect("IR elaborate: planned function vanished before delivery-arm apply");
    let Synthesis {
        business,
        injection,
        payload_type,
    } = plan.synthesis;

    let payload_local = function.next_local_id();
    let body_id = function.next_block_id();
    let first_value = function.next_value_id().0;
    let payload = ValueId(first_value);
    let widened = ValueId(first_value + 1);
    let reply_none = ValueId(first_value + 2);
    let envelope = ValueId(first_value + 3);

    // Reshape the bare payload into the `(M, Option.None)` the
    // business body consumes, then branch into that body.
    let mut instructions = vec![IRInstruction::LocalRead {
        dest: payload,
        local: payload_local,
        ty: payload_type.clone(),
    }];
    let message = match injection {
        Injection::Direct => payload,
        Injection::UnionWrap {
            member_index,
            union_type,
        } => {
            instructions.push(IRInstruction::UnionWrap {
                dest: widened,
                member_index,
                member_type: payload_type.clone(),
                ty: union_type,
                value: payload,
            });
            widened
        }
    };
    instructions.extend(envelope_instructions(
        message, reply_none, envelope, &business,
    ));
    function.blocks.push(IRBasicBlock {
        id: body_id,
        instructions,
        label: kind.label().to_string(),
        params: Vec::new(),
        terminator: IRTerminator::Branch(BranchTarget::to(business.body)),
    });
    append_delivery_arm(
        function,
        plan.block_index,
        plan.receive_index,
        ReceiveArm {
            body: body_id,
            payload_local,
            payload_type,
            tag: kind.tag(),
        },
    );
}

/// Synthesize a `kind` receive arm into every process loop whose
/// business message includes that payload. Idempotent, so a `Receive`
/// that already has the arm is left untouched.
pub(super) fn deliver(packages: &mut [IRPackage], kind: DeliveryKind) {
    for plan in gather(packages, kind) {
        apply(packages, kind, plan);
    }
}

/// The three instructions that wrap `message` into the
/// `(M, Option<ReplyTo>)` envelope a business arm delivers, ending
/// with the write into the arm's payload local.
fn envelope_instructions(
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

fn gather(packages: &[IRPackage], kind: DeliveryKind) -> Vec<ArmPlan> {
    let mut plans = Vec::new();
    for (package_index, package) in packages.iter().enumerate() {
        for function in package.functions.values() {
            for (block_index, block) in function.blocks.iter().enumerate() {
                for (receive_index, instruction) in block.instructions.iter().enumerate() {
                    let IRInstruction::Receive { arms, .. } = instruction else {
                        continue;
                    };
                    let Some(synthesis) = resolve(packages, arms, kind) else {
                        continue;
                    };
                    plans.push(ArmPlan {
                        block_index,
                        function: function.symbol.clone(),
                        package_index,
                        receive_index,
                        synthesis,
                    });
                }
            }
        }
    }
    plans
}

/// Locate the member of a union `message_type` that `is_payload`
/// accepts. Returns that member type and the `UnionWrap` that lifts it
/// back into the union. `None` when `message_type` is not a union or
/// has no such member.
pub(super) fn inject_union_member(
    message_type: &IRType,
    is_payload: fn(&IRType) -> bool,
) -> Option<(IRType, Injection)> {
    let IRType::Union { members, .. } = message_type else {
        return None;
    };
    let member_index = members.iter().position(is_payload)?;
    let injection = Injection::UnionWrap {
        member_index: member_index as u8,
        union_type: message_type.clone(),
    };
    Some((members[member_index].clone(), injection))
}

fn resolve(packages: &[IRPackage], arms: &[ReceiveArm], kind: DeliveryKind) -> Option<Synthesis> {
    let business = resolve_business_envelope(packages, arms, kind.tag())?;
    let (payload_type, injection) = kind.resolve(&business.message_type)?;
    Some(Synthesis {
        business,
        injection,
        payload_type,
    })
}

fn resolve_business_envelope(
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
