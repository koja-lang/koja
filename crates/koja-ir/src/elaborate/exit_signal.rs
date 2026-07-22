//! The `elaborate` ExitSignal delivery sub-pass, mirroring
//! [`super::io_ready`]: the runtime delivers a bare
//! `Process.ExitSignal` payload (`TAG_EXIT_SIGNAL = 4`) because the
//! watcher's monomorphized `M` (and its union tag layout) is
//! unknowable runtime-side, so this post-monomorphize pass synthesizes
//! a tag-4 arm that reshapes the bare struct into the business arm's
//! `(M, Option.None)`. Unlike `IOReady`, `M` may also be *exactly*
//! `ExitSignal`, in which case the reshape skips the `UnionWrap`.

use crate::function::{
    BranchTarget, IRBasicBlock, IRInstruction, IRSymbol, IRTerminator, ReceiveArm, ReceiveTag,
};
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

use super::delivery::{
    BusinessEnvelope, append_delivery_arm, envelope_instructions, resolve_business_envelope,
};

/// Mangled symbol of the stdlib `Process.ExitSignal` struct
/// (non-generic, so the bare package-qualified name).
const EXIT_SIGNAL_SYMBOL: &str = "Global.Process.ExitSignal";

/// Synthesize a tag-4 receive arm into every process loop whose business
/// message type includes `Process.ExitSignal`. Idempotent, so a `Receive`
/// that already has an `ExitSignal` arm is left untouched.
pub(crate) fn deliver_exit_signal(packages: &mut [IRPackage]) {
    for plan in gather(packages) {
        apply(packages, plan);
    }
}

/// How the bare `ExitSignal` becomes the business arm's message `M`:
/// wrapped into `M`'s union at `member_index`, or used directly when
/// `M` *is* `ExitSignal`.
enum Injection {
    Direct,
    UnionWrap {
        member_index: u8,
        union_type: IRType,
    },
}

/// A located, fully-resolved synthesis request. Gathered under a shared
/// borrow (decls live in the same package set we later mutate), then
/// applied, so every field is owned with no borrow into `packages`.
struct ArmPlan {
    block_index: usize,
    business: BusinessEnvelope,
    exit_signal_type: IRType,
    function: IRSymbol,
    injection: Injection,
    package_index: usize,
    receive_index: usize,
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
                        exit_signal_type: synth.exit_signal_type,
                        function: function.symbol.clone(),
                        injection: synth.injection,
                        package_index,
                        receive_index,
                    });
                }
            }
        }
    }
    plans
}

/// The synthesis inputs for one `Receive`, or `None` when its business
/// message doesn't include `ExitSignal` (or it already has a tag-4 arm).
struct Synthesis {
    business: BusinessEnvelope,
    exit_signal_type: IRType,
    injection: Injection,
}

fn resolve(packages: &[IRPackage], arms: &[ReceiveArm]) -> Option<Synthesis> {
    let business = resolve_business_envelope(packages, arms, ReceiveTag::ExitSignal)?;
    let (exit_signal_type, injection) = match &business.message_type {
        message_type if is_exit_signal(message_type) => (message_type.clone(), Injection::Direct),
        IRType::Union { members, .. } => {
            let member_index = members.iter().position(is_exit_signal)?;
            (
                members[member_index].clone(),
                Injection::UnionWrap {
                    member_index: member_index as u8,
                    union_type: business.message_type.clone(),
                },
            )
        }
        _ => return None,
    };
    Some(Synthesis {
        business,
        exit_signal_type,
        injection,
    })
}

fn is_exit_signal(member: &IRType) -> bool {
    matches!(member, IRType::Struct(symbol) if symbol.mangled() == EXIT_SIGNAL_SYMBOL)
}

/// Splice the synthesized arm into one located `Receive`: declare the
/// `ExitSignal` payload slot, build the reshape body block, and append
/// the tag-4 arm. Fresh ids are minted one past the function's current
/// max, so applying multiple plans to the same function stays
/// collision-free.
fn apply(packages: &mut [IRPackage], plan: ArmPlan) {
    let function = packages[plan.package_index]
        .functions
        .get_mut(plan.function.mangled())
        .expect("exit_signal elaborate: planned function vanished before apply");

    let signal_local = function.next_local_id();
    let body_id = function.next_block_id();
    let first_value = function.next_value_id();
    let signal = ValueId(first_value);
    let message = ValueId(first_value + 1);
    let reply_none = ValueId(first_value + 2);
    let envelope = ValueId(first_value + 3);

    // Reshape the bare `ExitSignal` into the `(M, Option.None)` the
    // business body consumes, then branch into that body.
    let mut instructions = vec![IRInstruction::LocalRead {
        dest: signal,
        local: signal_local,
        ty: plan.exit_signal_type.clone(),
    }];
    let message = match plan.injection {
        Injection::Direct => signal,
        Injection::UnionWrap {
            member_index,
            union_type,
        } => {
            instructions.push(IRInstruction::UnionWrap {
                dest: message,
                member_index,
                member_type: plan.exit_signal_type.clone(),
                ty: union_type,
                value: signal,
            });
            message
        }
    };
    instructions.extend(envelope_instructions(
        message,
        reply_none,
        envelope,
        &plan.business,
    ));
    function.blocks.push(IRBasicBlock {
        id: body_id,
        instructions,
        label: "receive_exit_signal".to_string(),
        params: Vec::new(),
        terminator: IRTerminator::Branch(BranchTarget::to(plan.business.body)),
    });
    append_delivery_arm(
        function,
        plan.block_index,
        plan.receive_index,
        ReceiveArm {
            body: body_id,
            payload_local: signal_local,
            payload_type: plan.exit_signal_type,
            tag: ReceiveTag::ExitSignal,
        },
    );
}
