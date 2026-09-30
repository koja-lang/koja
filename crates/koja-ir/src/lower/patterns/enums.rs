//! Enum-flavored pattern lowering: `EnumUnit`, `EnumTuple`, and
//! `EnumStruct`. The unit case lives inline in the dispatcher
//! ([`super::lower_pattern_check`]) since it's just an
//! [`emit_enum_tag_eq`] + [`super::single_test`] pair. The tuple
//! and struct cases differ only in how a sub-pattern finds its
//! payload slot (by position or by field name). Each builds a
//! [`PayloadSlot`] list and hands it to the shared
//! [`lower_enum_payload_check`], which follows the cross-shape
//! [`super::structs::lower_subpattern_into`] merge discipline.
//!
//! Tuple / struct variants with non-binding payload elements
//! emit an outer tag-test step followed by AND-chained payload
//! tests. Each payload test executes in a fresh block dominated
//! by the tag-check success edge, so the `EnumPayloadFieldGet`
//! projection is safe.

use koja_ast::ast::{FieldPattern, Name, Pattern};
use koja_ast::identifier::{GlobalRegistryId, ResolvedType};
use koja_typecheck::ResolvedVariantData;

use super::super::arms::emit_tag_eq;
use super::super::ctx::FnLowerCtx;
use super::super::enums::{
    enum_definition_from_entry, enum_entry_from_resolution, resolved_enum_symbol,
};
use super::structs::lower_subpattern_into;
use super::{
    BindOp, BindStep, ChainMode, PatternCheck, PatternInputs, PayloadBind, TestStep, global_id_of,
};
use crate::enum_decl::IRVariantTag;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::types::{IRType, ValueId};

pub(super) fn lower_enum_struct_check(
    variant_name: &Name,
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let metadata = enum_pattern_metadata(variant_name, inputs, ctx);
    let ResolvedVariantData::Struct(declared_fields) = metadata.variant_data else {
        panic!(
            "IR lower: enum struct pattern `{}.{variant_name}` targets a \
             non-struct variant (typecheck invariant violation)",
            metadata.label,
        );
    };
    let payloads = named_payloads(fields, declared_fields, &metadata);
    Ok(lower_enum_payload_check(
        &metadata, &payloads, inputs, ctx, block,
    ))
}

pub(super) fn lower_enum_tuple_check(
    variant_name: &Name,
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let metadata = enum_pattern_metadata(variant_name, inputs, ctx);
    let ResolvedVariantData::Tuple(declared_payload) = metadata.variant_data else {
        panic!(
            "IR lower: enum tuple pattern `{}.{variant_name}` targets a \
             non-tuple variant (typecheck invariant violation)",
            metadata.label,
        );
    };
    let payloads = positional_payloads(elements, declared_payload);
    Ok(lower_enum_payload_check(
        &metadata, &payloads, inputs, ctx, block,
    ))
}

/// One payload position a sub-pattern tests or binds: its index in
/// the variant's payload, its declared type, and the sub-pattern.
type PayloadSlot<'a> = (u32, &'a ResolvedType, &'a Pattern);

/// Tuple variant adapter: sub-patterns pair with payload slots by
/// position.
fn positional_payloads<'a>(
    elements: &'a [Pattern],
    declared_payload: &'a [ResolvedType],
) -> Vec<PayloadSlot<'a>> {
    elements
        .iter()
        .zip(declared_payload)
        .enumerate()
        .map(|(index, (pattern, declared_ty))| (index as u32, declared_ty, pattern))
        .collect()
}

/// Struct variant adapter: each field pattern finds its payload
/// slot by name. Fields the pattern omits contribute nothing.
fn named_payloads<'a>(
    fields: &'a [FieldPattern],
    declared_fields: &'a [koja_typecheck::ResolvedStructField],
    metadata: &EnumPatternMetadata<'_>,
) -> Vec<PayloadSlot<'a>> {
    fields
        .iter()
        .map(|field| {
            let (payload_index, declared) = declared_fields
                .iter()
                .enumerate()
                .find(|(_, decl)| decl.name == field.name.text)
                .unwrap_or_else(|| {
                    panic!(
                        "IR lower: enum struct pattern `{}.{variant}.{name}` references \
                         unknown field (typecheck invariant violation)",
                        metadata.label,
                        variant = metadata.variant_name(),
                        name = field.name,
                    )
                });
            (payload_index as u32, &declared.ty, &field.pattern)
        })
        .collect()
}

