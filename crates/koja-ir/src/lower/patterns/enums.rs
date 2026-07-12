//! Enum-flavored pattern lowering: `EnumUnit`, `EnumTuple`, and
//! `EnumStruct`. The unit case lives inline in the dispatcher
//! ([`super::lower_pattern_check`]) since it's just an
//! [`emit_enum_tag_eq`] + [`super::single_test`] pair; the tuple
//! and struct cases own their own payload walkers that share the
//! [`enum_pattern_metadata`] resolver and the cross-shape
//! [`super::structs::lower_subpattern_into`] merge discipline.
//!
//! Tuple / struct variants with non-binding payload elements
//! emit an outer tag-test step followed by AND-chained payload
//! tests. Each payload test executes in a fresh block dominated
//! by the tag-check success edge, so the `EnumPayloadFieldGet`
//! projection is safe.

use koja_ast::ast::{FieldPattern, Pattern};
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use koja_typecheck::ResolvedVariantData;

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::enums::{
    enum_definition_from_entry, enum_entry_from_resolution, resolved_enum_symbol,
};
use super::structs::lower_subpattern_into;
use super::{BindOp, BindStep, ChainMode, PatternCheck, PatternInputs, PayloadBind, TestStep};
use crate::enum_decl::IRVariantTag;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::types::{ConstValue, IRBinOp, IRType, ValueId};

pub(super) fn lower_enum_struct_check(
    variant_name: &str,
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let metadata = enum_pattern_metadata(variant_name, inputs, output);
    let ResolvedVariantData::Struct(declared_fields) = metadata.variant_data else {
        panic!(
            "IR lower: enum struct pattern `{}.{variant_name}` targets a \
             non-struct variant — typecheck invariant violation",
            metadata.label,
        );
    };
    let tag_cond = emit_enum_tag_eq_with(&metadata, inputs.subject, ctx, block);
    let tag_step = TestStep {
        cond: tag_cond,
        test_block: block,
    };

    let mut steps = vec![tag_step];
    let mut binds = Vec::new();
    let mut current_block = block;

    walk_enum_struct_fields(
        fields,
        declared_fields,
        &metadata,
        inputs,
        ctx,
        &mut current_block,
        &mut steps,
        &mut binds,
        output,
    );

    Ok((
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: binds,
            steps,
        },
        current_block,
    ))
}

pub(super) fn lower_enum_tuple_check(
    variant_name: &str,
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let metadata = enum_pattern_metadata(variant_name, inputs, output);
    let ResolvedVariantData::Tuple(declared_payload) = metadata.variant_data else {
        panic!(
            "IR lower: enum tuple pattern `{}.{variant_name}` targets a \
             non-tuple variant — typecheck invariant violation",
            metadata.label,
        );
    };
    let tag_cond = emit_enum_tag_eq_with(&metadata, inputs.subject, ctx, block);
    let tag_step = TestStep {
        cond: tag_cond,
        test_block: block,
    };

    let mut steps = vec![tag_step];
    let mut binds = Vec::new();
    let mut current_block = block;

    walk_enum_tuple_elements(
        elements,
        declared_payload,
        &metadata,
        inputs,
        ctx,
        &mut current_block,
        &mut steps,
        &mut binds,
        output,
    );

    Ok((
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: binds,
            steps,
        },
        current_block,
    ))
}

