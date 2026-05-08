//! Per-function and per-block invariants. [`seal_package`] /
//! [`seal_block`] / [`seal_ssa`] are reused for both function- and
//! script-shaped IR; the only difference is the seeded `ValueId` set
//! (function params for fns, empty for scripts).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::dominators::{compute_immediate_dominators, dominator_tree_children};
use crate::function::{
    BranchTarget, FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRTerminator,
};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

use super::enums::seal_enum_decls;
use super::structs::seal_struct_decls;
use super::{
    instruction_operands, require_supported_const, require_supported_type, seal_panic,
    terminator_operands, terminator_targets,
};

pub(super) fn seal_package(pkg: &IRPackage) {
    seal_struct_decls(pkg);
    seal_enum_decls(pkg);
    for (sym, function) in &pkg.functions {
        if sym != &function.symbol {
            seal_panic(&format!(
                "package `{}` keys function at `{sym}` but the function's own symbol is `{}`",
                pkg.package, function.symbol,
            ));
        }
        seal_function(function);
    }
}

fn seal_function(function: &IRFunction) {
    let owner = format!("function `{}`", function.symbol);
    match &function.kind {
        FunctionKind::Intrinsic => {
            if !function.blocks.is_empty() {
                seal_panic(&format!(
                    "{owner} is `@intrinsic` but carries {} basic block(s); intrinsic bodies \
                     are synthesized at emit time and must lower to empty `blocks`",
                    function.blocks.len(),
                ));
            }
        }
        FunctionKind::Regular => {
            if function.blocks.is_empty() {
                seal_panic(&format!("{owner} has no basic blocks"));
            }
        }
        FunctionKind::Extern(_) => {
            if !function.blocks.is_empty() {
                seal_panic(&format!(
                    "{owner} is `@extern \"C\"` but carries {} basic block(s); FFI \
                     declarations have no body and must lower to empty `blocks`",
                    function.blocks.len(),
                ));
            }
        }
    }
    require_supported_type(&function.return_type, &|| format!("{owner} return type"));
    // Function parameter `ValueId`s seed the entry block's
    // dominator-tree-rooted defined set: params are visible to
    // every block the entry dominates, which in a well-formed CFG
    // is every reachable block.
    let mut parameter_value_ids: HashSet<ValueId> = HashSet::new();
    for (index, param) in function.params.iter().enumerate() {
        require_supported_type(&param.ty, &|| {
            format!("{owner} parameter #{index} ({}) type", param.id)
        });
        if !parameter_value_ids.insert(param.id) {
            seal_panic(&format!(
                "{owner} lists duplicate parameter value `{}`",
                param.id,
            ));
        }
    }
    if function.blocks.is_empty() {
        return;
    }
    let block_ids = collect_block_ids(&function.blocks, &owner);
    let block_params = collect_block_params(&function.blocks, &owner);
    for block in &function.blocks {
        seal_block(block, &owner, &block_ids, &block_params);
    }
    seal_ssa(&function.blocks, &owner, &parameter_value_ids);
    seal_locals(function, &owner);
}

/// Per-function index of every block's [`crate::function::BlockParam`]
/// signature, keyed by [`IRBlockId`]. Used by [`seal_block`] to
/// validate that each [`BranchTarget`]'s `args` list matches the
/// target block's declared param signature in count (and, where the
/// per-block walk has captured both the param's and the arg's type,
/// in type as well — see [`require_branch_target_well_formed`]).
///
/// Built once per function so the per-block walk doesn't repeat the
/// scan.
fn collect_block_params(blocks: &[IRBasicBlock], owner: &str) -> BTreeMap<IRBlockId, Vec<IRType>> {
    let mut by_block: BTreeMap<IRBlockId, Vec<IRType>> = BTreeMap::new();
    for block in blocks {
        for (index, param) in block.params.iter().enumerate() {
            require_supported_type(&param.ty, &|| {
                format!(
                    "{owner} block {} param #{index} ({}) type",
                    block.id, param.dest
                )
            });
        }
        by_block.insert(
            block.id,
            block.params.iter().map(|p| p.ty.clone()).collect(),
        );
    }
    by_block
}

