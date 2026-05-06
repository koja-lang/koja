//! Per-function and per-block invariants. Both
//! [`super::program::seal_program`] and [`super::script::seal_script`]
//! call into [`seal_package`] / [`seal_block`] — the actual block
//! validation is identical for fn-shaped and script-shaped IR; the
//! only difference is what the surrounding seeded-set looks like
//! (function params seeded for fns, empty for scripts).

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
    seal_locals(function, &owner);
}

/// Per-function local-slot invariants:
///
/// - Every [`IRLocalId`] is `LocalDecl`'d exactly once. Two declarations
///   for the same id would imply two slots claiming the same identity,
///   which the lower pass should never emit.
/// - Every `LocalRead` / `LocalWrite` references a previously declared
///   `IRLocalId`. Block-local flow analysis is a follow-up; today the
///   lower pass guarantees the `LocalDecl` lands in the entry block
///   ahead of any read/write, so a function-wide membership check is
///   tight enough.
/// - Every parameter's [`crate::IRFunctionParam::local_id`] is among
///   the declared set; param promotion at function entry must emit
///   the matching `LocalDecl` so body references work uniformly with
///   body-declared locals.
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
    // Intrinsics carry params for backend signature shape but emit no
    // body and therefore no `LocalDecl`s. Param promotion is a body
    // concern; skip the membership check on the empty-blocks shape.
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
        if let IRInstruction::Const { value, dest } = inst {
            require_supported_const(value, &|| format!("{owner} const instruction at {dest}"));
        }
        // Local-slot instructions don't define a `ValueId` and so
        // don't participate in the per-block defined-set walk; the
        // slot-identity invariants (one decl per slot, etc.) are
        // checked function-wide in `seal_locals`.
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
