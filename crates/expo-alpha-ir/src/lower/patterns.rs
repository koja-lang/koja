//! Pattern lowering: walk a [`Pattern`] against a subject `ValueId`
//! and produce a [`PatternCheck`] describing whether the arm fires
//! unconditionally, after one or more chained predicates, and what
//! field binds (`EnumPayloadFieldGet` / `FieldGet` + `LocalWrite`)
//! the success edge needs to perform before the arm body runs. The
//! match driver in [`super::match_expr`] consumes the result to
//! wire the gating `CondBranch`(es) and the per-arm body block.
//!
//! Admits leaves (wildcard / binding / literal), `EnumUnit`,
//! `EnumTuple` / `EnumStruct` (one-level — payload elements / fields
//! restricted to wildcard / binding), `Struct` (same restriction —
//! always a `CatchAll` carrying field binds), and `Or` (alternatives
//! restricted to literal / EnumUnit, no bindings). Every other
//! shape is a feature gap diagnosed in typecheck and is unreachable
//! on the success path.

use expo_alpha_typecheck::{GlobalRegistry, ResolvedVariantData};
use expo_ast::ast::{Diagnostic, FieldPattern, Pattern};
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
use super::structs::{resolved_struct_symbol, struct_definition_from_resolution};

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
    /// Pattern fires unconditionally (wildcard / binding / struct
    /// destructure). `binds` carries any field-extraction binds the
    /// driver must emit at the head of the success block before
    /// running the guard / body. Wildcard and binding always have
    /// empty binds; struct destructure carries one entry per named
    /// `Pattern::Binding` field.
    CatchAll { binds: Vec<PayloadBind> },
    /// One or more chained predicates. Length 1 for a single
    /// `Literal` / `EnumUnit` / `EnumTuple` / `EnumStruct` pattern;
    /// length n for an `Or` of n alternatives. The driver wires
    /// every step's success edge to the same success block, every
    /// interior step's failure edge to the next step's
    /// `test_block`, and the last step's failure edge to the
    /// caller-supplied fall-through.
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

/// One field binding emitted on the success edge. The driver
/// appends the right `*FieldGet` + `LocalWrite` pair to the head
/// of the success block before the arm body runs. Source variant
/// drives instruction selection.
pub(super) struct PayloadBind {
    pub(super) field_type: IRType,
    pub(super) local: IRLocalId,
    pub(super) source: BindSource,
}

/// Where a [`PayloadBind`] reads its value from. `EnumPayload`
/// emits `EnumPayloadFieldGet` (tag-gated extraction); `StructField`
/// emits `FieldGet` (no tag — the subject is already a struct).
pub(super) enum BindSource {
    EnumPayload {
        enum_symbol: IRSymbol,
        payload_index: u32,
        tag: IRVariantTag,
    },
    StructField {
        field_index: u32,
        struct_symbol: IRSymbol,
    },
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
            Ok((PatternCheck::CatchAll { binds: Vec::new() }, block))
        }
        Pattern::EnumStruct {
            fields,
            type_path: _,
            variant,
            ..
        } => lower_enum_struct_check(variant, fields, &inputs, ctx, block, output),
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
        Pattern::Struct { fields, .. } => {
            Ok((lower_struct_check(fields, &inputs, ctx, output), block))
        }
        Pattern::Wildcard { .. } => Ok((PatternCheck::CatchAll { binds: Vec::new() }, block)),
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