/// Per-function local-slot invariants: each [`IRLocalId`] is
/// `LocalDecl`'d exactly once, every read/write references a declared
/// id, and every param's `local_id` lands in the declared set
/// (param promotion emits the matching `LocalDecl`).
fn seal_locals(function: &IRFunction, owner: &str) {
    let mut declared: HashSet<IRLocalId> = HashSet::new();
    for block in &function.blocks {
        for inst in &block.instructions {
            if let IRInstruction::LocalDecl { local, .. } = inst
                && !declared.insert(*local)
            {
                seal_panic(&format!(
                    "{owner} declares local slot `{local}` more than once",
                ));
            }
        }
    }
    // Intrinsics and `@extern "C"` decls carry params for backend
    // signature shape but emit no body, so they have no matching
    // `LocalDecl`s to check.
    if matches!(function.kind, FunctionKind::Regular) {
        for param in &function.params {
            if !declared.contains(&param.local_id) {
                seal_panic(&format!(
                    "{owner} parameter slot `{}` was never `LocalDecl`'d",
                    param.local_id,
                ));
            }
        }
    }
    for block in &function.blocks {
        for inst in &block.instructions {
            match inst {
                IRInstruction::LocalRead { local, .. }
                | IRInstruction::LocalWrite { local, .. }
                    if !declared.contains(local) =>
                {
                    seal_panic(&format!(
                        "{owner} references undeclared local slot `{local}`",
                    ));
                }
                _ => {}
            }
        }
    }
}

/// Block-id set for terminator-target validation. Asserts uniqueness.
pub(super) fn collect_block_ids(blocks: &[IRBasicBlock], owner: &str) -> HashSet<IRBlockId> {
    let mut ids = HashSet::with_capacity(blocks.len());
    for block in blocks {
        if !ids.insert(block.id) {
            seal_panic(&format!(
                "{owner} contains duplicate block id `{}`",
                block.id,
            ));
        }
    }
    ids
}

/// Block-local structural invariants: every const value sits in the
/// supported-width set, every terminator target names an existing
/// block, and every branch arg list matches its target's block-param
/// arity. The cross-block SSA defined-before-use check lives in
/// [`seal_ssa`] (function-level, dominator-tree walk).
pub(super) fn seal_block(
    block: &IRBasicBlock,
    owner: &str,
    block_ids: &HashSet<IRBlockId>,
    block_params: &BTreeMap<IRBlockId, Vec<IRType>>,
) {
    for instruction in &block.instructions {
        if let IRInstruction::Const { value, dest } = instruction {
            require_supported_const(value, &|| format!("{owner} const instruction at {dest}"));
        }
    }
    for target in terminator_targets(&block.terminator) {
        if !block_ids.contains(&target) {
            seal_panic(&format!(
                "{owner} block {} terminator targets unknown block `{target}`",
                block.id,
            ));
        }
    }
    seal_branch_target_arities(&block.terminator, block.id, owner, block_params);
}

/// Walk every reachable block in `blocks` over the dominator tree
/// rooted at the entry block (`blocks[0]` by lowering convention)
/// and assert SSA defined-before-use plus single-def. The
/// `defined` set carries every `ValueId` available at the current
/// block: function parameters seed it at entry, block params join
/// it on entry to their declaring block, instruction `dest`s join
/// it as the walk advances, and every value pops back out when the
/// dominator-subtree walk returns. An operand is in scope iff its
/// def lives in some dominator of the using block — exactly what
/// dominance-based SSA admits.
pub(super) fn seal_ssa(
    blocks: &[IRBasicBlock],
    owner: &str,
    parameter_value_ids: &HashSet<ValueId>,
) {
    let entry = blocks[0].id;
    let immediate_dominators = compute_immediate_dominators(blocks, entry);
    let children = dominator_tree_children(&immediate_dominators, blocks);
    let blocks_by_id: HashMap<IRBlockId, &IRBasicBlock> =
        blocks.iter().map(|block| (block.id, block)).collect();

    let mut defined = parameter_value_ids.clone();
    walk_dominator_subtree(entry, owner, &children, &blocks_by_id, &mut defined);
}

