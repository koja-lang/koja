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
//! For each `Receive` whose business arm takes a `(union, Option<…>)`
//! whose union has an `IOReady` member, it synthesizes a tag-2 arm that
//! reshapes the delivered `IOReady` into the `(M, Option.None)` the
//! business body already consumes, then branches there. Every step reuses
//! an existing `IRInstruction`, so no backend gains a new emission shape.

use crate::function::{
    BranchTarget, IRBasicBlock, IRInstruction, IRSymbol, IRTerminator, ReceiveArm, ReceiveTag,
};
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

use super::delivery::{
    BusinessEnvelope, append_delivery_arm, envelope_instructions, resolve_business_envelope,
};

/// Mangled symbol of the kernel `IO.Ready` enum (`global/src/io.koja`).
/// Non-generic, so its symbol is the bare package-qualified name.
const IO_READY_SYMBOL: &str = "Global.IO.Ready";

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
    block_index: usize,
    business: BusinessEnvelope,
    function: IRSymbol,
    io_ready_type: IRType,
    member_index: u8,
    package_index: usize,
    receive_index: usize,
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
                        business: synth.business,
                        function: function.symbol.clone(),
                        io_ready_type: synth.io_ready_type,
                        member_index: synth.member_index,
                        package_index,
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
    business: BusinessEnvelope,
    io_ready_type: IRType,
    member_index: u8,
    union_type: IRType,
}

fn resolve(packages: &[IRPackage], arms: &[ReceiveArm]) -> Option<Synthesis> {
    let business = resolve_business_envelope(packages, arms, ReceiveTag::IOReady)?;
    let IRType::Union { members, .. } = &business.message_type else {
        return None;
    };
    let member_index = members.iter().position(is_io_ready)?;
    let io_ready_type = members[member_index].clone();
    let union_type = business.message_type.clone();
    Some(Synthesis {
        business,
        io_ready_type,
        member_index: member_index as u8,
        union_type,
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

    let io_local = function.next_local_id();
    let body_id = function.next_block_id();
    let first_value = function.next_value_id();
    let io_ready = ValueId(first_value);
    let widened = ValueId(first_value + 1);
    let reply_none = ValueId(first_value + 2);
    let envelope = ValueId(first_value + 3);

    // Reshape the bare `IOReady` into the `(M, Option.None)` the
    // business body consumes, then branch into that body.
    let mut instructions = vec![
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
    ];
    instructions.extend(envelope_instructions(
        widened,
        reply_none,
        envelope,
        &plan.business,
    ));
    function.blocks.push(IRBasicBlock {
        id: body_id,
        instructions,
        label: "receive_io_ready".to_string(),
        params: Vec::new(),
        terminator: IRTerminator::Branch(BranchTarget::to(plan.business.body)),
    });
    append_delivery_arm(
        function,
        plan.block_index,
        plan.receive_index,
        ReceiveArm {
            body: body_id,
            payload_local: io_local,
            payload_type: plan.io_ready_type,
            tag: ReceiveTag::IOReady,
        },
    );
}