fn lower_enum_struct_check(
    variant_name: &str,
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let cond = emit_enum_tag_eq(variant_name, inputs, ctx, block, output);
    let payload_binds = build_enum_struct_binds(variant_name, fields, inputs, ctx, output);
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

fn lower_enum_tuple_check(
    variant_name: &str,
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let cond = emit_enum_tag_eq(variant_name, inputs, ctx, block, output);
    let payload_binds = build_enum_tuple_binds(variant_name, elements, inputs, ctx, output);
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

/// Plain-struct pattern. Always a [`PatternCheck::CatchAll`] — the
/// IR emits no tag check, only the per-field
/// [`BindSource::StructField`] binds for named [`Pattern::Binding`]
/// fields. Wildcards are skipped.
fn lower_struct_check(
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> PatternCheck {
    let binds = build_struct_field_binds(fields, inputs, ctx, output);
    PatternCheck::CatchAll { binds }
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
fn build_enum_tuple_binds(
    variant_name: &str,
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> Vec<PayloadBind> {
    let metadata = enum_pattern_metadata(variant_name, inputs, output);
    let ResolvedVariantData::Tuple(declared_payload) = metadata.variant_data else {
        panic!(
            "alpha IR lower: enum tuple pattern `{}.{variant_name}` targets a \
             non-tuple variant — typecheck invariant violation",
            metadata.label,
        );
    };
    let mut binds = Vec::new();
    for (payload_index, (element, declared_ty)) in
        elements.iter().zip(declared_payload.iter()).enumerate()
    {
        let Pattern::Binding { local_id, name, .. } = element else {
            continue;
        };
        let ir_local = require_local(*local_id, name);
        let field_type = field_type_for(declared_ty, metadata.owner, inputs, output);
        ensure_local_declared(ir_local, &field_type, ctx);
        binds.push(PayloadBind {
            field_type,
            local: ir_local,
            source: BindSource::EnumPayload {
                enum_symbol: metadata.enum_symbol.clone(),
                payload_index: payload_index as u32,
                tag: metadata.tag,
            },
        });
    }
    binds
}

/// Build the bind list for an `EnumStruct` pattern. Looks each
/// surface field up by name on the variant's declared field
/// roster; only `Pattern::Binding` field patterns produce binds.
/// The `payload_index` is the field's declaration-order position,
/// matching what [`crate::seal::enums::seal_payload_field_index`]
/// expects.
fn build_enum_struct_binds(
    variant_name: &str,
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> Vec<PayloadBind> {
    let metadata = enum_pattern_metadata(variant_name, inputs, output);
    let ResolvedVariantData::Struct(declared_fields) = metadata.variant_data else {
        panic!(
            "alpha IR lower: enum struct pattern `{}.{variant_name}` targets a \
             non-struct variant — typecheck invariant violation",
            metadata.label,
        );
    };
    let mut binds = Vec::new();
    for field in fields {
        let Pattern::Binding { local_id, name, .. } = &field.pattern else {
            continue;
        };
        let (payload_index, declared) = declared_fields
            .iter()
            .enumerate()
            .find(|(_, decl)| decl.name == field.name)
            .unwrap_or_else(|| {
                panic!(
                    "alpha IR lower: enum struct pattern `{}.{variant_name}.{name}` \
                     references unknown field — typecheck invariant violation",
                    metadata.label,
                    name = field.name,
                )
            });
        let ir_local = require_local(*local_id, name);
        let field_type = field_type_for(&declared.ty, metadata.owner, inputs, output);
        ensure_local_declared(ir_local, &field_type, ctx);
        binds.push(PayloadBind {
            field_type,
            local: ir_local,
            source: BindSource::EnumPayload {
                enum_symbol: metadata.enum_symbol.clone(),
                payload_index: payload_index as u32,
                tag: metadata.tag,
            },
        });
    }
    binds
}

/// Build the bind list for a plain-struct pattern. Looks each
/// surface field up by name on the struct's declared field
/// roster; only `Pattern::Binding` field patterns produce binds.
/// Wildcards are skipped, mirroring the enum-payload path.
fn build_struct_field_binds(
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> Vec<PayloadBind> {
    let definition =
        struct_definition_from_resolution(inputs.subject_ty, inputs.registry, "struct pattern");
    let struct_symbol = resolved_struct_symbol(
        inputs.subject_ty,
        inputs.registry,
        &mut output.instantiations,
    );
    let owner = match inputs.subject_ty.resolution {
        Resolution::Global(id) => id,
        _ => panic!(
            "alpha IR lower: struct pattern subject has non-Global resolution after \
             typecheck seal",
        ),
    };
    let mut binds = Vec::new();
    for field in fields {
        let Pattern::Binding { local_id, name, .. } = &field.pattern else {
            continue;
        };
        let (field_index, declared) = definition.lookup_field(&field.name).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: struct pattern references unknown field `{}` — \
                     typecheck invariant violation",
                field.name,
            )
        });
        let ir_local = require_local(*local_id, name);
        let field_type = field_type_for(&declared.ty, owner, inputs, output);
        ensure_local_declared(ir_local, &field_type, ctx);
        binds.push(PayloadBind {
            field_type,
            local: ir_local,
            source: BindSource::StructField {
                field_index,
                struct_symbol: struct_symbol.clone(),
            },
        });
    }
    binds
}

struct EnumPatternMetadata<'a> {
    enum_symbol: IRSymbol,
    label: String,
    owner: expo_ast::identifier::GlobalRegistryId,
    tag: IRVariantTag,
    variant_data: &'a ResolvedVariantData,
}

/// Resolve everything every enum-payload bind helper needs from the
/// subject + variant name: registry entry, mangled symbol, tag,
/// owner-id, and a borrowed view of the declared payload shape.
fn enum_pattern_metadata<'a>(
    variant_name: &str,
    inputs: &'a PatternInputs<'_>,
    output: &mut LowerOutput,
) -> EnumPatternMetadata<'a> {
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
    let owner = match inputs.subject_ty.resolution {
        Resolution::Global(id) => id,
        _ => panic!("alpha IR lower: enum subject has non-Global resolution after typecheck seal",),
    };
    EnumPatternMetadata {
        enum_symbol,
        label: entry.identifier.to_string(),
        owner,
        tag: IRVariantTag(variant_index as u8),
        variant_data: &variant.data,
    }
}

fn require_local(local_id: Option<expo_ast::identifier::LocalId>, name: &str) -> IRLocalId {
    let id = local_id.unwrap_or_else(|| {
        panic!(
            "alpha IR lower: pattern binding `{name}` reaches lower without a \
             stamped LocalId — typecheck-resolve invariant violation",
        );
    });
    IRLocalId::from_local_id(id)
}

fn field_type_for(
    declared_ty: &ResolvedType,
    owner: expo_ast::identifier::GlobalRegistryId,
    inputs: &PatternInputs<'_>,
    output: &mut LowerOutput,
) -> IRType {
    let substituted = substitute_resolved_type(declared_ty, &inputs.subject_ty.type_args, owner);
    resolved_type_to_ir_type(&substituted, inputs.registry, &mut output.instantiations)
}

fn ensure_local_declared(local: IRLocalId, ty: &IRType, ctx: &mut FnLowerCtx) {
    if ctx.local_is_declared(local) {
        return;
    }
    let entry_block = ctx.entry_block();
    ctx.cfg.append(
        entry_block,
        IRInstruction::LocalDecl {
            local,
            ty: ty.clone(),
        },
    );
    ctx.mark_local_declared(local);
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
