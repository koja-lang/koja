//! Consume-fusion sub-pass. Rewrites a collection-mutator call whose
//! receiver value dies at the call site into a call to a
//! buffer-consuming twin intrinsic, deleting the death.
//!
//! Deep ownership guarantees no two live values ever share a buffer,
//! so a receiver released right after a `List.append` / `Map.put` /
//! `Set.insert` call would have its buffers freed there anyway. Fusing
//! that release into the call lets the backend take ownership of the
//! buffers and mutate them in place, so `x = x.append(y)` loops become
//! O(1) amortized instead of O(n) per call. The rewrite is
//! release-point-preserving, as it replaces "free the receiver's
//! buffers here" with "reuse them here", so any alias the reuse could
//! break would already be a use-after-free under the copying path.
//!
//! Two lowered shapes fuse, both local to one basic block:
//!
//! - **Owned temp** (fluent chains, `release_call_temps`): a
//!   `drop_value(recv)` follows `r = f(recv, ...)` with no use of
//!   `recv` between. The drop is deleted.
//! - **Slot rebind** (`x = x.append(y)`, see
//!   `lower::body::store_owned_into_local`): the call takes its
//!   receiver from `recv = read x` and is followed by the
//!   reassignment trio `stale = read x`, `drop_value(stale)`,
//!   `write x, r`. The stale read and its drop are deleted, and only
//!   drops of *other* values may sit between the call and the trio
//!   (owned values never alias the receiver's buffers).
//!
//! Any shape that does not match keeps the copying intrinsic. Twins
//! are registered per fused instantiation with the original's
//! signature, a `.$consume$`-suffixed symbol, and
//! [`IRIntrinsicId::Consuming`] for backend dispatch.

use std::collections::BTreeMap;

use crate::function::{FunctionKind, IRBasicBlock, IRFunction, IRInstruction, IRSymbol};
use crate::intrinsic_id::{ConsumingMethod, IRIntrinsicId, ListMethod, MapMethod, SetMethod};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::ValueId;

/// One eligible mutator instantiation, carrying the consuming
/// dispatch id and the twin symbol fused call sites are rewritten to.
struct EligibleMutator {
    method: ConsumingMethod,
    twin: IRSymbol,
}

/// Run the fusion over every function body (and, for scripts, the
/// inline `body`), then register a consuming twin for each mutator
/// instantiation that actually fused.
pub(super) fn fuse_consuming_mutators(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    let eligible = eligible_mutators(packages);
    if eligible.is_empty() {
        return;
    }

    let mut fused: BTreeMap<IRSymbol, ConsumingMethod> = BTreeMap::new();
    let function_blocks = packages
        .iter_mut()
        .flat_map(|package| package.functions.values_mut())
        .flat_map(|function| function.blocks.iter_mut());
    for block in function_blocks.chain(body.iter_mut()) {
        fuse_block(block, &eligible, &mut fused);
    }

    register_twins(packages, &fused);
}

/// The consuming twin for an intrinsic dispatch id, or `None` when
/// the function is not an eligible mutator. This table is the single
/// place a follow-up mutator (e.g. `List.replace_at`) gets added.
fn consuming_method(id: &IRIntrinsicId) -> Option<ConsumingMethod> {
    match id {
        IRIntrinsicId::List(ListMethod::Append) => Some(ConsumingMethod::ListAppend),
        IRIntrinsicId::Map(MapMethod::Put) => Some(ConsumingMethod::MapPut),
        IRIntrinsicId::Set(SetMethod::Insert) => Some(ConsumingMethod::SetInsert),
        _ => None,
    }
}

/// Collect every eligible mutator instantiation, keyed by its mangled
/// symbol for call-site lookup.
fn eligible_mutators(packages: &[IRPackage]) -> BTreeMap<String, EligibleMutator> {
    packages
        .iter()
        .flat_map(|package| package.functions.values())
        .filter_map(|function| {
            let FunctionKind::Intrinsic(id) = &function.kind else {
                return None;
            };
            let method = consuming_method(id)?;
            Some((
                function.symbol.mangled().to_string(),
                EligibleMutator {
                    method,
                    twin: function.symbol.derived(".$consume$"),
                },
            ))
        })
        .collect()
}

/// Where a fused call's receiver dies, in instruction indices
/// relative to the enclosing block.
enum ReceiverDeath {
    /// `drop_value(recv)` at the index. Deleted.
    OwnedTemp { drop_index: usize },
    /// `stale = read x` at the index, immediately followed by
    /// `drop_value(stale)` and `write x, result`. The first two are
    /// deleted.
    SlotRebind { stale_read_index: usize },
}

/// Scan one block for eligible calls whose receiver dies at the call
/// site, rewriting each into its consuming twin and recording the
/// fused originals.
fn fuse_block(
    block: &mut IRBasicBlock,
    eligible: &BTreeMap<String, EligibleMutator>,
    fused: &mut BTreeMap<IRSymbol, ConsumingMethod>,
) {
    let mut call_index = 0;
    while call_index < block.instructions.len() {
        let Some((original, death)) = match_fusion(block, call_index, eligible) else {
            call_index += 1;
            continue;
        };
        let mutator = &eligible[original.mangled()];
        match death {
            ReceiverDeath::OwnedTemp { drop_index } => {
                block.instructions.remove(drop_index);
            }
            ReceiverDeath::SlotRebind { stale_read_index } => {
                block
                    .instructions
                    .drain(stale_read_index..stale_read_index + 2);
            }
        }
        let IRInstruction::Call { callee, .. } = &mut block.instructions[call_index] else {
            unreachable!("consume fusion matched a non-call instruction");
        };
        *callee = mutator.twin.clone();
        fused.insert(original, mutator.method);
        call_index += 1;
    }
}

