//! Dominator-tree analysis over a function's control-flow graph.
//!
//! Implements Cooper, Harvey, and Kennedy's "A Simple, Fast Dominance
//! Algorithm" (2001). Two outputs:
//!
//! - [`compute_immediate_dominators`]: `Map<BlockId, BlockId>` where
//!   `map[b]` is `b`'s immediate dominator. Entry is excluded (it
//!   has no immediate dominator). Unreachable blocks are excluded
//!   too. Callers that need to seal them walk the unreachable set
//!   separately.
//! - [`dominator_tree_children`]: invert the immediate-dominator
//!   map into `Map<parent, Vec<child>>` for top-down traversal of
//!   the dominator tree.
//!
//! These are pure CFG primitives. They consume nothing about
//! instruction operands or value types and only walk terminator
//! targets to derive successor edges. Reusable for any future
//! dataflow pass (DCE, GVN, etc.).

use std::collections::{HashMap, HashSet};

use crate::function::{IRBasicBlock, IRBlockId, IRInstruction, IRTerminator};

/// Walk every reachable block from `entry` and return the immediate
/// dominator of each non-entry reachable block. The map's domain is
/// "every reachable block except `entry`". The entry itself has no
/// immediate dominator and is intentionally absent from the map.
///
/// Unreachable blocks are silently skipped (they receive no
/// immediate-dominator entry). The caller is responsible for
/// surfacing dead-code diagnostics if it cares about reachability.
/// Dominance analysis alone has nothing to say about unreachable
/// code.
pub(crate) fn compute_immediate_dominators(
    blocks: &[IRBasicBlock],
    entry: IRBlockId,
) -> HashMap<IRBlockId, IRBlockId> {
    let postorder = postorder_traversal(blocks, entry);
    let postorder_index: HashMap<IRBlockId, usize> = postorder
        .iter()
        .enumerate()
        .map(|(index, block)| (*block, index))
        .collect();
    let predecessors = predecessor_map(blocks);

    // Sentinel: entry is its own immediate dominator while iterating.
    // Removed before returning so callers see "entry has no
    // immediate dominator" as the absence of a map entry.
    let mut immediate_dominators: HashMap<IRBlockId, IRBlockId> = HashMap::new();
    immediate_dominators.insert(entry, entry);

    let mut changed = true;
    while changed {
        changed = false;
        for &block in postorder.iter().rev() {
            if block == entry {
                continue;
            }
            let known_predecessors: Vec<IRBlockId> = predecessors
                .get(&block)
                .into_iter()
                .flatten()
                .copied()
                .filter(|predecessor| immediate_dominators.contains_key(predecessor))
                .collect();
            if known_predecessors.is_empty() {
                continue;
            }
            let mut new_immediate_dominator = known_predecessors[0];
            for &other in &known_predecessors[1..] {
                new_immediate_dominator = intersect(
                    other,
                    new_immediate_dominator,
                    &immediate_dominators,
                    &postorder_index,
                );
            }
            if immediate_dominators.get(&block) != Some(&new_immediate_dominator) {
                immediate_dominators.insert(block, new_immediate_dominator);
                changed = true;
            }
        }
    }

    immediate_dominators.remove(&entry);
    immediate_dominators
}

/// Whether `dominator` dominates `block`, given the immediate-dominator
/// map and its `entry`. The entry dominates every reachable block.
/// Otherwise walk `block`'s idom chain looking for `dominator`. `entry`
/// is absent from `immediate_dominators` (its sentinel is stripped), so
/// the walk terminates with `false` rather than looping.
pub(crate) fn dominates(
    immediate_dominators: &HashMap<IRBlockId, IRBlockId>,
    entry: IRBlockId,
    dominator: IRBlockId,
    block: IRBlockId,
) -> bool {
    if dominator == entry {
        return true;
    }
    let mut current = block;
    loop {
        if current == dominator {
            return true;
        }
        match immediate_dominators.get(&current) {
            Some(parent) => current = *parent,
            None => return false,
        }
    }
}

/// Invert the immediate-dominator map into a parent-to-children
/// adjacency list for top-down dominator-tree traversal. Children
/// for any given parent are emitted in `blocks` declaration order
/// so traversals are deterministic. The entry block is not a key in
/// `immediate_dominators` itself but is the parent of every block
/// whose immediate dominator is `entry`, so it appears as a key in
/// the returned map iff at least one block has it as its immediate
/// dominator (which is always true for any non-trivial function).
pub(crate) fn dominator_tree_children(
    immediate_dominators: &HashMap<IRBlockId, IRBlockId>,
    blocks: &[IRBasicBlock],
) -> HashMap<IRBlockId, Vec<IRBlockId>> {
    let mut children: HashMap<IRBlockId, Vec<IRBlockId>> = HashMap::new();
    for block in blocks {
        if let Some(parent) = immediate_dominators.get(&block.id) {
            children.entry(*parent).or_default().push(block.id);
        }
    }
    children
}