/// Recursive descent over the dominator tree. The mutable `defined`
/// set is the live "in scope at this point in the walk" set; values
/// added on entry are removed on exit so siblings see their parent's
/// scope but not each other's.
fn walk_dominator_subtree(
    block_id: IRBlockId,
    owner: &str,
    children: &HashMap<IRBlockId, Vec<IRBlockId>>,
    blocks_by_id: &HashMap<IRBlockId, &IRBasicBlock>,
    defined: &mut HashSet<ValueId>,
) {
    let block = blocks_by_id[&block_id];
    let mut introduced = Vec::new();

    for param in &block.params {
        if !defined.insert(param.dest) {
            seal_panic(&format!(
                "{owner} block {block_id} declares block param `{}` that shadows an already-defined value",
                param.dest,
            ));
        }
        introduced.push(param.dest);
    }
    for instruction in &block.instructions {
        for operand in instruction_operands(instruction) {
            require_in_scope(operand, owner, block_id, defined);
        }
        if let Some(dest) = instruction.dest() {
            if !defined.insert(dest) {
                seal_panic(&format!("{owner} redefines value `{dest}`"));
            }
            introduced.push(dest);
        }
    }
    for operand in terminator_operands(&block.terminator) {
        require_in_scope(operand, owner, block_id, defined);
    }

    if let Some(child_blocks) = children.get(&block_id) {
        for &child in child_blocks {
            walk_dominator_subtree(child, owner, children, blocks_by_id, defined);
        }
    }

    for value in introduced {
        defined.remove(&value);
    }
}

/// Operand-in-scope assertion under dominance. Failure surfaces as
/// "no dominating block defines `…`" rather than the stricter
/// "before it is defined" (which only fit the per-block model). The
/// underlying invariant is the same: a value's def must dominate
/// every use site.
fn require_in_scope(value: ValueId, owner: &str, block_id: IRBlockId, defined: &HashSet<ValueId>) {
    if !defined.contains(&value) {
        seal_panic(&format!(
            "{owner} block {block_id} references value `{value}` whose definition does not dominate this use",
        ));
    }
}

/// Validate that every [`BranchTarget`] in `term` passes exactly as
/// many `args` as the target block declares [`crate::function::BlockParam`]s.
/// Type-matching of args against params requires a global value-type
/// index that this seal walk doesn't yet build; the count check is
/// the strict invariant (an arity mismatch always indicates a
/// lowering bug). Type validation happens at the LLVM-emission
/// boundary via inkwell's `add_incoming` type check.
fn seal_branch_target_arities(
    term: &IRTerminator,
    pred: IRBlockId,
    owner: &str,
    block_params: &BTreeMap<IRBlockId, Vec<IRType>>,
) {
    match term {
        IRTerminator::Branch(target) => {
            require_branch_target_arity(target, pred, owner, block_params);
        }
        IRTerminator::CondBranch {
            else_target,
            then_target,
            ..
        } => {
            require_branch_target_arity(then_target, pred, owner, block_params);
            require_branch_target_arity(else_target, pred, owner, block_params);
        }
        IRTerminator::Return { .. } => {}
    }
}

fn require_branch_target_arity(
    target: &BranchTarget,
    pred: IRBlockId,
    owner: &str,
    block_params: &BTreeMap<IRBlockId, Vec<IRType>>,
) {
    let Some(params) = block_params.get(&target.block) else {
        // Unknown target id was already reported by `terminator_targets`
        // / `block_ids` walk; skip the arity check rather than panic
        // twice for the same root cause.
        return;
    };
    if target.args.len() != params.len() {
        seal_panic(&format!(
            "{owner} branch from {pred} to {} passes {} arg{} but target declares {} param{}",
            target.block,
            target.args.len(),
            if target.args.len() == 1 { "" } else { "s" },
            params.len(),
            if params.len() == 1 { "" } else { "s" },
        ));
    }
}

#[cfg(test)]
mod block_param_tests {
    //! Hand-built CFG fragments exercising the block-parameter and
    //! [`BranchTarget`] arg/param invariants on `seal_block`. The
    //! happy path runs `seal_block` and expects no panic; mismatch
    //! cases pin the specific seal-violation message so future edits
    //! don't accidentally weaken the contract.

    use super::*;
    use crate::function::BlockParam;
    use crate::types::ConstValue;

    /// Build a 2-block CFG: entry emits a `Const::Int64(42)` and
    /// branches to merge with `args: [const_id]`; merge declares
    /// one `Int64` BlockParam and returns it. Returns the function
    /// shape and a label for use in seal panics.
    fn entry_branches_to_merge(merge_args: Vec<ValueId>) -> (Vec<IRBasicBlock>, String) {
        let entry_id = IRBlockId(0);
        let merge_id = IRBlockId(1);
        let const_id = ValueId(0);
        let merge_param = ValueId(1);
        let entry = IRBasicBlock {
            id: entry_id,
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![IRInstruction::Const {
                dest: const_id,
                value: ConstValue::Int64(42),
            }],
            terminator: IRTerminator::Branch(BranchTarget::with_args(merge_id, merge_args)),
        };
        let merge = IRBasicBlock {
            id: merge_id,
            label: "merge".to_string(),
            params: vec![BlockParam {
                dest: merge_param,
                ty: IRType::Int64,
            }],
            instructions: Vec::new(),
            terminator: IRTerminator::Return {
                value: Some(merge_param),
            },
        };
        (vec![entry, merge], "test fn".to_string())
    }

