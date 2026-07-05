//! The `elaborate` ExitSignal delivery sub-pass, mirroring
//! [`super::io_ready`]: the runtime delivers a bare
//! `Process.ExitSignal` payload (`TAG_EXIT_SIGNAL = 4`) because the
//! watcher's monomorphized `M` (and its union tag layout) is
//! unknowable runtime-side, so this post-monomorphize pass synthesizes
//! a tag-4 arm that reshapes the bare struct into the business arm's
//! `Pair<M, Option.None>`. Unlike `IOReady`, `M` may also be *exactly*
//! `ExitSignal`, in which case the reshape skips the `UnionWrap`.

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

/// Mangled symbol of the stdlib `Process.ExitSignal` struct
/// (non-generic, so the bare package-qualified name).
const EXIT_SIGNAL_SYMBOL: &str = "Global.Process.ExitSignal";

/// `Option.None` variant name, used to resolve the None tag from the
/// monomorphized decl instead of hard-coding the wire byte.
const OPTION_NONE_VARIANT: &str = "None";

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
    /// The `ExitSignal` struct type and how it injects into `M`.
    exit_signal_type: IRType,
    injection: Injection,
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
                        exit_signal_type: synth.exit_signal_type,
                        function: function.symbol.clone(),
                        injection: synth.injection,
                        none_tag: synth.none_tag,
                        option_symbol: synth.option_symbol,
                        package_index,
                        pair_symbol: synth.pair_symbol,
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
    business_body: IRBlockId,
    business_payload_local: IRLocalId,
    exit_signal_type: IRType,
    injection: Injection,
    none_tag: IRVariantTag,
    option_symbol: IRSymbol,
    pair_symbol: IRSymbol,
}

fn resolve(packages: &[IRPackage], arms: &[ReceiveArm]) -> Option<Synthesis> {
    if arms.iter().any(|arm| arm.tag == ReceiveTag::ExitSignal) {
        return None;
    }
    let business = arms.iter().find(|arm| arm.tag == ReceiveTag::Business)?;
    let IRType::Struct(pair_symbol) = &business.payload_type else {
        return None;
    };
    let [message_field, reply_field] = find_struct(packages, pair_symbol)?.fields.as_slice() else {
        return None;
    };
    let (exit_signal_type, injection) = match &message_field.ir_type {
        message_type if is_exit_signal(message_type) => (message_type.clone(), Injection::Direct),
        IRType::Union { members, .. } => {
            let member_index = members.iter().position(is_exit_signal)?;
            (
                members[member_index].clone(),
                Injection::UnionWrap {
                    member_index: member_index as u8,
                    union_type: message_field.ir_type.clone(),
                },
            )
        }
        _ => return None,
    };
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
        exit_signal_type,
        injection,
        none_tag,
        option_symbol: option_symbol.clone(),
        pair_symbol: pair_symbol.clone(),
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

    let signal_local = next_local_id(function);
    let body_id = next_block_id(function);
    let first_value = next_value_id(function);
    let signal = ValueId(first_value);
    let message = ValueId(first_value + 1);
    let reply_none = ValueId(first_value + 2);
    let envelope = ValueId(first_value + 3);

    // The payload slot must be declared on every path (the backend
    // zero-inits it and untaken arms never write it), so it lives in entry.
    function.blocks[0]
        .instructions
        .push(IRInstruction::LocalDecl {
            local: signal_local,
            ty: plan.exit_signal_type.clone(),
        });

    // Reshape the bare `ExitSignal` into the `Pair<M, Option.None>` the
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
    instructions.extend([
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
                    value: message,
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
    ]);
    function.blocks.push(IRBasicBlock {
        id: body_id,
        instructions,
        label: "receive_exit_signal".to_string(),
        params: Vec::new(),
        terminator: IRTerminator::Branch(BranchTarget::to(plan.business_body)),
    });

    let IRInstruction::Receive { arms, .. } =
        &mut function.blocks[plan.block_index].instructions[plan.receive_index]
    else {
        panic!("exit_signal elaborate: planned receive vanished before apply");
    };
    arms.push(ReceiveArm {
        body: body_id,
        payload_local: signal_local,
        payload_type: plan.exit_signal_type,
        tag: ReceiveTag::ExitSignal,
    });
}
