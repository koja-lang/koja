//! Consume-fusion sub-pass. Rewrites a buffer-building instruction
//! whose receiver value dies at that instruction into its consuming
//! form, deleting the death.
//!
//! A receiver released right after `r = f(recv, ...)` would have its
//! storage freed there anyway. Fusing that release into the
//! instruction lets the backend take ownership of the storage and
//! extend it in place, so `x = x.append(y)` and `s = s <> piece`
//! loops become O(1) amortized instead of O(n) per step. The rewrite
//! is release-point-preserving, as it replaces "free the receiver's
//! storage here" with "reuse it here", so any alias the reuse could
//! break would already be a use-after-free under the copying path.
//!
//! Two kinds of [`ConsumingSite`] fuse:
//!
//! - **Collection mutators** (`List.append`, `Map.put`, `Set.insert`).
//!   Collection buffers are deep-copied on `Clone`, so the IR death
//!   proof alone is enough and the call is rewritten to a consuming
//!   twin intrinsic that mutates in place unconditionally.
//! - **Byte concat** (`<>` on `String` / `Binary`). Leaf blocks are
//!   rc-shared on `Clone`, so the IR proof only covers the value. The
//!   instruction is flagged `consumes_lhs` and the runtime checks
//!   `rc == 1` before growing in place, otherwise it copies and does
//!   the release the fusion deleted.
//!
//! Three lowered death shapes match, all local to one basic block:
//!
//! - **Owned temp** (fluent chains, `release_call_temps`): a
//!   `drop_value(recv)` follows the site with no use of `recv`
//!   between. The drop is deleted.
//! - **Slot rebind** (`x = x.append(y)`, see
//!   `lower::body::store_owned_into_local`): the site takes its
//!   receiver from `recv = read x` and is followed by the
//!   reassignment trio `stale = read x`, `drop_value(stale)`,
//!   `write x, r`. The stale read and its drop are deleted. Between
//!   the site and the trio nothing may read `recv` or touch `x`.
//!   Further eligible sites there carry the result forward
//!   (`x = x.append(a).append(b)`, `s = s <> a <> b`), and the trio
//!   must write the last carrier, so every link of the chain fuses.
//! - **Slot exit** (`f(n - 1, acc.append(x))` in tail position): the
//!   site takes its receiver from `recv = read x` and the next
//!   instruction touching `x` is its `drop_local x`, with nothing
//!   reading `recv` before it or in the terminator. The slot's
//!   storage dies at that drop, so the drop is deleted and the site
//!   takes it over. The slot then holds a dead pointer that nothing
//!   reads: a tail-call back-edge stores the new arg over it, or the
//!   function returns.
//!
//! Any shape that does not match keeps the copying form. Twins are
//! registered per fused instantiation with the original's signature,
//! a `.$consume$`-suffixed symbol, and [`IRIntrinsicId::Consuming`]
//! for backend dispatch.

use std::collections::BTreeMap;

use crate::function::{FunctionKind, IRBasicBlock, IRFunction, IRInstruction, IRSymbol};
use crate::intrinsic_id::{ConsumingMethod, IRIntrinsicId, ListMethod, MapMethod, SetMethod};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::{ConcatKind, ValueId};

/// One eligible mutator instantiation, carrying the consuming
/// dispatch id and the twin symbol fused call sites are rewritten to.
struct EligibleMutator {
    method: ConsumingMethod,
    twin: IRSymbol,
}

/// Eligible mutator instantiations keyed by mangled callee symbol.
type EligibleMutators = BTreeMap<String, EligibleMutator>;

/// An instruction that may take ownership of its receiver's storage
/// when the receiver dies there.
enum ConsumingSite {
    /// `<>` on `String` / `Binary`. Fusing sets `consumes_lhs`.
    Concat { receiver: ValueId, result: ValueId },
    /// `List.append` / `Map.put` / `Set.insert`. Fusing rewrites the
    /// callee to its consuming twin.
    Mutator {
        original: IRSymbol,
        receiver: ValueId,
        result: ValueId,
    },
}

