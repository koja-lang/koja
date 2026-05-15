//! Plain-struct destructure pattern lowering.
//!
//! Walks each [`FieldPattern`] against the corresponding declared
//! field type. Wildcard fields contribute nothing; binding fields
//! emit a chained [`PayloadBind`] starting at
//! [`BindOp::StructField`]. Literal / nested-struct / nested-enum
//! field patterns recurse via [`super::lower_pattern_check`]
//! against the field's extracted value, with their resulting
//! `TestStep`s and `PayloadBind`s concatenated into the outer
//! chain under [`ChainMode::And`].
//!
//! When every listed field's interior reduces to a catch-all the
//! lowering folds back to a [`PatternCheck::CatchAll`] so the
//! match driver can short-circuit the chain.

use expo_ast::ast::{FieldPattern, Pattern};
use expo_ast::identifier::{Resolution, ResolvedType};

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::structs::{resolved_struct_symbol, struct_definition_from_resolution};
use super::{
    BindOp, BindStep, ChainMode, PatternCheck, PatternInputs, PayloadBind, TestStep,
    ensure_local_declared, field_type_for, lower_pattern_check, require_local,
};
use crate::function::{IRBlockId, IRInstruction};
use crate::types::{IRType, ValueId};

pub(super) fn lower_struct_check(
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> (PatternCheck, IRBlockId) {
    let definition =
        struct_definition_from_resolution(inputs.subject_ty, inputs.registry, "struct pattern");
    let struct_symbol = resolved_struct_symbol(
        inputs.subject_ty,
        inputs.registry,
        &mut output.instantiations,
    );
    let owner = match inputs.subject_ty {
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => *id,
        _ => panic!(
            "alpha IR lower: struct pattern subject has non-Global resolution after \
             typecheck seal",
        ),
    };
    let mut binds = Vec::new();
    let mut steps = Vec::new();
    let mut current_block = block;
    for field in fields {
        let (field_index, declared) = definition.lookup_field(&field.name).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: struct pattern references unknown field `{}` — \
                 typecheck invariant violation",
                field.name,
            )
        });
        let (field_resolved_ty, field_ir_type) =
            field_type_for(&declared.ty, owner, inputs, output);
        let prefix = BindStep {
            op: BindOp::StructField {
                field_index,
                struct_symbol: struct_symbol.clone(),
            },
            output_type: field_ir_type.clone(),
        };
        lower_subpattern_into(
            &field.pattern,
            &field_resolved_ty,
            &field_ir_type,
            inputs.subject,
            prefix,
            inputs,
            ctx,
            &mut current_block,
            &mut steps,
            &mut binds,
            output,
        );
    }
    if steps.is_empty() {
        (PatternCheck::CatchAll { binds }, current_block)
    } else {
        (
            PatternCheck::Tests {
                chain_mode: ChainMode::And,
                payload_binds: binds,
                steps,
            },
            current_block,
        )
    }
}

/// Lower one nested sub-pattern (a struct field or enum payload
/// position) into the outer chain. `prefix` is the extraction op
/// (e.g. `BindOp::StructField` or `BindOp::EnumPayloadField`) that
/// reads this sub-pattern's value from the outer extraction
/// source; bindings reached through it prepend `prefix` to their
/// chain. Non-binding sub-patterns mint a fresh test block (when
/// any prior sibling test has already been emitted), emit the
/// projection that exposes the sub-pattern's value to its own
/// test logic, then recurse via [`super::lower_pattern_check`]
/// and merge the returned `TestStep`s and `PayloadBind`s.
///
/// Lives here (not in `enums`) because both struct field walks
/// and enum tuple / struct payload walks share the same merge
/// discipline. `extraction_source` is the value `prefix` reads
/// from — the outer subject for plain structs, the enum payload
/// value for enum-tuple/-struct variants.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_subpattern_into(
    pattern: &Pattern,
    sub_resolved_ty: &ResolvedType,
    sub_ir_type: &IRType,
    extraction_source: ValueId,
    prefix: BindStep,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    current_block: &mut IRBlockId,
    steps: &mut Vec<TestStep>,
    binds: &mut Vec<PayloadBind>,
    output: &mut LowerOutput,
) {
    match pattern {
        Pattern::Wildcard { .. } => {}
        Pattern::Binding { local_id, name, .. } => {
            let ir_local = require_local(*local_id, name);
            ensure_local_declared(ir_local, sub_ir_type, ctx);
            binds.push(PayloadBind {
                local: ir_local,
                chain: vec![prefix],
            });
        }
        _ if is_static_catch_all(pattern) => {
            // The inner pattern is structurally a catch-all
            // (bindings / wildcards / catch-all-only nested
            // structs). Bypass the projection-and-mint dance and
            // emit one chained `PayloadBind` per binding leaf;
            // the projection then happens at the success block
            // through the bind chain.
            let mut chain = vec![prefix];
            collect_catch_all_binds(
                pattern,
                sub_resolved_ty,
                sub_ir_type,
                &mut chain,
                inputs,
                ctx,
                binds,
                output,
            );
        }
        _ => {
            if !steps.is_empty() {
                *current_block = ctx.fresh_block("match_and_field");
            }
            let sub_value = emit_subpattern_projection(
                extraction_source,
                &prefix,
                sub_ir_type,
                ctx,
                *current_block,
            );
            let nested_inputs = PatternInputs {
                registry: inputs.registry,
                subject: sub_value,
                subject_ty: sub_resolved_ty,
            };
            let result = lower_pattern_check(pattern, nested_inputs, ctx, *current_block, output);
            let Ok((inner_check, after_block)) = result else {
                return;
            };
            *current_block = after_block;
            consume_inner_check(inner_check, &prefix, steps, binds);
        }
    }
}

