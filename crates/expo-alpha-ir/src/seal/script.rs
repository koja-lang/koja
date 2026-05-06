//! Script-shaped seal entry. Re-asserts every per-block invariant
//! [`super::function::seal_block`] checks for fns on the implicit
//! script body ([`IRScript::blocks`] + [`IRScript::return_type`]),
//! then validates all call targets against the script's own
//! [`IRScript::function`] lookup.

use std::collections::BTreeSet;

use crate::function::IRInstruction;
use crate::script::IRScript;
use crate::types::ValueId;

use super::enums::seal_enum_ops;
use super::function::{collect_block_ids, seal_block, seal_package};
use super::structs::{package_instructions, script_body_instructions, seal_struct_ops};
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
    seal_script_struct_ops(script);
    seal_script_enum_ops(script);
}

/// Cross-IR struct check for script-shaped output. Mirrors
/// [`super::program::seal_program_struct_ops`]: walks both the
/// implicit script body and every package fragment, validating each
/// `StructInit` / `FieldGet` against the assembled struct lookup.
fn seal_script_struct_ops(script: &IRScript) {
    let lookup = |mangled: &str| script.struct_decl(mangled);
    seal_struct_ops(script_body_instructions(&script.blocks), &lookup);
    for pkg in &script.packages {
        seal_struct_ops(package_instructions(pkg), &lookup);
    }
}

/// Cross-IR enum check for script-shaped output. Mirrors
/// [`super::program::seal_program_struct_ops`] / `seal_program_enum_ops`:
/// walks both the implicit script body and every package fragment,
/// validating each `EnumConstruct` against the assembled enum lookup.
fn seal_script_enum_ops(script: &IRScript) {
    let lookup = |mangled: &str| script.enum_decl(mangled);
    seal_enum_ops(script_body_instructions(&script.blocks), &lookup);
    for pkg in &script.packages {
        seal_enum_ops(package_instructions(pkg), &lookup);
    }
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
