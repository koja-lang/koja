//! Overwrite-site rewrite for cycle-broken [`IRType::Indirect`]
//! struct fields, run before glue discovery so the shapes it emits
//! seed and rewrite like lowering's own.
//!
//! Boxes are shared under `Clone` (`rc++`), so a field overwrite must
//! release the *box* rc-aware instead of freeing it raw or dropping
//! contents another owner may still reference. Lowering cannot emit
//! this itself: it runs before [`crate::cycle`] stamps the boxes, so
//! every `FieldGet` / `FieldSet` it emits carries the unboxed view.
//!
//! Three overwrite shapes exist, keyed by the projection that pairs
//! with a `FieldSet` on a boxed slot.
//!
//! - A **stale pair** is lowering's `FieldGet` plus an immediate
//!   `DropValue` of the old contents (the leaf of `s.f = v` when the
//!   inner type owns heap). Retyped to the boxed view, the
//!   `DropValue` becomes the rc-aware box release.
//! - A **write-through** is a nested assignment chain that projects
//!   the boxed field, mutates the copy, and re-boxes (`a.b.c = v`
//!   through boxed `b`). The projected copy borrows the box's
//!   constituents, so it must acquire them (a `Clone` of the inner
//!   value) before the chain's stale drop releases one, and the old
//!   box then gets an explicit boxed projection plus `DropValue`.
//! - With **no projection at all**, the inner type owns no heap and
//!   lowering emitted no stale drop. The old box still holds an
//!   allocation and gets the explicit boxed projection plus
//!   `DropValue`.

use std::collections::BTreeMap;

use crate::function::{IRBasicBlock, IRInstruction, IRSymbol};
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

use super::unbox;

/// A `FieldSet` on an `Indirect` slot, which this pass must pair
/// with an rc-aware release of the old box.
struct OverwriteSite {
    base: ValueId,
    block: usize,
    boxed: IRType,
    field_index: u32,
    index: usize,
    struct_symbol: IRSymbol,
}

/// Where the matching projection was found and how it is used.
enum Projection {
    /// Lowering's stale-contents drop, a `FieldGet` followed
    /// immediately by a `DropValue` of its dest.
    StalePair { block: usize, index: usize },
    /// `FieldGet` whose dest flows onward (nested-chain step).
    WriteThrough { block: usize, index: usize },
}

pub(super) fn rewrite_indirect_overwrites(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    let indirect_fields: BTreeMap<(IRSymbol, u32), IRType> = packages
        .iter()
        .flat_map(|package| package.structs.values())
        .flat_map(|decl| {
            decl.fields
                .iter()
                .filter(|field| matches!(&field.ir_type, IRType::Indirect(_)))
                .map(|field| ((decl.symbol.clone(), field.index), field.ir_type.clone()))
        })
        .collect();
    if indirect_fields.is_empty() {
        return;
    }

    for (blocks, param_seed) in packages
        .iter_mut()
        .flat_map(|package| package.functions.values_mut())
        .map(|function| {
            let seed = function
                .params
                .iter()
                .map(|param| param.id.0 + 1)
                .max()
                .unwrap_or(0);
            (function.blocks.as_mut_slice(), seed)
        })
        .chain(std::iter::once((body, 0)))
    {
        rewrite_blocks(blocks, param_seed, &indirect_fields);
    }
}

fn rewrite_blocks(
    blocks: &mut [IRBasicBlock],
    param_seed: u32,
    indirect_fields: &BTreeMap<(IRSymbol, u32), IRType>,
) {
    let sites = collect_sites(blocks, indirect_fields);
    if sites.is_empty() {
        return;
    }
    let mut next_value = high_water_mark(blocks, param_seed);
    // (block, position, instructions to insert before that position)
    let mut insertions: Vec<(usize, usize, Vec<IRInstruction>)> = Vec::new();

    for site in &sites {
        match find_projection(blocks, site) {
            Some(Projection::StalePair { block, index }) => {
                retype_stale_pair(&mut blocks[block].instructions[index..=index + 1], site);
            }
            Some(Projection::WriteThrough { block, index }) => {
                let clone = acquire_projection(
                    &mut blocks[block].instructions[index],
                    site,
                    &mut next_value,
                );
                insertions.push((block, index + 1, vec![clone]));
                insertions.push((
                    site.block,
                    site.index,
                    release_old_box(site, &mut next_value),
                ));
            }
            None => {
                insertions.push((
                    site.block,
                    site.index,
                    release_old_box(site, &mut next_value),
                ));
            }
        }
    }

    apply_insertions(blocks, insertions);
}