/// Match one call site against the two fusable shapes. Returns the
/// original callee symbol and the receiver's death, or `None` when
/// the instruction is not an eligible call or the receiver stays
/// live.
fn match_fusion(
    block: &IRBasicBlock,
    call_index: usize,
    eligible: &BTreeMap<String, EligibleMutator>,
) -> Option<(IRSymbol, ReceiverDeath)> {
    let IRInstruction::Call { dest, callee, args } = &block.instructions[call_index] else {
        return None;
    };
    if !eligible.contains_key(callee.mangled()) {
        return None;
    }
    let (&receiver, rest) = args.split_first()?;
    if rest.contains(&receiver) {
        return None;
    }

    let death = owned_temp_death(block, call_index, receiver)
        .or_else(|| slot_rebind_death(block, call_index, receiver, *dest))?;
    Some((callee.clone(), death))
}

/// Match the owned-temp shape, where the first use of `receiver`
/// after the call is its own `drop_value`, in the same block.
fn owned_temp_death(
    block: &IRBasicBlock,
    call_index: usize,
    receiver: ValueId,
) -> Option<ReceiverDeath> {
    for (offset, instruction) in block.instructions[call_index + 1..].iter().enumerate() {
        if let IRInstruction::DropValue { value, .. } = instruction
            && *value == receiver
        {
            return Some(ReceiverDeath::OwnedTemp {
                drop_index: call_index + 1 + offset,
            });
        }
        if instruction.uses_value(receiver) {
            return None;
        }
    }
    None
}

/// Match the slot-rebind shape, where `receiver` was read from a slot
/// that is untouched up to the call, and the call is followed (across
/// other-value drops only) by the stale-read / drop / write trio on
/// the same slot.
fn slot_rebind_death(
    block: &IRBasicBlock,
    call_index: usize,
    receiver: ValueId,
    result: ValueId,
) -> Option<ReceiverDeath> {
    let slot = receiver_slot(block, call_index, receiver)?;
    for (offset, instruction) in block.instructions[call_index + 1..].iter().enumerate() {
        match instruction {
            // Drops of other values release owned storage, which deep
            // ownership guarantees never aliases the receiver's
            // buffers. Anything else could read the consumed value.
            IRInstruction::DropValue { value, .. } if *value != receiver => continue,
            IRInstruction::DropLocal { local, .. } if *local != slot => continue,
            IRInstruction::LocalRead { dest, local, .. } if *local == slot => {
                let stale_read_index = call_index + 1 + offset;
                return rebind_trio_matches(block, stale_read_index, *dest, slot, result)
                    .then_some(ReceiverDeath::SlotRebind { stale_read_index });
            }
            _ => return None,
        }
    }
    None
}

/// The slot `receiver` was read from, provided the slot is untouched
/// and the receiver unused between that read and the call (so the
/// slot still holds the receiver's value at the call).
fn receiver_slot(block: &IRBasicBlock, call_index: usize, receiver: ValueId) -> Option<IRLocalId> {
    let read_index = block.instructions[..call_index]
        .iter()
        .position(|instruction| instruction.dest() == Some(receiver))?;
    let IRInstruction::LocalRead { local, .. } = &block.instructions[read_index] else {
        return None;
    };
    let untouched = block.instructions[read_index + 1..call_index]
        .iter()
        .all(|instruction| !instruction.touches_local(*local) && !instruction.uses_value(receiver));
    untouched.then_some(*local)
}

/// Whether the two instructions after `stale_read_index` complete the
/// reassignment trio. The stale read is already matched by the
/// caller, so this checks for `drop_value(stale)` then
/// `write slot, result`.
fn rebind_trio_matches(
    block: &IRBasicBlock,
    stale_read_index: usize,
    stale: ValueId,
    slot: IRLocalId,
    result: ValueId,
) -> bool {
    let Some([drop, write]) = block
        .instructions
        .get(stale_read_index + 1..stale_read_index + 3)
    else {
        return false;
    };
    matches!(drop, IRInstruction::DropValue { value, .. } if *value == stale)
        && matches!(
            write,
            IRInstruction::LocalWrite { local, value } if *local == slot && *value == result
        )
}

/// Register the consuming twin for each fused original. The twin
/// keeps the original's signature, carries an empty body (backends
/// synthesize it, like every intrinsic), takes a `.$consume$`
/// suffixed symbol, and lives next to the original in its package.
fn register_twins(packages: &mut [IRPackage], fused: &BTreeMap<IRSymbol, ConsumingMethod>) {
    for (original, method) in fused {
        let package = packages
            .iter_mut()
            .find(|package| package.functions.contains_key(original.mangled()))
            .expect("consume fusion: fused callee has an owning package");
        let template = &package.functions[original.mangled()];
        let twin = IRFunction {
            blocks: Vec::new(),
            def_location: None,
            kind: FunctionKind::Intrinsic(IRIntrinsicId::Consuming(*method)),
            params: template.params.clone(),
            return_type: template.return_type.clone(),
            symbol: original.derived(".$consume$"),
        };
        package.functions.insert(twin.symbol.clone(), twin);
    }
}
