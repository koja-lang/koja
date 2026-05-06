//! Per-function and per-block invariants. [`seal_package`] /
//! [`seal_block`] are reused for both function- and script-shaped IR;
//! only the seeded `ValueId` set differs (params for fns, empty for
//! scripts).

use std::collections::{BTreeSet, HashSet};

use crate::function::{FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::ValueId;

use super::structs::seal_struct_decls;
use super::{
    instruction_operands, require_defined, require_supported_const, require_supported_type,
    seal_panic, terminator_operands, terminator_targets,
};

pub(super) fn seal_package(pkg: &IRPackage) {
    seal_struct_decls(pkg);
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
    // Param `ValueId`s seed every block's operand-defined set.
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
    seal_locals(function, &owner);
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
    // Intrinsics carry params for backend signature shape but emit
    // no body, so they have no matching `LocalDecl`s to check.
    if function.kind == FunctionKind::Regular {
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
        if let IRInstruction::Const { value, dest } = inst {
            require_supported_const(value, &|| format!("{owner} const instruction at {dest}"));
        }
        // Local-slot instructions don't define a `ValueId`; their
        // slot-identity invariants are checked in `seal_locals`.
        if let Some(dest) = inst.dest()
            && !defined.insert(dest)
        {
            seal_panic(&format!("{owner} redefines value `{dest}`"));
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