fn collect_sites(
    blocks: &[IRBasicBlock],
    indirect_fields: &BTreeMap<(IRSymbol, u32), IRType>,
) -> Vec<OverwriteSite> {
    let mut sites = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let IRInstruction::FieldSet {
                base,
                field_index,
                struct_symbol,
                ..
            } = instruction
            else {
                continue;
            };
            let Some(boxed) = indirect_fields.get(&(struct_symbol.clone(), *field_index)) else {
                continue;
            };
            sites.push(OverwriteSite {
                base: *base,
                block: block_index,
                boxed: boxed.clone(),
                field_index: *field_index,
                index,
                struct_symbol: struct_symbol.clone(),
            });
        }
    }
    sites
}

/// Find the `FieldGet` projecting the overwritten slot off the same
/// base value. At most one exists, since the base is the assignment's
/// own root / chain read, projected once per slot.
fn find_projection(blocks: &[IRBasicBlock], site: &OverwriteSite) -> Option<Projection> {
    for (block_index, block) in blocks.iter().enumerate() {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let IRInstruction::FieldGet {
                base,
                dest,
                field_index,
                struct_symbol,
                ..
            } = instruction
            else {
                continue;
            };
            if *base != site.base
                || *field_index != site.field_index
                || struct_symbol != &site.struct_symbol
            {
                continue;
            }
            let dropped_next = matches!(
                block.instructions.get(index + 1),
                Some(IRInstruction::DropValue { value, .. }) if value == dest
            );
            return Some(if dropped_next {
                Projection::StalePair {
                    block: block_index,
                    index,
                }
            } else {
                Projection::WriteThrough {
                    block: block_index,
                    index,
                }
            });
        }
    }
    None
}

/// Retype lowering's stale `FieldGet` + `DropValue` pair to the boxed
/// view, turning the contents drop into the rc-aware box release.
fn retype_stale_pair(pair: &mut [IRInstruction], site: &OverwriteSite) {
    let [
        IRInstruction::FieldGet { field_type, .. },
        IRInstruction::DropValue { ty, .. },
    ] = pair
    else {
        unreachable!("overwrite rewrite: stale pair shape re-checked after classification");
    };
    *field_type = site.boxed.clone();
    *ty = site.boxed.clone();
}

/// Turn a write-through projection into an owned copy: rename the
/// `FieldGet` dest to a fresh id and hand the old id to a `Clone` of
/// the inner value, so every existing use sees the acquired copy and
/// the chain's stale drop releases the copy's reference, not the
/// (possibly shared) box contents.
fn acquire_projection(
    projection: &mut IRInstruction,
    site: &OverwriteSite,
    next_value: &mut ValueId,
) -> IRInstruction {
    let IRInstruction::FieldGet { dest, .. } = projection else {
        unreachable!("overwrite rewrite: projection shape re-checked after classification");
    };
    let borrowed = fresh(next_value);
    let owned = std::mem::replace(dest, borrowed);
    IRInstruction::Clone {
        dest: owned,
        source: borrowed,
        ty: unbox(&site.boxed).clone(),
    }
}

/// The boxed projection + rc-aware `DropValue` releasing the old box
/// right before the `FieldSet` stores its replacement.
fn release_old_box(site: &OverwriteSite, next_value: &mut ValueId) -> Vec<IRInstruction> {
    let stale = fresh(next_value);
    vec![
        IRInstruction::FieldGet {
            base: site.base,
            dest: stale,
            field_index: site.field_index,
            field_type: site.boxed.clone(),
            struct_symbol: site.struct_symbol.clone(),
        },
        IRInstruction::DropValue {
            value: stale,
            ty: site.boxed.clone(),
        },
    ]
}

/// Splice the planned insertions in, per block, back to front so
/// earlier positions stay valid.
fn apply_insertions(
    blocks: &mut [IRBasicBlock],
    mut insertions: Vec<(usize, usize, Vec<IRInstruction>)>,
) {
    insertions.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    for (block, position, instructions) in insertions.into_iter().rev() {
        blocks[block]
            .instructions
            .splice(position..position, instructions);
    }
}

/// The next `ValueId` past every value the function defines
/// (function params via `param_seed`, block params, and instruction
/// dests).
fn high_water_mark(blocks: &[IRBasicBlock], param_seed: u32) -> ValueId {
    let mut max = param_seed;
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
    ValueId(max)
}

fn fresh(next: &mut ValueId) -> ValueId {
    let id = *next;
    next.0 += 1;
    id
}
