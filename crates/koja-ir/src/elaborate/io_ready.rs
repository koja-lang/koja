//! The `elaborate` I/O delivery sub-pass.
//!
//! The reactor already sends readiness events (`TAG_IO_READY = 2`, a bare
//! `IOReady{variant, fd}` payload), but the receive side only dispatched
//! `Business` (0) and `Lifecycle` (1) arms, so a tag-2 envelope hit the
//! `unreachable` trap. Source lowering can't close the gap, because the
//! default `Process.run` is generic over `M`, so we can't tell whether `M` is a
//! union containing `IOReady` until monomorphization. This pass runs in
//! `elaborate` (post-monomorphize), where `M` is concrete.
//!
//! For each `Receive` whose business arm takes a `Pair<union, Option<…>>`
//! whose union has an `IOReady` member, it synthesizes a tag-2 arm that
//! reshapes the delivered `IOReady` into the `Pair<M, Option.None>` the
//! business body already consumes, then branches there. Every step reuses
//! an existing `IRInstruction`, so no backend gains a new emission shape.

use crate::enum_decl::{EnumPayloadInit, IRVariantTag};
use crate::function::{
    BranchTarget, IRBasicBlock, IRBlockId, IRInstruction, IRSymbol, IRTerminator, ReceiveArm,
    ReceiveTag,
};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::struct_decl::StructFieldInit;
use crate::types::{IRType, ValueId};

use super::{find_enum, find_struct, next_block_id, next_local_id, next_value_id};

/// Mangled symbol of the kernel `IO.Ready` enum (`global/src/io.koja`).
/// Non-generic, so its symbol is the bare package-qualified name.
const IO_READY_SYMBOL: &str = "Global.IO.Ready";

/// `Option.None` variant name. Resolves the None tag from the
/// monomorphized decl instead of hard-coding the wire byte.
const OPTION_NONE_VARIANT: &str = "None";

/// Synthesize a tag-2 receive arm into every process loop whose business
/// message union contains `IOReady`. Idempotent, so a `Receive` that
/// already has an `IOReady` arm is left untouched.
pub(crate) fn deliver_io_ready(packages: &mut [IRPackage]) {
    for plan in gather(packages) {
        apply(packages, plan);
    }
}

/// A located, fully-resolved synthesis request. Gathered under a shared
/// borrow (decls live in the same package set we later mutate), then
/// applied, so every field is owned, with no borrow into `packages`.
struct ArmPlan {
    /// `Receive` location: package, function, block, instruction.
    block_index: usize,
    function: IRSymbol,
    package_index: usize,
    receive_index: usize,
    /// Business arm we funnel into and the `Pair` it expects.
    business_body: IRBlockId,
    business_payload_local: IRLocalId,
    none_tag: IRVariantTag,
    option_symbol: IRSymbol,
    pair_symbol: IRSymbol,
    /// `IOReady` enum type / its index in the union `M`.
    io_ready_type: IRType,
    member_index: u8,
    union_type: IRType,
}

fn gather(packages: &[IRPackage]) -> Vec<ArmPlan> {
    let mut plans = Vec::new();
    for (package_index, package) in packages.iter().enumerate() {
        for function in package.functions.values() {
            for (block_index, block) in function.blocks.iter().enumerate() {
                for (receive_index, instruction) in block.instructions.iter().enumerate() {
                    let IRInstruction::Receive { arms, .. } = instruction else {
                        continue;
                    };
                    let Some(synth) = resolve(packages, arms) else {
                        continue;
                    };
                    plans.push(ArmPlan {
                        block_index,
                        business_body: synth.business_body,
                        business_payload_local: synth.business_payload_local,
                        function: function.symbol.clone(),
                        io_ready_type: synth.io_ready_type,
                        member_index: synth.member_index,
                        none_tag: synth.none_tag,
                        option_symbol: synth.option_symbol,
                        package_index,
                        pair_symbol: synth.pair_symbol,
                        receive_index,
                        union_type: synth.union_type,
                    });
                }
            }
        }
    }
    plans
}

/// The synthesis inputs for one `Receive`, or `None` when it is not a
/// union-message business loop (or already has an `IOReady` arm).
struct Synthesis {
    business_body: IRBlockId,
    business_payload_local: IRLocalId,
    io_ready_type: IRType,
    member_index: u8,
    none_tag: IRVariantTag,
    option_symbol: IRSymbol,
    pair_symbol: IRSymbol,
    union_type: IRType,
}