impl ConsumingSite {
    /// The site at `block.instructions[index]`, or `None` when the
    /// instruction is not one of the fusable shapes.
    fn at(
        block: &IRBasicBlock,
        index: usize,
        eligible: &EligibleMutators,
    ) -> Option<ConsumingSite> {
        match &block.instructions[index] {
            IRInstruction::Call { dest, callee, args } => {
                if !eligible.contains_key(callee.mangled()) {
                    return None;
                }
                let (&receiver, rest) = args.split_first()?;
                if rest.contains(&receiver) {
                    return None;
                }
                Some(ConsumingSite::Mutator {
                    original: callee.clone(),
                    receiver,
                    result: *dest,
                })
            }
            IRInstruction::Concat {
                consumes_lhs: false,
                dest,
                kind: ConcatKind::Binary | ConcatKind::String,
                lhs,
                rhs,
            } if lhs != rhs => Some(ConsumingSite::Concat {
                receiver: *lhs,
                result: *dest,
            }),
            _ => None,
        }
    }

    fn receiver(&self) -> ValueId {
        match self {
            ConsumingSite::Concat { receiver, .. } | ConsumingSite::Mutator { receiver, .. } => {
                *receiver
            }
        }
    }

    fn result(&self) -> ValueId {
        match self {
            ConsumingSite::Concat { result, .. } | ConsumingSite::Mutator { result, .. } => *result,
        }
    }

    /// Rewrite `instruction` (the one this site was matched at) into
    /// its consuming form, recording a fused mutator original.
    fn apply(
        self,
        instruction: &mut IRInstruction,
        eligible: &EligibleMutators,
        fused: &mut BTreeMap<IRSymbol, ConsumingMethod>,
    ) {
        match (self, instruction) {
            (ConsumingSite::Concat { .. }, IRInstruction::Concat { consumes_lhs, .. }) => {
                *consumes_lhs = true;
            }
            (ConsumingSite::Mutator { original, .. }, IRInstruction::Call { callee, .. }) => {
                let mutator = &eligible[original.mangled()];
                *callee = mutator.twin.clone();
                fused.insert(original, mutator.method);
            }
            _ => unreachable!("consume fusion applied a site to a different instruction"),
        }
    }
}