/// True when `pattern` would lower to a [`PatternCheck::CatchAll`]
/// regardless of subject type: wildcards, bindings, and struct
/// destructures whose every listed field is itself a static
/// catch-all. Used by [`lower_subpattern_into`] to skip the
/// projection-and-block-mint dance for nested patterns that
/// contribute no runtime predicates.
fn is_static_catch_all(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Wildcard { .. } | Pattern::Binding { .. } => true,
        Pattern::Struct { fields, .. } => fields.iter().all(|f| is_static_catch_all(&f.pattern)),
        _ => false,
    }
}

/// Walk a static-catch-all `pattern` and append one chained
/// [`PayloadBind`] per binding leaf. The caller threads `chain`
/// with the outer prefix already pushed; this helper appends each
/// nested struct field's [`BindOp::StructField`] before recursing
/// and pops on the way out, so siblings see clean state.
/// Wildcards and binding-less destructures contribute nothing.
#[allow(clippy::too_many_arguments)]
fn collect_catch_all_binds(
    pattern: &Pattern,
    sub_resolved_ty: &ResolvedType,
    sub_ir_type: &IRType,
    chain: &mut Vec<BindStep>,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    binds: &mut Vec<PayloadBind>,
    output: &mut LowerOutput,
) {
    match pattern {
        Pattern::Wildcard { .. } => {}
        Pattern::Binding { local_id, name, .. } => {
            let ir_local = require_local(*local_id, name);
            ensure_local_declared(ir_local, sub_ir_type, ctx);
            binds.push(PayloadBind {
                local: ir_local,
                chain: chain.iter().map(clone_bind_step).collect(),
            });
        }
        Pattern::Struct { fields, .. } => {
            let definition = super::super::structs::struct_definition_from_resolution(
                sub_resolved_ty,
                inputs.registry,
                "struct pattern",
            );
            let struct_symbol = super::super::structs::resolved_struct_symbol(
                sub_resolved_ty,
                inputs.registry,
                &mut output.instantiations,
            );
            let owner = match sub_resolved_ty {
                ResolvedType::Named {
                    resolution: Resolution::Global(id),
                    ..
                } => *id,
                _ => panic!(
                    "alpha IR lower: nested struct pattern subject has non-Global \
                     resolution after typecheck seal",
                ),
            };
            let nested_inputs = PatternInputs {
                registry: inputs.registry,
                subject: inputs.subject,
                subject_ty: sub_resolved_ty,
            };
            for field in fields {
                let (field_index, declared) =
                    definition.lookup_field(&field.name).unwrap_or_else(|| {
                        panic!(
                            "alpha IR lower: nested struct pattern references unknown \
                             field `{}` — typecheck invariant violation",
                            field.name,
                        )
                    });
                let (field_resolved_ty, field_ir_type) =
                    super::field_type_for(&declared.ty, owner, &nested_inputs, output);
                chain.push(BindStep {
                    op: BindOp::StructField {
                        field_index,
                        struct_symbol: struct_symbol.clone(),
                    },
                    output_type: field_ir_type.clone(),
                });
                collect_catch_all_binds(
                    &field.pattern,
                    &field_resolved_ty,
                    &field_ir_type,
                    chain,
                    &nested_inputs,
                    ctx,
                    binds,
                    output,
                );
                chain.pop();
            }
        }
        _ => panic!(
            "alpha IR lower: collect_catch_all_binds reached a non-catch-all pattern \
             — caller must gate via is_static_catch_all",
        ),
    }
}

