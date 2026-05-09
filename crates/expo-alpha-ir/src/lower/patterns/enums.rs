//! Enum-flavored pattern lowering: `EnumUnit`, `EnumTuple`, and
//! `EnumStruct`. The unit case lives inline in the dispatcher
//! ([`super::lower_pattern_check`]) since it's just an
//! [`emit_enum_tag_eq`] + [`super::single_test`] pair; the tuple
//! and struct cases own their own bind-list builders that share
//! the [`enum_pattern_metadata`] resolver.

use expo_alpha_typecheck::ResolvedVariantData;
use expo_ast::ast::{FieldPattern, Pattern};
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::enums::{
    enum_definition_from_entry, enum_entry_from_resolution, resolved_enum_symbol,
};
use super::{
    BindSource, PatternCheck, PatternInputs, PayloadBind, TestStep, ensure_local_declared,
    field_type_for, require_local,
};
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

pub(super) fn lower_enum_tuple_check(
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

/// Emit `EnumTagGet(subject) == const(tag)` into `block` and return
/// the resulting `Bool` value.
pub(super) fn emit_enum_tag_eq(
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

struct EnumPatternMetadata<'a> {
    enum_symbol: IRSymbol,
    label: String,
    owner: GlobalRegistryId,
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
    let owner = match inputs.subject_ty {
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => *id,
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