    fn run_seal(blocks: &[IRBasicBlock], owner: &str) {
        let block_ids = collect_block_ids(blocks, owner);
        let block_params = collect_block_params(blocks, owner);
        for block in blocks {
            seal_block(block, owner, &block_ids, &block_params);
        }
        let parameter_value_ids: HashSet<ValueId> = HashSet::new();
        seal_ssa(blocks, owner, &parameter_value_ids);
    }

    #[test]
    fn merge_with_matching_arg_count_passes() {
        let (blocks, owner) = entry_branches_to_merge(vec![ValueId(0)]);
        run_seal(&blocks, &owner);
    }

    #[test]
    #[should_panic(expected = "passes 0 args but target declares 1 param")]
    fn merge_with_too_few_args_panics() {
        let (blocks, owner) = entry_branches_to_merge(Vec::new());
        run_seal(&blocks, &owner);
    }

    #[test]
    #[should_panic(expected = "passes 2 args but target declares 1 param")]
    fn merge_with_too_many_args_panics() {
        let (blocks, owner) = entry_branches_to_merge(vec![ValueId(0), ValueId(0)]);
        run_seal(&blocks, &owner);
    }

    #[test]
    fn block_param_is_visible_to_block_body_operand_check() {
        // Merge declares a BlockParam, then the merge body reads it
        // via a UnaryOp. The seed-the-defined-set-with-block-params
        // step is what makes this well-formed; without it the
        // unary's operand check would panic.
        let entry_id = IRBlockId(0);
        let merge_id = IRBlockId(1);
        let const_id = ValueId(0);
        let merge_param = ValueId(1);
        let unary_dest = ValueId(2);
        let entry = IRBasicBlock {
            id: entry_id,
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![IRInstruction::Const {
                dest: const_id,
                value: ConstValue::Int64(7),
            }],
            terminator: IRTerminator::Branch(BranchTarget::with_args(merge_id, vec![const_id])),
        };
        let merge = IRBasicBlock {
            id: merge_id,
            label: "merge".to_string(),
            params: vec![BlockParam {
                dest: merge_param,
                ty: IRType::Int64,
            }],
            instructions: vec![IRInstruction::UnaryOp {
                dest: unary_dest,
                op: crate::types::IRUnaryOp::Neg,
                operand: merge_param,
            }],
            terminator: IRTerminator::Return {
                value: Some(unary_dest),
            },
        };
        let blocks = vec![entry, merge];
        run_seal(&blocks, "test fn");
    }

    #[test]
    #[should_panic(expected = "does not dominate")]
    fn block_param_does_not_leak_across_blocks() {
        // Merge's BlockParam is in scope only inside the
        // dominator-subtree rooted at merge. A sibling block trying
        // to use `merge_param` as an operand hits the dominance
        // check (merge does not dominate sibling).
        let entry_id = IRBlockId(0);
        let sibling_id = IRBlockId(1);
        let merge_id = IRBlockId(2);
        let const_id = ValueId(0);
        let merge_param = ValueId(1);
        let entry = IRBasicBlock {
            id: entry_id,
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![IRInstruction::Const {
                dest: const_id,
                value: ConstValue::Int64(0),
            }],
            terminator: IRTerminator::Branch(BranchTarget::to(sibling_id)),
        };
        // Sibling references `merge_param` directly — illegal
        // because merge does not dominate sibling, so its
        // BlockParam is out of scope here.
        let sibling = IRBasicBlock {
            id: sibling_id,
            label: "sibling".to_string(),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: IRTerminator::Return {
                value: Some(merge_param),
            },
        };
        let merge = IRBasicBlock {
            id: merge_id,
            label: "merge".to_string(),
            params: vec![BlockParam {
                dest: merge_param,
                ty: IRType::Int64,
            }],
            instructions: Vec::new(),
            terminator: IRTerminator::Return {
                value: Some(merge_param),
            },
        };
        let blocks = vec![entry, sibling, merge];
        run_seal(&blocks, "test fn");
    }
}
