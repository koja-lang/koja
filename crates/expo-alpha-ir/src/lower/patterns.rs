//! Pattern lowering: walk a [`Pattern`] against a subject `ValueId`
//! and produce a [`PatternCheck`] describing whether the arm fires
//! unconditionally, after one or more chained predicates, and what
//! payload binds (`EnumPayloadFieldGet` + `LocalWrite`) the success
//! edge needs to perform before the arm body runs. The match driver
//! in [`super::match_expr`] consumes the result to wire the gating
//! `CondBranch`(es) and the per-arm body block.
//!
//! Admits leaves (wildcard / binding / literal), `EnumUnit`,
//! `EnumTuple` (one-level — payload elements restricted to wildcard
//! / binding), and `Or` (alternatives restricted to literal /
//! EnumUnit, no bindings). Every other shape is a feature gap
//! diagnosed in typecheck and is unreachable on the success path.

use expo_alpha_typecheck::{GlobalRegistry, ResolvedVariantData};
use expo_ast::ast::{Diagnostic, Pattern};
use expo_ast::identifier::{Resolution, ResolvedType};
use expo_ast::labels::{pattern_kind_label, pattern_span};

use crate::enum_decl::IRVariantTag;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::substitute_resolved_type;
use crate::local::IRLocalId;
use crate::types::{ConstValue, IRBinOp, IRType, ValueId};

use super::arms::lower_result_ty;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::enums::{enum_definition_from_entry, enum_entry_from_resolution, resolved_enum_symbol};
use super::ops::{const_value_type, lower_literal};
use super::package::resolved_type_to_ir_type;

/// Read-only inputs threaded through every recursive helper.
/// Bundling them keeps `lower_pattern_check` and its per-shape
/// helpers under the clippy `too_many_arguments` threshold.
pub(super) struct PatternInputs<'a> {
    pub(super) registry: &'a GlobalRegistry,
    pub(super) subject: ValueId,
    pub(super) subject_ty: &'a ResolvedType,
}

/// What the `match` driver needs to wire after lowering one arm's
/// pattern against the subject.
pub(super) enum PatternCheck {
    /// `_` / binding — fires unconditionally. Any side effects
    /// (binding `LocalWrite`) were emitted into the input block.
    CatchAll,
    /// One or more chained predicates. Length 1 for a single
    /// `Literal` / `EnumUnit` / `EnumTuple` pattern; length n for
    /// an `Or` of n alternatives. The driver wires every step's
    /// success edge to the same body block, every interior step's
    /// failure edge to the next step's `test_block`, and the last
    /// step's failure edge to the caller-supplied fall-through.
    Tests {
        payload_binds: Vec<PayloadBind>,
        steps: Vec<TestStep>,
    },
}

/// One predicate gating arm execution. The test instructions are
/// already emitted into `test_block`; the driver sets that block's
/// terminator to a `CondBranch` keyed on `cond`.
pub(super) struct TestStep {
    pub(super) cond: ValueId,
    pub(super) test_block: IRBlockId,
}

/// One enum-payload field binding emitted on a tag-matched success
/// edge. The driver appends `EnumPayloadFieldGet` + `LocalWrite` to
/// the head of the body block before the arm body runs.
pub(super) struct PayloadBind {
    pub(super) enum_symbol: IRSymbol,
    pub(super) field_type: IRType,
    pub(super) local: IRLocalId,
    pub(super) payload_index: u32,
    pub(super) tag: IRVariantTag,
}