/// Walk `b1` and `b2` up the partially-built dominator tree until
/// they meet. Implements Cooper-Harvey-Kennedy's "intersect". At
/// each step, the deeper finger (lower postorder index) walks up.
/// Terminates because the entry's sentinel `idom[entry] = entry`
/// breaks the walk before underflowing the tree.
fn intersect(
    b1: IRBlockId,
    b2: IRBlockId,
    immediate_dominators: &HashMap<IRBlockId, IRBlockId>,
    postorder_index: &HashMap<IRBlockId, usize>,
) -> IRBlockId {
    let mut finger1 = b1;
    let mut finger2 = b2;
    while finger1 != finger2 {
        while postorder_index[&finger1] < postorder_index[&finger2] {
            finger1 = immediate_dominators[&finger1];
        }
        while postorder_index[&finger2] < postorder_index[&finger1] {
            finger2 = immediate_dominators[&finger2];
        }
    }
    finger1
}

/// Postorder traversal of the CFG starting at `entry`. Blocks
/// appear in the order their DFS subtree completes (children
/// before their parent), so the entry is the last element. Only
/// reachable blocks appear in the result.
fn postorder_traversal(blocks: &[IRBasicBlock], entry: IRBlockId) -> Vec<IRBlockId> {
    let by_id: HashMap<IRBlockId, &IRBasicBlock> =
        blocks.iter().map(|block| (block.id, block)).collect();
    let mut visited: HashSet<IRBlockId> = HashSet::new();
    let mut order: Vec<IRBlockId> = Vec::new();
    visit(entry, &by_id, &mut visited, &mut order);
    order
}

fn visit(
    block_id: IRBlockId,
    by_id: &HashMap<IRBlockId, &IRBasicBlock>,
    visited: &mut HashSet<IRBlockId>,
    order: &mut Vec<IRBlockId>,
) {
    if !visited.insert(block_id) {
        return;
    }
    let Some(block) = by_id.get(&block_id) else {
        return;
    };
    for successor in successors(block) {
        visit(successor, by_id, visited, order);
    }
    order.push(block_id);
}

/// Successor block ids reachable from `terminator`. `seal/mod.rs` keeps
/// its own `terminator_targets` copy to avoid depending on this module.
fn terminator_successors(terminator: &IRTerminator) -> Vec<IRBlockId> {
    match terminator {
        IRTerminator::Branch(target) => vec![target.block],
        IRTerminator::CondBranch {
            then_target,
            else_target,
            ..
        } => vec![then_target.block, else_target.block],
        IRTerminator::Return { .. } | IRTerminator::TailCall { .. } | IRTerminator::Unreachable => {
            Vec::new()
        }
    }
}

pub(crate) fn successors(block: &IRBasicBlock) -> Vec<IRBlockId> {
    let mut targets = Vec::new();
    for instruction in &block.instructions {
        if let IRInstruction::Receive { after, arms, .. } = instruction {
            targets.extend(arms.iter().map(|arm| arm.body));
            targets.extend(after.iter().map(|after| after.body));
        }
    }
    targets.extend(terminator_successors(&block.terminator));
    targets
}

/// `block -> blocks that branch into it`. Built once per function
/// at dominance-analysis entry and reused inside the iterative
/// fixed-point loop. Unreachable blocks may have no entry in the
/// returned map (no predecessors), and that's fine. The caller
/// filters them out.
fn predecessor_map(blocks: &[IRBasicBlock]) -> HashMap<IRBlockId, Vec<IRBlockId>> {
    let mut predecessors: HashMap<IRBlockId, Vec<IRBlockId>> = HashMap::new();
    for block in blocks {
        for successor in successors(block) {
            predecessors.entry(successor).or_default().push(block.id);
        }
    }
    predecessors
}

#[cfg(test)]
mod tests {
    //! Hand-built CFGs covering the canonical shapes the pipeline
    //! lowering emits today: linear, diamond (`if`/`else`),
    //! chained-test (`cond`/`match`). Every test pins the
    //! immediate-dominator relation block-by-block so future edits
    //! to the algorithm don't silently weaken the contract.

    use super::*;
    use crate::function::{BranchTarget, IRBasicBlock, IRBlockId, IRTerminator};

