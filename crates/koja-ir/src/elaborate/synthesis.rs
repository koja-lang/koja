//! IR body synthesis for *aggregate* clone / drop glue (`Struct` /
//! `Enum` / `Union`). Given the operand type and the program's decls,
//! [`clone_body`] / [`drop_body`] build a self-contained CFG that
//! projects each constituent, acquires / releases it (recursing into
//! the constituent's own glue by `Call`), and — for clone — rebuilds
//! the aggregate.
//!
//! Every body's single parameter is [`SELF_VALUE`] (`ValueId(0)`),
//! typed as the operand. Fresh SSA values number from 1 and blocks
//! from 0, so the bodies stand alone (no [`crate::FnLowerCtx`]
//! counter to thread). The shapes mirror what lowering already emits
//! for field projection (`FieldGet` / `StructInit`), enum match
//! dispatch (`EnumTagGet` + `Int8` tag compare + `CondBranch`), and
//! union match dispatch (`UnionTagGet` / `UnionPayloadGet` /
//! `UnionWrap`), so they pass seal's SSA / block-param invariants the
//! same way hand-lowered bodies do.

use crate::cfg::CFGBuilder;
use crate::enum_decl::{EnumPayloadInit, IRVariantPayload};
use crate::function::{
    BranchTarget, IRBasicBlock, IRBlockId, IRInstruction, IRSymbol, IRTerminator,
};
use crate::mangling::{clone_glue_symbol, drop_glue_symbol};
use crate::package::IRPackage;
use crate::struct_decl::StructFieldInit;
use crate::types::{ConstValue, IRBinOp, IRType, ValueId};

use super::{find_enum, find_struct, is_leaf, needs_drop, needs_glue};

/// The glue's sole parameter — `self`, typed as the operand. Both the
/// shell ([`super::glue_shell`]) and every synthesized body agree on
/// this id so projections read straight off it.
pub(super) const SELF_VALUE: ValueId = ValueId(0);

/// Build the `clone_T` body for an aggregate `ty`. Panics if `ty` is
/// not a synthesizable aggregate — callers gate on [`super::is_aggregate`].
pub(super) fn clone_body(ty: &IRType, packages: &[IRPackage]) -> Vec<IRBasicBlock> {
    let mut synthesizer = Synthesizer::new();
    match ty {
        IRType::Struct(symbol) => synthesizer.struct_clone(symbol, packages),
        IRType::Enum(symbol) => synthesizer.enum_clone(ty, symbol, packages),
        IRType::Union { members, .. } => synthesizer.union_clone(ty, members, packages),
        other => panic!("elaborate synthesis: clone_body on non-aggregate {other:?}"),
    }
    synthesizer.finish()
}

/// Build the `drop_T` body for an aggregate `ty`. Panics if `ty` is
/// not a synthesizable aggregate — callers gate on [`super::is_aggregate`].
pub(super) fn drop_body(ty: &IRType, packages: &[IRPackage]) -> Vec<IRBasicBlock> {
    let mut synthesizer = Synthesizer::new();
    match ty {
        IRType::Struct(symbol) => synthesizer.struct_drop(symbol, packages),
        IRType::Enum(symbol) => synthesizer.enum_drop(symbol, packages),
        IRType::Union { members, .. } => synthesizer.union_drop(ty, members, packages),
        other => panic!("elaborate synthesis: drop_body on non-aggregate {other:?}"),
    }
    synthesizer.finish()
}

/// How a single constituent is acquired at a clone boundary / released
/// at a drop boundary.
enum Disposition {
    /// `Copy` scalar (or a no-glue aggregate): clone is the same SSA
    /// value, drop is a no-op.
    Trivial,
    /// Heap leaf: inline `Clone` (`rc++`) / `DropValue` (`rc--`).
    Leaf,
    /// Heap-managed composite: `Call` its own `clone_T` / `drop_T`.
    Glue,
}

fn disposition(ty: &IRType, packages: &[IRPackage]) -> Disposition {
    if is_leaf(ty) {
        Disposition::Leaf
    } else if needs_glue(ty, packages) {
        Disposition::Glue
    } else {
        Disposition::Trivial
    }
}

/// Self-contained CFG accumulator for one glue body: a [`CFGBuilder`]
/// plus the fresh-value / fresh-block counters lowering would
/// otherwise own on [`crate::FnLowerCtx`].
struct Synthesizer {
    cfg: CFGBuilder,
    next_block: u32,
    next_value: u32,
}

