//! Acquisition / release rewrite: turn every *composite* ownership
//! `Clone` / `DeepCopy` / `DropLocal` / `DropValue` lowering emitted
//! into a `Call` to the synthesized per-type glue, so backends only
//! ever see leaf `Clone` / `DeepCopy` / `Drop` inline and a uniform
//! `Call` for composites (northstar: no dynamic-dispatch IR).
//!
//! A composite is rewritten iff its type carries glue, i.e. it is in
//! the `needed` set [`super::discover_glue_types`] produced (or, for
//! `DeepCopy`, the `deep_needed` set from
//! [`super::discover_deep_copy_types`]). Those sets are exactly the
//! heap-managed composites, and a no-glue composite (a struct of scalars,
//! say) is left as a plain `Clone` / `Drop` the backend renders as a
//! register copy / no-op. Leaves stay inline `rc++` / `rc--`.
//!
//! `DropLocal` names a *slot*, not a value, so its rewrite expands to
//! a `LocalRead` of the slot followed by the glue `Call`. `Clone` and
//! `DropValue` are already value-keyed and rewrite in place.
//!
//! Fresh `ValueId`s (for the `LocalRead` result and each glue call's
//! unit sink) are minted above the function's current high-water mark
//! so they can't collide with an existing definition.

use std::collections::BTreeSet;

use crate::function::{IRBasicBlock, IRFunction, IRInstruction};
use crate::mangling::{clone_glue_symbol, deep_copy_glue_symbol, drop_glue_symbol};
use crate::types::{IRType, ValueId};

/// Rewrite every function body in `packages` plus the (optional)
/// script `body`. Borrows only the classification sets, so it can
/// mutate each body freely.
pub(super) fn rewrite_function(
    function: &mut IRFunction,
    needed: &BTreeSet<IRType>,
    deep_needed: &BTreeSet<IRType>,
) {
    if needed.is_empty() && deep_needed.is_empty() {
        return;
    }
    let seed = function
        .params
        .iter()
        .map(|param| param.id.0)
        .max()
        .map_or(0, |max| max + 1);
    rewrite_blocks(&mut function.blocks, needed, deep_needed, seed);
}

/// Rewrite a standalone block list (the script body, which has no
/// function params to seed the value counter).
pub(super) fn rewrite_blocks_standalone(
    blocks: &mut [IRBasicBlock],
    needed: &BTreeSet<IRType>,
    deep_needed: &BTreeSet<IRType>,
) {
    if needed.is_empty() && deep_needed.is_empty() {
        return;
    }
    rewrite_blocks(blocks, needed, deep_needed, 0);
}

fn rewrite_blocks(
    blocks: &mut [IRBasicBlock],
    needed: &BTreeSet<IRType>,
    deep_needed: &BTreeSet<IRType>,
    seed: u32,
) {
    let mut next = ValueId(seed.max(high_water_mark(blocks)));
    for block in blocks.iter_mut() {
        let mut rewritten = Vec::with_capacity(block.instructions.len());
        for instruction in block.instructions.drain(..) {
            rewrite_instruction(instruction, needed, deep_needed, &mut next, &mut rewritten);
        }
        block.instructions = rewritten;
    }
}

fn rewrite_instruction(
    instruction: IRInstruction,
    needed: &BTreeSet<IRType>,
    deep_needed: &BTreeSet<IRType>,
    next: &mut ValueId,
    out: &mut Vec<IRInstruction>,
) {
    match instruction {
        IRInstruction::Clone { dest, source, ty } if needed.contains(&ty) => {
            out.push(IRInstruction::Call {
                dest,
                callee: clone_glue_symbol(&ty),
                args: vec![source],
            });
        }
        IRInstruction::DeepCopy { dest, source, ty } if deep_needed.contains(&ty) => {
            out.push(IRInstruction::Call {
                dest,
                callee: deep_copy_glue_symbol(&ty),
                args: vec![source],
            });
        }
        IRInstruction::DropValue { value, ty } if needed.contains(&ty) => {
            out.push(IRInstruction::Call {
                dest: fresh(next),
                callee: drop_glue_symbol(&ty),
                args: vec![value],
            });
        }
        IRInstruction::DropLocal { local, ty } if needed.contains(&ty) => {
            let loaded = fresh(next);
            out.push(IRInstruction::LocalRead {
                dest: loaded,
                local,
                ty: ty.clone(),
            });
            out.push(IRInstruction::Call {
                dest: fresh(next),
                callee: drop_glue_symbol(&ty),
                args: vec![loaded],
            });
        }
        other => out.push(other),
    }
}