/// Emit `EnumTagGet(subject) == const(tag)` into `block` and return
/// the resulting `Bool` value. Used by the dispatcher's
/// [`Pattern::EnumUnit`] arm to assemble a single-step
/// [`PatternCheck::Tests`]. The dispatcher routes through
/// [`super::single_test`] so no payload extraction happens for
/// unit-variant patterns.
pub(super) fn emit_enum_tag_eq(
    variant_name: &str,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> ValueId {
    let metadata = enum_pattern_metadata(variant_name, inputs, output);
    emit_enum_tag_eq_with(&metadata, inputs.subject, ctx, block)
}

fn emit_enum_tag_eq_with(
    metadata: &EnumPatternMetadata<'_>,
    subject: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let tag_value = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::EnumTagGet {
            dest: tag_value,
            value: subject,
            ty: metadata.enum_symbol.clone(),
        },
    );
    let const_dest = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: const_dest,
            value: ConstValue::Int8(metadata.tag.0 as i8),
        },
    );
    let cond = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest: cond,
            lhs: tag_value,
            op: IRBinOp::Eq,
            operand_ty: IRType::Int8,
            rhs: const_dest,
        },
    );
    cond
}

#[allow(clippy::too_many_arguments)]
fn walk_enum_tuple_elements(
    elements: &[Pattern],
    declared_payload: &[ResolvedType],
    metadata: &EnumPatternMetadata<'_>,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    current_block: &mut IRBlockId,
    steps: &mut Vec<TestStep>,
    binds: &mut Vec<PayloadBind>,
    output: &mut LowerOutput,
) {
    for (payload_index, (element, declared_ty)) in
        elements.iter().zip(declared_payload.iter()).enumerate()
    {
        let (element_resolved_ty, element_ir_type) =
            super::field_type_for(declared_ty, metadata.owner, inputs, output);
        let prefix = BindStep {
            op: BindOp::EnumPayloadField {
                enum_symbol: metadata.enum_symbol.clone(),
                payload_index: payload_index as u32,
                tag: metadata.tag,
            },
            output_type: element_ir_type.clone(),
        };
        lower_subpattern_into(
            element,
            &element_resolved_ty,
            &element_ir_type,
            inputs.subject,
            prefix,
            inputs,
            ctx,
            current_block,
            steps,
            binds,
            output,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_enum_struct_fields(
    fields: &[FieldPattern],
    declared_fields: &[koja_typecheck::ResolvedStructField],
    metadata: &EnumPatternMetadata<'_>,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    current_block: &mut IRBlockId,
    steps: &mut Vec<TestStep>,
    binds: &mut Vec<PayloadBind>,
    output: &mut LowerOutput,
) {
    for field in fields {
        let (payload_index, declared) = declared_fields
            .iter()
            .enumerate()
            .find(|(_, decl)| decl.name == field.name)
            .unwrap_or_else(|| {
                panic!(
                    "IR lower: enum struct pattern `{}.{variant}.{name}` references \
                     unknown field — typecheck invariant violation",
                    metadata.label,
                    variant = metadata.variant_name(),
                    name = field.name,
                )
            });
        let (field_resolved_ty, field_ir_type) =
            super::field_type_for(&declared.ty, metadata.owner, inputs, output);
        let prefix = BindStep {
            op: BindOp::EnumPayloadField {
                enum_symbol: metadata.enum_symbol.clone(),
                payload_index: payload_index as u32,
                tag: metadata.tag,
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
            current_block,
            steps,
            binds,
            output,
        );
    }
}

struct EnumPatternMetadata<'a> {
    enum_symbol: IRSymbol,
    label: String,
    owner: GlobalRegistryId,
    tag: IRVariantTag,
    variant: &'a koja_typecheck::ResolvedEnumVariant,
    variant_data: &'a ResolvedVariantData,
}

impl EnumPatternMetadata<'_> {
    fn variant_name(&self) -> &str {
        &self.variant.name
    }
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
            "IR lower: enum `{}` has no variant `{variant_name}` — \
             typecheck invariant violation",
            entry.identifier,
        )
    });
    let owner = match inputs.subject_ty {
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => *id,
        _ => panic!("IR lower: enum subject has non-Global resolution after typecheck seal",),
    };
    EnumPatternMetadata {
        enum_symbol,
        label: entry.identifier.to_string(),
        owner,
        tag: IRVariantTag(variant_index as u8),
        variant,
        variant_data: &variant.data,
    }
}