fn resolve(packages: &[IRPackage], arms: &[ReceiveArm]) -> Option<Synthesis> {
    if arms.iter().any(|arm| arm.tag == ReceiveTag::IOReady) {
        return None;
    }
    let business = arms.iter().find(|arm| arm.tag == ReceiveTag::Business)?;
    let IRType::Struct(pair_symbol) = &business.payload_type else {
        return None;
    };
    let [message_field, reply_field] = find_struct(packages, pair_symbol)?.fields.as_slice() else {
        return None;
    };
    let IRType::Union { members, .. } = &message_field.ir_type else {
        return None;
    };
    let member_index = members.iter().position(is_io_ready)?;
    let IRType::Enum(option_symbol) = &reply_field.ir_type else {
        return None;
    };
    let none_tag = find_enum(packages, option_symbol)?
        .variants
        .iter()
        .find(|variant| variant.name == OPTION_NONE_VARIANT)?
        .tag;
    Some(Synthesis {
        business_body: business.body,
        business_payload_local: business.payload_local,
        io_ready_type: members[member_index].clone(),
        member_index: member_index as u8,
        none_tag,
        option_symbol: option_symbol.clone(),
        pair_symbol: pair_symbol.clone(),
        union_type: message_field.ir_type.clone(),
    })
}

fn is_io_ready(member: &IRType) -> bool {
    matches!(member, IRType::Enum(symbol) if symbol.mangled() == IO_READY_SYMBOL)
}

/// Splice the synthesized arm into one located `Receive`: declare the
/// `IOReady` payload slot, build the reshape body block, and append the
/// tag-2 arm. Fresh ids are minted one past the function's current max,
/// so applying multiple plans to the same function stays collision-free.
fn apply(packages: &mut [IRPackage], plan: ArmPlan) {
    let function = packages[plan.package_index]
        .functions
        .get_mut(plan.function.mangled())
        .expect("io_ready elaborate: planned function vanished before apply");

    let io_local = next_local_id(function);
    let body_id = next_block_id(function);
    let first_value = next_value_id(function);
    let io_ready = ValueId(first_value);
    let widened = ValueId(first_value + 1);
    let reply_none = ValueId(first_value + 2);
    let envelope = ValueId(first_value + 3);

    // The payload slot must be declared on every path (the backend
    // zero-inits it and untaken arms never write it), so it lives in entry.
    function.blocks[0]
        .instructions
        .push(IRInstruction::LocalDecl {
            local: io_local,
            ty: plan.io_ready_type.clone(),
        });

    // Reshape the bare `IOReady` into the `Pair<M, Option.None>` the
    // business body consumes, then branch into that body.
    function.blocks.push(IRBasicBlock {
        id: body_id,
        instructions: vec![
            IRInstruction::LocalRead {
                dest: io_ready,
                local: io_local,
                ty: plan.io_ready_type.clone(),
            },
            IRInstruction::UnionWrap {
                dest: widened,
                member_index: plan.member_index,
                member_type: plan.io_ready_type.clone(),
                ty: plan.union_type,
                value: io_ready,
            },
            IRInstruction::EnumConstruct {
                dest: reply_none,
                payload: EnumPayloadInit::Unit,
                tag: plan.none_tag,
                ty: plan.option_symbol,
            },
            IRInstruction::StructInit {
                dest: envelope,
                fields: vec![
                    StructFieldInit {
                        index: 0,
                        value: widened,
                    },
                    StructFieldInit {
                        index: 1,
                        value: reply_none,
                    },
                ],
                ty: plan.pair_symbol,
            },
            IRInstruction::LocalWrite {
                local: plan.business_payload_local,
                value: envelope,
            },
        ],
        label: "receive_io_ready".to_string(),
        params: Vec::new(),
        terminator: IRTerminator::Branch(BranchTarget::to(plan.business_body)),
    });

    let IRInstruction::Receive { arms, .. } =
        &mut function.blocks[plan.block_index].instructions[plan.receive_index]
    else {
        panic!("io_ready elaborate: planned receive vanished before apply");
    };
    arms.push(ReceiveArm {
        body: body_id,
        payload_local: io_local,
        payload_type: plan.io_ready_type,
        tag: ReceiveTag::IOReady,
    });
}