    fn block(id: u32, terminator: IRTerminator) -> IRBasicBlock {
        IRBasicBlock {
            id: IRBlockId(id),
            label: format!("bb{id}"),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator,
        }
    }

    fn branch(to: u32) -> IRTerminator {
        IRTerminator::Branch(BranchTarget::to(IRBlockId(to)))
    }

    fn cond_branch(cond: u32, then_to: u32, else_to: u32) -> IRTerminator {
        use crate::types::ValueId;
        IRTerminator::CondBranch {
            cond: ValueId(cond),
            then_target: BranchTarget::to(IRBlockId(then_to)),
            else_target: BranchTarget::to(IRBlockId(else_to)),
        }
    }

    fn return_void() -> IRTerminator {
        IRTerminator::Return { value: None }
    }

    #[test]
    fn linear_chain_chains_immediate_dominators() {
        // 0 -> 1 -> 2 -> return
        let blocks = vec![
            block(0, branch(1)),
            block(1, branch(2)),
            block(2, return_void()),
        ];
        let immediate_dominators = compute_immediate_dominators(&blocks, IRBlockId(0));
        assert_eq!(immediate_dominators[&IRBlockId(1)], IRBlockId(0));
        assert_eq!(immediate_dominators[&IRBlockId(2)], IRBlockId(1));
        assert!(!immediate_dominators.contains_key(&IRBlockId(0)));
    }

    #[test]
    fn diamond_pins_idom_of_merge_to_entry() {
        // 0 (cond) -> 1 -> 3
        // 0 (cond) -> 2 -> 3
        let blocks = vec![
            block(0, cond_branch(0, 1, 2)),
            block(1, branch(3)),
            block(2, branch(3)),
            block(3, return_void()),
        ];
        let immediate_dominators = compute_immediate_dominators(&blocks, IRBlockId(0));
        assert_eq!(immediate_dominators[&IRBlockId(1)], IRBlockId(0));
        assert_eq!(immediate_dominators[&IRBlockId(2)], IRBlockId(0));
        assert_eq!(
            immediate_dominators[&IRBlockId(3)],
            IRBlockId(0),
            "merge block's immediate dominator is the diamond's split point",
        );
    }

    #[test]
    fn chained_test_blocks_dominate_through_else_targets() {
        // match-style: 0 (cond) -> body_0
        //              0 (cond) -> 1 (cond)
        //              1 (cond) -> body_1
        //              1 (cond) -> 2 (catch-all)
        //              body_0 -> merge
        //              body_1 -> merge
        //              2      -> merge
        let blocks = vec![
            block(0, cond_branch(0, 10, 1)),
            block(1, cond_branch(1, 11, 2)),
            block(2, branch(12)),
            block(10, branch(99)),
            block(11, branch(99)),
            block(12, branch(99)),
            block(99, return_void()),
        ];
        let immediate_dominators = compute_immediate_dominators(&blocks, IRBlockId(0));
        assert_eq!(immediate_dominators[&IRBlockId(1)], IRBlockId(0));
        assert_eq!(
            immediate_dominators[&IRBlockId(2)],
            IRBlockId(1),
            "second test block's immediate dominator is the first test block",
        );
        assert_eq!(immediate_dominators[&IRBlockId(99)], IRBlockId(0));
    }

    #[test]
    fn unreachable_blocks_are_skipped() {
        // 0 -> 1 (return). Block 2 has no predecessors and is unreachable.
        let blocks = vec![
            block(0, branch(1)),
            block(1, return_void()),
            block(2, return_void()),
        ];
        let immediate_dominators = compute_immediate_dominators(&blocks, IRBlockId(0));
        assert!(
            !immediate_dominators.contains_key(&IRBlockId(2)),
            "unreachable block should not appear in the immediate-dominator map; got {immediate_dominators:?}",
        );
    }

    #[test]
    fn dominator_tree_children_inverts_idom() {
        // 0 (cond) -> 1 -> 3
        // 0 (cond) -> 2 -> 3
        let blocks = vec![
            block(0, cond_branch(0, 1, 2)),
            block(1, branch(3)),
            block(2, branch(3)),
            block(3, return_void()),
        ];
        let immediate_dominators = compute_immediate_dominators(&blocks, IRBlockId(0));
        let children = dominator_tree_children(&immediate_dominators, &blocks);
        let mut entry_children = children[&IRBlockId(0)].clone();
        entry_children.sort();
        assert_eq!(
            entry_children,
            vec![IRBlockId(1), IRBlockId(2), IRBlockId(3)],
            "entry dominates all three downstream blocks directly",
        );
    }
}