/// The next `ValueId` past every value the blocks already define
/// (block params + instruction dests). Combined with the param seed
/// so the counter clears both.
fn high_water_mark(blocks: &[IRBasicBlock]) -> u32 {
    let mut max = 0;
    for block in blocks {
        for param in &block.params {
            max = max.max(param.dest.0 + 1);
        }
        for instruction in &block.instructions {
            if let Some(dest) = instruction.dest() {
                max = max.max(dest.0 + 1);
            }
        }
    }
    max
}

fn fresh(next: &mut ValueId) -> ValueId {
    let id = *next;
    next.0 += 1;
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{IRBlockId, IRSymbol, IRTerminator};
    use crate::local::IRLocalId;

    #[test]
    fn composite_clone_and_drop_rewrite_to_glue_calls() {
        let composite = IRType::Struct(IRSymbol::synthetic("Test.S".to_string()));
        let mut needed = BTreeSet::new();
        needed.insert(composite.clone());

        let mut blocks = vec![IRBasicBlock {
            id: IRBlockId(0),
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![
                IRInstruction::Clone {
                    dest: ValueId(1),
                    source: ValueId(0),
                    ty: composite.clone(),
                },
                IRInstruction::DropValue {
                    value: ValueId(1),
                    ty: composite.clone(),
                },
                IRInstruction::DropLocal {
                    local: IRLocalId::synthetic_placeholder(),
                    ty: composite.clone(),
                },
                // A leaf `Clone` is left inline (backend `rc++`).
                IRInstruction::Clone {
                    dest: ValueId(2),
                    source: ValueId(0),
                    ty: IRType::String,
                },
            ],
            terminator: IRTerminator::Return { value: None },
        }];

        rewrite_blocks_standalone(&mut blocks, &needed, &BTreeSet::new());
        let instructions = &blocks[0].instructions;
        // Clone -> Call clone glue, DropValue -> Call drop glue,
        // DropLocal -> LocalRead + Call drop glue, and the leaf
        // Clone stays intact.
        assert_eq!(instructions.len(), 5);
        assert!(matches!(
            &instructions[0],
            IRInstruction::Call { callee, .. } if callee.mangled().ends_with(".$clone$")
        ));
        assert!(matches!(
            &instructions[1],
            IRInstruction::Call { callee, .. } if callee.mangled().ends_with(".$drop$")
        ));
        assert!(matches!(&instructions[2], IRInstruction::LocalRead { .. }));
        assert!(matches!(
            &instructions[3],
            IRInstruction::Call { callee, .. } if callee.mangled().ends_with(".$drop$")
        ));
        assert!(matches!(
            &instructions[4],
            IRInstruction::Clone {
                ty: IRType::String,
                ..
            }
        ));
    }

    #[test]
    fn composite_deep_copy_rewrites_to_deep_copy_glue_call() {
        let composite = IRType::Struct(IRSymbol::synthetic("Test.S".to_string()));
        let mut deep_needed = BTreeSet::new();
        deep_needed.insert(composite.clone());

        let mut blocks = vec![IRBasicBlock {
            id: IRBlockId(0),
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![
                IRInstruction::DeepCopy {
                    dest: ValueId(1),
                    source: ValueId(0),
                    ty: composite.clone(),
                },
                // A `Clone` of the same type stays inline, because only
                // the deep-copy family is in `deep_needed`.
                IRInstruction::Clone {
                    dest: ValueId(2),
                    source: ValueId(0),
                    ty: composite.clone(),
                },
                // A leaf `DeepCopy` is left inline (backend heap copy).
                IRInstruction::DeepCopy {
                    dest: ValueId(3),
                    source: ValueId(0),
                    ty: IRType::String,
                },
            ],
            terminator: IRTerminator::Return { value: None },
        }];

        rewrite_blocks_standalone(&mut blocks, &BTreeSet::new(), &deep_needed);
        let instructions = &blocks[0].instructions;
        assert_eq!(instructions.len(), 3);
        assert!(matches!(
            &instructions[0],
            IRInstruction::Call { callee, .. } if callee.mangled().ends_with(".$deep_copy$")
        ));
        assert!(matches!(&instructions[1], IRInstruction::Clone { .. }));
        assert!(matches!(
            &instructions[2],
            IRInstruction::DeepCopy {
                ty: IRType::String,
                ..
            }
        ));
    }

    #[test]
    fn no_glue_set_leaves_everything_untouched() {
        let mut blocks = vec![IRBasicBlock {
            id: IRBlockId(0),
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![IRInstruction::Clone {
                dest: ValueId(1),
                source: ValueId(0),
                ty: IRType::Struct(IRSymbol::synthetic("Test.S".to_string())),
            }],
            terminator: IRTerminator::Return { value: None },
        }];
        rewrite_blocks_standalone(&mut blocks, &BTreeSet::new(), &BTreeSet::new());
        assert!(matches!(
            &blocks[0].instructions[0],
            IRInstruction::Clone { .. }
        ));
    }
}