/// Emit the single IR instruction described by `prefix.op` reading
/// from `source` into `block`. Returns the fresh `ValueId` holding
/// the sub-pattern's input. Used to surface a field value (or an
/// enum payload value) so a nested non-binding pattern can test
/// against it.
fn emit_subpattern_projection(
    source: ValueId,
    prefix: &BindStep,
    sub_ir_type: &IRType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let dest = ctx.fresh_value(sub_ir_type.clone());
    match &prefix.op {
        BindOp::StructField {
            field_index,
            struct_symbol,
        } => {
            ctx.cfg.append(
                block,
                IRInstruction::FieldGet {
                    base: source,
                    dest,
                    field_index: *field_index,
                    field_type: sub_ir_type.clone(),
                    struct_symbol: struct_symbol.clone(),
                },
            );
        }
        BindOp::EnumPayloadField {
            enum_symbol,
            payload_index,
            tag,
        } => {
            ctx.cfg.append(
                block,
                IRInstruction::EnumPayloadFieldGet {
                    dest,
                    field_type: sub_ir_type.clone(),
                    payload_index: *payload_index,
                    tag: *tag,
                    ty: enum_symbol.clone(),
                    value: source,
                },
            );
        }
        BindOp::UnionPayload {
            member_index,
            member_type,
            union_type,
        } => {
            ctx.cfg.append(
                block,
                IRInstruction::UnionPayloadGet {
                    dest,
                    member_index: *member_index,
                    member_type: member_type.clone(),
                    ty: union_type.clone(),
                    value: source,
                },
            );
        }
    }
    dest
}

/// Merge a nested pattern's [`PatternCheck`] into the outer
/// chain: prepend `prefix` to every nested [`PayloadBind`] (so it
/// reads through the outer projection), and concatenate the
/// nested [`TestStep`]s under the outer [`ChainMode::And`]
/// discipline. Typecheck restricts nested or-patterns to literal /
/// EnumUnit alternatives that produce no binds and a single
/// Or-mode step; lifting them into an And-chain would require
/// re-coding the wiring, so the lowerer panics if it reaches
/// here. (The fixtures gate currently rejects any nested
/// or-pattern inside struct/enum fields.)
pub(super) fn consume_inner_check(
    inner: PatternCheck,
    prefix: &BindStep,
    steps: &mut Vec<TestStep>,
    binds: &mut Vec<PayloadBind>,
) {
    match inner {
        PatternCheck::CatchAll { binds: inner_binds } => {
            for bind in inner_binds {
                binds.push(prepend_step(prefix, bind));
            }
        }
        PatternCheck::Tests {
            chain_mode,
            payload_binds,
            steps: inner_steps,
        } => {
            assert!(
                matches!(chain_mode, ChainMode::And),
                "alpha IR lower: nested pattern inside a struct/enum field produced an \
                 Or-chained check — typecheck-resolve admits only And-shaped nested \
                 patterns here",
            );
            for bind in payload_binds {
                binds.push(prepend_step(prefix, bind));
            }
            steps.extend(inner_steps);
        }
    }
}

fn prepend_step(prefix: &BindStep, mut bind: PayloadBind) -> PayloadBind {
    bind.chain.insert(0, clone_bind_step(prefix));
    bind
}

pub(super) fn clone_bind_step(step: &BindStep) -> BindStep {
    BindStep {
        op: clone_bind_op(&step.op),
        output_type: step.output_type.clone(),
    }
}

fn clone_bind_op(op: &BindOp) -> BindOp {
    match op {
        BindOp::EnumPayloadField {
            enum_symbol,
            payload_index,
            tag,
        } => BindOp::EnumPayloadField {
            enum_symbol: enum_symbol.clone(),
            payload_index: *payload_index,
            tag: *tag,
        },
        BindOp::StructField {
            field_index,
            struct_symbol,
        } => BindOp::StructField {
            field_index: *field_index,
            struct_symbol: struct_symbol.clone(),
        },
        BindOp::UnionPayload {
            member_index,
            member_type,
            union_type,
        } => BindOp::UnionPayload {
            member_index: *member_index,
            member_type: member_type.clone(),
            union_type: union_type.clone(),
        },
    }
}