pub(super) fn lower_pattern_check(
    pattern: &Pattern,
    inputs: PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    match pattern {
        Pattern::Binding { local_id, name, .. } => {
            lower_binding_check(*local_id, name, &inputs, ctx, block, output);
            Ok((PatternCheck::CatchAll, block))
        }
        Pattern::EnumTuple {
            elements,
            type_path: _,
            variant,
            ..
        } => lower_enum_tuple_check(variant, elements, &inputs, ctx, block, output),
        Pattern::EnumUnit { variant, .. } => {
            let cond = emit_enum_tag_eq(variant, &inputs, ctx, block, output);
            Ok(single_test(cond, block))
        }
        Pattern::Literal { span, value } => {
            let const_value = lower_literal(value, *span, &mut output.diagnostics)?;
            let const_ty = const_value_type(&const_value);
            let const_dest = ctx.fresh_value(const_ty.clone());
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest: const_dest,
                    value: const_value,
                },
            );
            let cond = ctx.fresh_value(IRType::Bool);
            ctx.cfg.append(
                block,
                IRInstruction::BinaryOp {
                    dest: cond,
                    lhs: inputs.subject,
                    op: IRBinOp::Eq,
                    rhs: const_dest,
                },
            );
            Ok(single_test(cond, block))
        }
        Pattern::Or { patterns, .. } => Ok(lower_or_check(patterns, &inputs, ctx, block, output)),
        Pattern::Wildcard { .. } => Ok((PatternCheck::CatchAll, block)),
        other => {
            output.diagnostics.push(Diagnostic::error(
                format!(
                    "alpha IR does not yet lower match pattern `{}`",
                    pattern_kind_label(other),
                ),
                pattern_span(other),
            ));
            Err(())
        }
    }
}

fn lower_binding_check(
    local_id: Option<expo_ast::identifier::LocalId>,
    name: &str,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) {
    let id = local_id.unwrap_or_else(|| {
        panic!(
            "alpha IR lower: match binding `{name}` reaches lower without a stamped \
             LocalId — typecheck resolve invariant violation",
        );
    });
    let ir_local = IRLocalId::from_local_id(id);
    if !ctx.local_is_declared(ir_local) {
        let ty = lower_result_ty(inputs.subject_ty, inputs.registry, output);
        let entry = ctx.entry_block();
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty,
            },
        );
        ctx.mark_local_declared(ir_local);
    }
    ctx.cfg.append(
        block,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: inputs.subject,
        },
    );
}

fn lower_enum_tuple_check(
    variant_name: &str,
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let cond = emit_enum_tag_eq(variant_name, inputs, ctx, block, output);
    let payload_binds = build_payload_binds(variant_name, elements, inputs, ctx, output);
    Ok((
        PatternCheck::Tests {
            payload_binds,
            steps: vec![TestStep {
                cond,
                test_block: block,
            }],
        },
        block,
    ))
}

fn lower_or_check(
    alternatives: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> (PatternCheck, IRBlockId) {
    let mut steps = Vec::with_capacity(alternatives.len());
    let mut current = block;
    for (index, alternative) in alternatives.iter().enumerate() {
        if index > 0 {
            current = ctx.fresh_block(format!("match_or_alt_{index}"));
        }
        let cond = emit_or_alternative(alternative, inputs, ctx, current, output);
        steps.push(TestStep {
            cond,
            test_block: current,
        });
    }
    (
        PatternCheck::Tests {
            payload_binds: Vec::new(),
            steps,
        },
        current,
    )
}

/// Or alternatives are restricted by typecheck to `Literal` /
/// `EnumUnit` (no bindings, no nested or-patterns). Anything else
/// reaching here is a typecheck-resolve invariant violation.
fn emit_or_alternative(
    pattern: &Pattern,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> ValueId {
    match pattern {
        Pattern::EnumUnit { variant, .. } => emit_enum_tag_eq(variant, inputs, ctx, block, output),
        Pattern::Literal { span, value } => {
            let const_value = lower_literal(value, *span, &mut output.diagnostics)
                .expect("alpha IR lower: typecheck must have rejected non-lowerable literal");
            let const_ty = const_value_type(&const_value);
            let const_dest = ctx.fresh_value(const_ty);
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest: const_dest,
                    value: const_value,
                },
            );
            let cond = ctx.fresh_value(IRType::Bool);
            ctx.cfg.append(
                block,
                IRInstruction::BinaryOp {
                    dest: cond,
                    lhs: inputs.subject,
                    op: IRBinOp::Eq,
                    rhs: const_dest,
                },
            );
            cond
        }
        other => panic!(
            "alpha IR lower: or-alternative `{}` reached lowering — \
             typecheck-resolve admits only Literal / EnumUnit alternatives",
            pattern_kind_label(other),
        ),
    }
}

