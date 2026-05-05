//! Script-shaped seal entry. Re-asserts every per-block invariant
//! [`super::function::seal_block`] checks for fns on the implicit
//! script body ([`IRScript::blocks`] + [`IRScript::return_type`]),
//! then validates all call targets against the script's own
//! [`IRScript::function`] lookup.

use std::collections::BTreeSet;

use crate::function::IRInstruction;
use crate::script::IRScript;
use crate::types::ValueId;

use super::function::{collect_block_ids, seal_block, seal_package};
use super::{require_supported_type, seal_panic};

pub(crate) fn seal_script(script: &IRScript) {
    for pkg in &script.packages {
        seal_package(pkg);
    }
    let owner = "script body";
    if script.blocks.is_empty() {
        seal_panic(&format!("{owner} has no basic blocks"));
    }
    require_supported_type(&script.return_type, &|| format!("{owner} return type"));
    let block_ids = collect_block_ids(&script.blocks, owner);
    let seeded: BTreeSet<ValueId> = BTreeSet::new();
    for block in &script.blocks {
        seal_block(block, owner, &seeded, &block_ids);
    }
    seal_script_calls(script);
}

/// Script counterpart of [`super::program`]'s `seal_program_calls`:
/// `IRScript` carries its own `packages` table; both the inline
/// script body and any helper functions inside `packages` may emit
/// calls, and every one of those must resolve to something
/// `script.function()` can find.
fn seal_script_calls(script: &IRScript) {
    for block in &script.blocks {
        for inst in &block.instructions {
            if let IRInstruction::Call { callee, .. } = inst
                && script.function(callee.mangled()).is_none()
            {
                seal_panic(&format!(
                    "script body calls `{callee}`, but that function is not \
                     registered in the IRScript",
                ));
            }
        }
    }
    for pkg in &script.packages {
        for (owner, function) in &pkg.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    if let IRInstruction::Call { callee, .. } = inst
                        && script.function(callee.mangled()).is_none()
                    {
                        seal_panic(&format!(
                            "function `{owner}` calls `{callee}`, but that function is not \
                             registered in the IRScript",
                        ));
                    }
                }
            }
        }
    }
}