/// The tag test in `block`, then every payload slot's sub-pattern
/// AND-chained behind it.
fn lower_enum_payload_check(
    metadata: &EnumPatternMetadata<'_>,
    payloads: &[PayloadSlot<'_>],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> (PatternCheck, IRBlockId) {
    let tag_cond = emit_enum_tag_eq_with(metadata, inputs.subject, ctx, block);
    let mut steps = vec![TestStep {
        cond: tag_cond,
        test_block: block,
    }];
    let mut binds = Vec::new();
    let mut current_block = block;
    walk_enum_payload(
        payloads,
        metadata,
        inputs,
        ctx,
        &mut current_block,
        &mut steps,
        &mut binds,
    );
    (
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: binds,
            steps,
        },
        current_block,
    )
}

/// Emit `EnumTagGet(subject) == const(tag)` into `block` and return
/// the resulting `Bool` value. Used by the dispatcher's
/// [`Pattern::EnumUnit`] arm to assemble a single-step
/// [`PatternCheck::Tests`]. The dispatcher routes through
/// [`super::single_test`] so no payload extraction happens for
/// unit-variant patterns.
pub(super) fn emit_enum_tag_eq(
    variant_name: &Name,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> ValueId {
    let metadata = enum_pattern_metadata(variant_name, inputs, ctx);
    emit_enum_tag_eq_with(&metadata, inputs.subject, ctx, block)
}

fn emit_enum_tag_eq_with(
    metadata: &EnumPatternMetadata<'_>,
    subject: ValueId,
    ctx: &mut FnLowerCtx<'_>,
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
    emit_tag_eq(tag_value, metadata.tag.0, ctx, block)
}

/// Lower every payload slot's sub-pattern into the outer chain,
/// each reading its value through an `EnumPayloadField` step.
fn walk_enum_payload(
    payloads: &[PayloadSlot<'_>],
    metadata: &EnumPatternMetadata<'_>,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    current_block: &mut IRBlockId,
    steps: &mut Vec<TestStep>,
    binds: &mut Vec<PayloadBind>,
) {
    for &(payload_index, declared_ty, pattern) in payloads {
        let (resolved_ty, ir_type) =
            super::field_type_for(declared_ty, metadata.owner, inputs, ctx);
        let prefix = BindStep {
            op: BindOp::EnumPayloadField {
                enum_symbol: metadata.enum_symbol.clone(),
                payload_index,
                tag: metadata.tag,
            },
            output_type: ir_type.clone(),
        };
        lower_subpattern_into(
            pattern,
            &resolved_ty,
            &ir_type,
            inputs.subject,
            prefix,
            inputs,
            ctx,
            current_block,
            steps,
            binds,
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
fn enum_pattern_metadata<'r>(
    variant_name: &Name,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'r>,
) -> EnumPatternMetadata<'r> {
    let entry = enum_entry_from_resolution(inputs.subject_ty, ctx.registry);
    let definition = enum_definition_from_entry(entry);
    let enum_symbol = resolved_enum_symbol(
        inputs.subject_ty,
        ctx.registry,
        &mut ctx.output.instantiations,
    );
    let (variant_index, variant) = definition
        .lookup_variant(variant_name.as_str())
        .unwrap_or_else(|| {
            panic!(
                "IR lower: enum `{}` has no variant `{variant_name}` \
             (typecheck invariant violation)",
                entry.identifier,
            )
        });
    let owner = global_id_of(inputs.subject_ty, "enum subject");
    EnumPatternMetadata {
        enum_symbol,
        label: entry.identifier.to_string(),
        owner,
        tag: IRVariantTag(variant_index as u8),
        variant,
        variant_data: &variant.data,
    }
}