/// Emit `EnumTagGet(subject) == const(tag)` into `block` and return
/// the resulting `Bool` value.
fn emit_enum_tag_eq(
    variant_name: &str,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> ValueId {
    let entry = enum_entry_from_resolution(inputs.subject_ty, inputs.registry);
    let definition = enum_definition_from_entry(entry);
    let symbol = resolved_enum_symbol(
        inputs.subject_ty,
        inputs.registry,
        &mut output.instantiations,
    );
    let (variant_index, _) = definition.lookup_variant(variant_name).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: enum `{}` has no variant `{variant_name}` — \
             typecheck seal must have rejected this",
            entry.identifier,
        )
    });
    let tag = IRVariantTag(variant_index as u8);
    let tag_value = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::EnumTagGet {
            dest: tag_value,
            value: inputs.subject,
            ty: symbol,
        },
    );
    let const_dest = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: const_dest,
            value: ConstValue::Int8(tag.0 as i8),
        },
    );
    let cond = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest: cond,
            lhs: tag_value,
            op: IRBinOp::Eq,
            rhs: const_dest,
        },
    );
    cond
}

/// Build the bind list for an `EnumTuple` pattern's payload. Only
/// `Pattern::Binding` elements produce binds; `Pattern::Wildcard`
/// elements are skipped. Typecheck rejects every other element
/// shape, so reaching one here is an invariant violation.
fn build_payload_binds(
    variant_name: &str,
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> Vec<PayloadBind> {
    let entry = enum_entry_from_resolution(inputs.subject_ty, inputs.registry);
    let definition = enum_definition_from_entry(entry);
    let enum_symbol = resolved_enum_symbol(
        inputs.subject_ty,
        inputs.registry,
        &mut output.instantiations,
    );
    let (variant_index, variant) = definition.lookup_variant(variant_name).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: enum `{}` has no variant `{variant_name}` — \
                 typecheck invariant violation",
            entry.identifier,
        )
    });
    let tag = IRVariantTag(variant_index as u8);
    let ResolvedVariantData::Tuple(declared_payload) = &variant.data else {
        panic!(
            "alpha IR lower: enum tuple pattern `{}.{variant_name}` targets a \
             non-tuple variant — typecheck invariant violation",
            entry.identifier,
        );
    };
    let owner = match inputs.subject_ty.resolution {
        Resolution::Global(id) => id,
        _ => panic!("alpha IR lower: enum subject has non-Global resolution after typecheck seal",),
    };
    let mut binds = Vec::new();
    for (payload_index, (element, declared_ty)) in
        elements.iter().zip(declared_payload.iter()).enumerate()
    {
        let Pattern::Binding { local_id, name, .. } = element else {
            continue;
        };
        let id = local_id.unwrap_or_else(|| {
            panic!(
                "alpha IR lower: payload binding `{name}` reaches lower without a \
                 stamped LocalId — typecheck-resolve invariant violation",
            );
        });
        let ir_local = IRLocalId::from_local_id(id);
        let element_ty = substitute_resolved_type(declared_ty, &inputs.subject_ty.type_args, owner);
        let field_type =
            resolved_type_to_ir_type(&element_ty, inputs.registry, &mut output.instantiations);
        if !ctx.local_is_declared(ir_local) {
            let entry_block = ctx.entry_block();
            ctx.cfg.append(
                entry_block,
                IRInstruction::LocalDecl {
                    local: ir_local,
                    ty: field_type.clone(),
                },
            );
            ctx.mark_local_declared(ir_local);
        }
        binds.push(PayloadBind {
            enum_symbol: enum_symbol.clone(),
            field_type,
            local: ir_local,
            payload_index: payload_index as u32,
            tag,
        });
    }
    binds
}

fn single_test(cond: ValueId, block: IRBlockId) -> (PatternCheck, IRBlockId) {
    (
        PatternCheck::Tests {
            payload_binds: Vec::new(),
            steps: vec![TestStep {
                cond,
                test_block: block,
            }],
        },
        block,
    )
}