impl Synthesizer {
    fn new() -> Self {
        Self {
            cfg: CFGBuilder::new(),
            next_block: 0,
            // 0 is the `self` parameter; bodies number from 1.
            next_value: 1,
        }
    }

    fn finish(self) -> Vec<IRBasicBlock> {
        self.cfg.into_blocks_with_closed().0
    }

    fn value(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn block(&mut self, label: impl Into<String>) -> IRBlockId {
        let id = IRBlockId(self.next_block);
        self.next_block += 1;
        self.cfg.add_block(id, label);
        id
    }

    fn append(&mut self, block: IRBlockId, instruction: IRInstruction) {
        self.cfg.append(block, instruction);
    }

    /// Acquire `value` (typed `ty`) into `block`, returning the owned
    /// SSA value to store into the rebuilt aggregate.
    fn acquire(
        &mut self,
        block: IRBlockId,
        value: ValueId,
        ty: &IRType,
        packages: &[IRPackage],
    ) -> ValueId {
        match disposition(ty, packages) {
            Disposition::Trivial => value,
            Disposition::Leaf => {
                let dest = self.value();
                self.append(
                    block,
                    IRInstruction::Clone {
                        dest,
                        source: value,
                        ty: ty.clone(),
                    },
                );
                dest
            }
            Disposition::Glue => {
                let dest = self.value();
                self.append(
                    block,
                    IRInstruction::Call {
                        dest,
                        callee: clone_glue_symbol(ty),
                        args: vec![value],
                    },
                );
                dest
            }
        }
    }

    /// Release `value` (typed `ty`) in `block`. A no-op for `Copy`
    /// constituents.
    fn release(&mut self, block: IRBlockId, value: ValueId, ty: &IRType, packages: &[IRPackage]) {
        match disposition(ty, packages) {
            Disposition::Trivial => {}
            Disposition::Leaf => self.append(
                block,
                IRInstruction::DropValue {
                    value,
                    ty: ty.clone(),
                },
            ),
            Disposition::Glue => {
                let dest = self.value();
                self.append(
                    block,
                    IRInstruction::Call {
                        dest,
                        callee: drop_glue_symbol(ty),
                        args: vec![value],
                    },
                );
            }
        }
    }

    // --- struct ----------------------------------------------------

    fn struct_clone(&mut self, symbol: &IRSymbol, packages: &[IRPackage]) {
        let decl = find_struct(packages, symbol)
            .unwrap_or_else(|| panic!("elaborate synth: clone of unregistered struct `{symbol}`"));
        let fields = decl.fields.clone();
        let entry = self.block("entry");
        let mut inits = Vec::with_capacity(fields.len());
        for field in &fields {
            let projected = self.value();
            self.append(
                entry,
                IRInstruction::FieldGet {
                    base: SELF_VALUE,
                    dest: projected,
                    field_index: field.index,
                    field_type: field.ir_type.clone(),
                    struct_symbol: symbol.clone(),
                },
            );
            let owned = self.acquire(entry, projected, &field.ir_type, packages);
            inits.push(StructFieldInit {
                index: field.index,
                value: owned,
            });
        }
        let result = self.value();
        self.append(
            entry,
            IRInstruction::StructInit {
                dest: result,
                fields: inits,
                ty: symbol.clone(),
            },
        );
        self.cfg.set_terminator(
            entry,
            IRTerminator::Return {
                value: Some(result),
            },
        );
    }

    fn struct_drop(&mut self, symbol: &IRSymbol, packages: &[IRPackage]) {
        let decl = find_struct(packages, symbol)
            .unwrap_or_else(|| panic!("elaborate synth: drop of unregistered struct `{symbol}`"));
        let fields = decl.fields.clone();
        let entry = self.block("entry");
        for field in &fields {
            if !needs_drop(&field.ir_type, packages) {
                continue;
            }
            let projected = self.value();
            self.append(
                entry,
                IRInstruction::FieldGet {
                    base: SELF_VALUE,
                    dest: projected,
                    field_index: field.index,
                    field_type: field.ir_type.clone(),
                    struct_symbol: symbol.clone(),
                },
            );
            self.release(entry, projected, &field.ir_type, packages);
        }
        self.cfg
            .set_terminator(entry, IRTerminator::Return { value: None });
    }

    // --- enum ------------------------------------------------------

    fn enum_clone(&mut self, enum_ty: &IRType, symbol: &IRSymbol, packages: &[IRPackage]) {
        let decl = find_enum(packages, symbol)
            .unwrap_or_else(|| panic!("elaborate synth: clone of unregistered enum `{symbol}`"));
        let variants = decl.variants.clone();
        let entry = self.block("entry");
        let tag = self.value();
        self.append(
            entry,
            IRInstruction::EnumTagGet {
                dest: tag,
                value: SELF_VALUE,
                ty: symbol.clone(),
            },
        );
        let (bodies, join) = self.tag_dispatch(entry, tag, variants.len());
        let join_param = self.value();
        self.cfg
            .declare_block_param(join, join_param, enum_ty.clone());
        self.cfg.set_terminator(
            join,
            IRTerminator::Return {
                value: Some(join_param),
            },
        );

        for (variant, &body) in variants.iter().zip(&bodies) {
            let payload = match &variant.payload {
                IRVariantPayload::Unit => EnumPayloadInit::Unit,
                IRVariantPayload::Tuple(types) => {
                    let mut values = Vec::with_capacity(types.len());
                    for (payload_index, field_ty) in types.iter().enumerate() {
                        let projected = self.enum_payload_get(
                            body,
                            symbol,
                            variant.tag,
                            payload_index as u32,
                            field_ty,
                        );
                        values.push(self.acquire(body, projected, field_ty, packages));
                    }
                    EnumPayloadInit::Tuple(values)
                }
                IRVariantPayload::Struct(fields) => {
                    let mut inits = Vec::with_capacity(fields.len());
                    for field in fields {
                        let projected = self.enum_payload_get(
                            body,
                            symbol,
                            variant.tag,
                            field.index,
                            &field.ir_type,
                        );
                        let owned = self.acquire(body, projected, &field.ir_type, packages);
                        inits.push(StructFieldInit {
                            index: field.index,
                            value: owned,
                        });
                    }
                    EnumPayloadInit::Struct(inits)
                }
            };
            let result = self.value();
            self.append(
                body,
                IRInstruction::EnumConstruct {
                    dest: result,
                    payload,
                    tag: variant.tag,
                    ty: symbol.clone(),
                },
            );
            self.cfg.set_terminator(
                body,
                IRTerminator::Branch(BranchTarget::with_args(join, vec![result])),
            );
        }
    }

    fn enum_drop(&mut self, symbol: &IRSymbol, packages: &[IRPackage]) {
        let decl = find_enum(packages, symbol)
            .unwrap_or_else(|| panic!("elaborate synth: drop of unregistered enum `{symbol}`"));
        let variants = decl.variants.clone();
        let entry = self.block("entry");
        let tag = self.value();
        self.append(
            entry,
            IRInstruction::EnumTagGet {
                dest: tag,
                value: SELF_VALUE,
                ty: symbol.clone(),
            },
        );
        let (bodies, join) = self.tag_dispatch(entry, tag, variants.len());
        self.cfg
            .set_terminator(join, IRTerminator::Return { value: None });

        for (variant, &body) in variants.iter().zip(&bodies) {
            match &variant.payload {
                IRVariantPayload::Unit => {}
                IRVariantPayload::Tuple(types) => {
                    for (payload_index, field_ty) in types.iter().enumerate() {
                        if !needs_drop(field_ty, packages) {
                            continue;
                        }
                        let projected = self.enum_payload_get(
                            body,
                            symbol,
                            variant.tag,
                            payload_index as u32,
                            field_ty,
                        );
                        self.release(body, projected, field_ty, packages);
                    }
                }
                IRVariantPayload::Struct(fields) => {
                    for field in fields {
                        if !needs_drop(&field.ir_type, packages) {
                            continue;
                        }
                        let projected = self.enum_payload_get(
                            body,
                            symbol,
                            variant.tag,
                            field.index,
                            &field.ir_type,
                        );
                        self.release(body, projected, &field.ir_type, packages);
                    }
                }
            }
            self.cfg
                .set_terminator(body, IRTerminator::Branch(BranchTarget::to(join)));
        }
    }

    fn enum_payload_get(
        &mut self,
        block: IRBlockId,
        symbol: &IRSymbol,
        tag: crate::enum_decl::IRVariantTag,
        payload_index: u32,
        field_type: &IRType,
    ) -> ValueId {
        let dest = self.value();
        self.append(
            block,
            IRInstruction::EnumPayloadFieldGet {
                dest,
                value: SELF_VALUE,
                tag,
                payload_index,
                field_type: field_type.clone(),
                ty: symbol.clone(),
            },
        );
        dest
    }

    // --- union -----------------------------------------------------

    fn union_clone(&mut self, union_ty: &IRType, members: &[IRType], packages: &[IRPackage]) {
        let entry = self.block("entry");
        let tag = self.value();
        self.append(
            entry,
            IRInstruction::UnionTagGet {
                dest: tag,
                ty: union_ty.clone(),
                value: SELF_VALUE,
            },
        );
        let (bodies, join) = self.tag_dispatch(entry, tag, members.len());
        let join_param = self.value();
        self.cfg
            .declare_block_param(join, join_param, union_ty.clone());
        self.cfg.set_terminator(
            join,
            IRTerminator::Return {
                value: Some(join_param),
            },
        );

        for (member_index, member_ty) in members.iter().enumerate() {
            let body = bodies[member_index];
            let member_index = member_index as u8;
            let projected = self.union_payload_get(body, union_ty, member_index, member_ty);
            let owned = self.acquire(body, projected, member_ty, packages);
            let result = self.value();
            self.append(
                body,
                IRInstruction::UnionWrap {
                    dest: result,
                    member_index,
                    member_type: member_ty.clone(),
                    ty: union_ty.clone(),
                    value: owned,
                },
            );
            self.cfg.set_terminator(
                body,
                IRTerminator::Branch(BranchTarget::with_args(join, vec![result])),
            );
        }
    }

    fn union_drop(&mut self, union_ty: &IRType, members: &[IRType], packages: &[IRPackage]) {
        let entry = self.block("entry");
        let tag = self.value();
        self.append(
            entry,
            IRInstruction::UnionTagGet {
                dest: tag,
                ty: union_ty.clone(),
                value: SELF_VALUE,
            },
        );
        let (bodies, join) = self.tag_dispatch(entry, tag, members.len());
        self.cfg
            .set_terminator(join, IRTerminator::Return { value: None });

        for (member_index, member_ty) in members.iter().enumerate() {
            let body = bodies[member_index];
            if needs_drop(member_ty, packages) {
                let projected =
                    self.union_payload_get(body, union_ty, member_index as u8, member_ty);
                self.release(body, projected, member_ty, packages);
            }
            self.cfg
                .set_terminator(body, IRTerminator::Branch(BranchTarget::to(join)));
        }
    }

    fn union_payload_get(
        &mut self,
        block: IRBlockId,
        union_ty: &IRType,
        member_index: u8,
        member_type: &IRType,
    ) -> ValueId {
        let dest = self.value();
        self.append(
            block,
            IRInstruction::UnionPayloadGet {
                dest,
                member_index,
                member_type: member_type.clone(),
                ty: union_ty.clone(),
                value: SELF_VALUE,
            },
        );
        dest
    }

    // --- shared tag dispatch --------------------------------------

    /// Build the tag-dispatch skeleton for an `arm_count`-way enum /
    /// union switch keyed on the already-projected `tag` (`Int8`) in
    /// `entry`. Returns one (open) body block per arm plus the shared
    /// `join` block; the caller fills each body and terminates it with
    /// a branch to `join`, and sets `join`'s terminator.
    ///
    /// Arms `0..arm_count-1` are reached by an equality gate; the last
    /// arm is the final `else`, since the tag is statically one of the
    /// arms (typecheck-exhaustive). A single-arm switch branches to its
    /// body unconditionally.
    fn tag_dispatch(
        &mut self,
        entry: IRBlockId,
        tag: ValueId,
        arm_count: usize,
    ) -> (Vec<IRBlockId>, IRBlockId) {
        let join = self.block("glue_join");
        let bodies: Vec<IRBlockId> = (0..arm_count)
            .map(|index| self.block(format!("arm{index}")))
            .collect();

        if arm_count == 1 {
            self.cfg
                .set_terminator(entry, IRTerminator::Branch(BranchTarget::to(bodies[0])));
            return (bodies, join);
        }

        let mut check = entry;
        for index in 0..arm_count - 1 {
            let constant = self.value();
            self.append(
                check,
                IRInstruction::Const {
                    dest: constant,
                    value: ConstValue::Int8(index as i8),
                },
            );
            let matches = self.value();
            self.append(
                check,
                IRInstruction::BinaryOp {
                    dest: matches,
                    lhs: tag,
                    op: IRBinOp::Eq,
                    rhs: constant,
                },
            );
            let next = if index < arm_count - 2 {
                self.block("check")
            } else {
                bodies[arm_count - 1]
            };
            self.cfg.set_terminator(
                check,
                IRTerminator::CondBranch {
                    cond: matches,
                    then_target: BranchTarget::to(bodies[index]),
                    else_target: BranchTarget::to(next),
                },
            );
            check = next;
        }
        (bodies, join)
    }
}