/// Run the fusion over every function body (and, for scripts, the
/// inline `body`), then register a consuming twin for each mutator
/// instantiation that actually fused.
pub(super) fn fuse_consuming_sites(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    let eligible = eligible_mutators(packages);

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
fn eligible_mutators(packages: &[IRPackage]) -> EligibleMutators {
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

/// Where a fused site's receiver dies, in instruction indices
/// relative to the enclosing block.
enum ReceiverDeath {
    /// `drop_value(recv)` at the index. Deleted.
    OwnedTemp { drop_index: usize },
    /// `drop_local x` at the index, where `x` is the slot the receiver
    /// was read from. Deleted.
    SlotExit { drop_index: usize },
    /// `stale = read x` at the index, immediately followed by
    /// `drop_value(stale)` and `write x, result`. The first two are
    /// deleted.
    SlotRebind { stale_read_index: usize },
}

/// Scan one block for eligible sites whose receiver dies there,
/// rewriting each into its consuming form and recording the fused
/// mutator originals.
fn fuse_block(
    block: &mut IRBasicBlock,
    eligible: &EligibleMutators,
    fused: &mut BTreeMap<IRSymbol, ConsumingMethod>,
) {
    let mut index = 0;
    while index < block.instructions.len() {
        let Some((site, death)) = match_fusion(block, index, eligible) else {
            index += 1;
            continue;
        };
        match death {
            ReceiverDeath::OwnedTemp { drop_index } | ReceiverDeath::SlotExit { drop_index } => {
                block.instructions.remove(drop_index);
            }
            ReceiverDeath::SlotRebind { stale_read_index } => {
                block
                    .instructions
                    .drain(stale_read_index..stale_read_index + 2);
            }
        }
        site.apply(&mut block.instructions[index], eligible, fused);
        index += 1;
    }
}

/// Match one instruction against the two fusable shapes. Returns the
/// site and the receiver's death, or `None` when the instruction is
/// not an eligible site or the receiver stays live.
fn match_fusion(
    block: &IRBasicBlock,
    index: usize,
    eligible: &EligibleMutators,
) -> Option<(ConsumingSite, ReceiverDeath)> {
    let site = ConsumingSite::at(block, index, eligible)?;
    let receiver = site.receiver();
    let death = owned_temp_death(block, index, receiver)
        .or_else(|| slot_rebind_death(block, index, receiver, site.result(), eligible))
        .or_else(|| slot_exit_death(block, index, receiver))?;
    Some((site, death))
}

/// Match the owned-temp shape, where the first use of `receiver`
/// after the site is its own `drop_value`, in the same block.
fn owned_temp_death(
    block: &IRBasicBlock,
    site_index: usize,
    receiver: ValueId,
) -> Option<ReceiverDeath> {
    for (offset, instruction) in block.instructions[site_index + 1..].iter().enumerate() {
        if let IRInstruction::DropValue { value, .. } = instruction
            && *value == receiver
        {
            return Some(ReceiverDeath::OwnedTemp {
                drop_index: site_index + 1 + offset,
            });
        }
        if instruction.uses_value(receiver) {
            return None;
        }
    }
    None
}

/// Match the slot-rebind shape, where `receiver` was read from a slot
/// that is untouched up to the site, and the site is followed by the
/// stale-read / drop / write trio on the same slot. Between the two,
/// nothing may read `receiver` or touch the slot, since both name the
/// storage being consumed. Everything else is allowed, including
/// further eligible sites that carry the result forward. The trio
/// must write the last carrier, so a chain like `x = x.append(a).append(b)`
/// rebinds the slot in one step and every link consumes.
fn slot_rebind_death(
    block: &IRBasicBlock,
    site_index: usize,
    receiver: ValueId,
    result: ValueId,
    eligible: &EligibleMutators,
) -> Option<ReceiverDeath> {
    let slot = receiver_slot(block, site_index, receiver)?;
    let mut carrier = result;
    for index in site_index + 1..block.instructions.len() {
        let instruction = &block.instructions[index];
        if instruction.uses_value(receiver) {
            return None;
        }
        if let IRInstruction::LocalRead { dest, local, .. } = instruction
            && *local == slot
        {
            return rebind_trio_matches(block, index, *dest, slot, carrier).then_some(
                ReceiverDeath::SlotRebind {
                    stale_read_index: index,
                },
            );
        }
        if instruction.touches_local(slot) {
            return None;
        }
        if let Some(link) = ConsumingSite::at(block, index, eligible)
            && link.receiver() == carrier
        {
            carrier = link.result();
        }
    }
    None
}

/// Match the slot-exit shape, where `receiver` was read from a slot
/// that is untouched up to the site, and the first instruction after
/// the site to touch the slot is its `drop_local`. Nothing may read
/// `receiver` before that drop or in the terminator, since the fused
/// site takes over the storage the drop would have released.
fn slot_exit_death(
    block: &IRBasicBlock,
    site_index: usize,
    receiver: ValueId,
) -> Option<ReceiverDeath> {
    let slot = receiver_slot(block, site_index, receiver)?;
    if block.terminator.uses_value(receiver) {
        return None;
    }
    for (offset, instruction) in block.instructions[site_index + 1..].iter().enumerate() {
        if instruction.uses_value(receiver) {
            return None;
        }
        if let IRInstruction::DropLocal { local, .. } = instruction
            && *local == slot
        {
            return Some(ReceiverDeath::SlotExit {
                drop_index: site_index + 1 + offset,
            });
        }
        if instruction.touches_local(slot) {
            return None;
        }
    }
    None
}

/// The slot `receiver` was read from, provided the slot is untouched
/// and the receiver unused between that read and the site (so the
/// slot still holds the receiver's value at the site).
fn receiver_slot(block: &IRBasicBlock, site_index: usize, receiver: ValueId) -> Option<IRLocalId> {
    let read_index = block.instructions[..site_index]
        .iter()
        .position(|instruction| instruction.dest() == Some(receiver))?;
    let IRInstruction::LocalRead { local, .. } = &block.instructions[read_index] else {
        return None;
    };
    let untouched = block.instructions[read_index + 1..site_index]
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
