//! Per-function and per-block invariants. Both
//! [`super::program::seal_program`] and [`super::script::seal_script`]
//! call into [`seal_package`] / [`seal_block`] — the actual block
//! validation is identical for fn-shaped and script-shaped IR; the
//! only difference is what the surrounding seeded-set looks like
//! (function params seeded for fns, empty for scripts).

use std::collections::{BTreeSet, HashSet};

use crate::function::{FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction};
use crate::package::IRPackage;
use crate::types::ValueId;

use super::{
    instruction_operands, require_defined, require_supported_const, require_supported_type,
    seal_panic, terminator_operands, terminator_targets,
};

pub(super) fn seal_package(pkg: &IRPackage) {
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
    match function.kind {
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
    }
    require_supported_type(&function.return_type, &|| format!("{owner} return type"));
    // Parameter `ValueId`s count as definitions for the purposes of
    // operand references inside the entry block. Seed them once and
    // pass the seed by reference into every block walk; per-block
    // defined-set composes onto the seed without mutating it across
    // blocks (cross-block value flow doesn't appear in this slice).
    let mut seeded: BTreeSet<ValueId> = BTreeSet::new();
    for (index, param) in function.params.iter().enumerate() {
        require_supported_type(&param.ty, &|| {
            format!("{owner} parameter #{index} ({}) type", param.id)
        });
        if !seeded.insert(param.id) {
            seal_panic(&format!(
                "{owner} lists duplicate parameter value `{}`",
                param.id,
            ));
        }
    }
    let block_ids = collect_block_ids(&function.blocks, &owner);
    for block in &function.blocks {
        seal_block(block, &owner, &seeded, &block_ids);
    }
}

/// Build the per-function block-id set used to validate every
/// terminator target. Asserts uniqueness en route.
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

pub(super) fn seal_block(
    block: &IRBasicBlock,
    owner: &str,
    seeded: &BTreeSet<ValueId>,
    block_ids: &HashSet<IRBlockId>,
) {
    let mut defined = seeded.clone();
    for inst in &block.instructions {
        for operand in instruction_operands(inst) {
            require_defined(operand, owner, &defined);
        }
        if let IRInstruction::Const { value, .. } = inst {
            require_supported_const(value, &|| {
                format!("{owner} const instruction at {}", inst.dest())
            });
        }
        if !defined.insert(inst.dest()) {
            seal_panic(&format!("{owner} redefines value `{}`", inst.dest()));
        }
    }
    for operand in terminator_operands(&block.terminator) {
        require_defined(operand, owner, &defined);
    }
    for target in terminator_targets(&block.terminator) {
        if !block_ids.contains(&target) {
            seal_panic(&format!(
                "{owner} block {} terminator targets unknown block `{target}`",
                block.id,
            ));
        }
    }
}
