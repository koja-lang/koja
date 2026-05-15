//! Pre-emit phase for [`expo_alpha_ir::IREnumDecl`]: build one
//! [`super::EnumLayout`] per decl on [`super::TypeLayouts`].
//!
//! Three-phase across all packages so an enum's tuple/struct payload
//! can carry another struct or enum regardless of declaration
//! order: [`declare_enum_type`] mints opaque placeholders for the
//! outer + every variant's complete + every non-Unit variant's
//! payload; [`define_enum_payload_bodies`] sets the variant payload
//! bodies (no size queries, so opaque inner references are fine);
//! [`define_enum_completes_and_outer`] then sets the variant
//! complete + outer bodies once every transitively-referenced
//! payload is set.
//!
//! The complete + outer phase has a sub-ordering requirement of its
//! own: a variant complete body sizes its padding from
//! `get_abi_alignment(payload)`, and the outer body sizes itself
//! from `max(get_abi_size(complete))` across variants — both
//! returns 0/1 when the payload references an opaque inner enum
//! outer. [`crate::program::compile_program`] walks `decl_order`
//! (a topologically-sorted enum list) so every dependency's outer
//! is already set when an enum's complete+outer phase runs.
//!
//! ## Layout (Rust-style, alignment-correct)
//!
//! - **Per-variant payload** (non-Unit): non-packed struct over the
//!   payload's field types in declaration order. Name:
//!   `<enum>.<variant>.payload`.
//! - **Per-variant complete**: `{ i8 tag, [pad x i8], payload }` for
//!   non-Unit (`pad = align(payload) - 1` so the payload starts at
//!   its natural alignment; `[0 x i8]` when align is 1, so the
//!   payload always lives at field index 2 — the construction
//!   emitter doesn't have to special-case the no-padding subcase).
//!   `{ i8 tag }` for Unit. Name: `<enum>.<variant>`.
//! - **Outer**: `{ [count x iN] }` where `N = max_align * 8` and
//!   `count * max_align >= max_complete_size` (rounded up). The
//!   `iN` chunks give LLVM the max-align hint a flat `[M x i8]`
//!   would lose. Construction (see [`crate::emit::instruction`]'s
//!   `emit_enum_construct`) allocas the outer, GEPs through the
//!   complete struct for the tag and the payload for fields, then
//!   loads the populated outer as the SSA result.

use expo_alpha_ir::{IREnumDecl, IREnumVariant, IRStructField, IRType, IRVariantPayload};
use inkwell::types::{BasicTypeEnum, StructType};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::layout::{EnumLayout, VariantLayout};
use crate::types::ir_basic_type;

pub(crate) fn declare_enum_type<'ctx>(ctx: &EmitContext<'ctx>, decl: &IREnumDecl) {
    ctx.context.opaque_struct_type(decl.symbol.mangled());
    for variant in &decl.variants {
        ctx.context
            .opaque_struct_type(&variant_complete_name(decl, variant));
        if !matches!(variant.payload, IRVariantPayload::Unit) {
            ctx.context
                .opaque_struct_type(&variant_payload_name(decl, variant));
        }
    }
}

/// Set every variant's payload body. No size or alignment queries
/// happen here, so it's safe to call before any of the referenced
/// types (other enums' outer chunks, mutually-referenced structs)
/// have their bodies set. The variant complete + outer bodies are
/// deferred to [`define_enum_completes_and_outer`].
pub(crate) fn define_enum_payload_bodies<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IREnumDecl,
) -> Result<(), LlvmError> {
    for variant in &decl.variants {
        define_payload_body(ctx, decl, variant)?;
    }
    Ok(())
}

/// Set every variant's complete body and the enum's outer chunk
/// body, then register the variant layouts. Must run after every
/// transitively-referenced enum's outer body has been set —
/// `get_abi_alignment` on a variant payload that names an opaque
/// enum outer returns 1 instead of the real alignment, which would
/// collapse the padding and outer chunk count. The caller
/// ([`crate::program::compile_program`]) drives this in
/// topological dependency order.
pub(crate) fn define_enum_completes_and_outer<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IREnumDecl,
) -> Result<(), LlvmError> {
    let mut variants = Vec::with_capacity(decl.variants.len());
    let mut max_complete_size: u64 = 0;
    let mut max_complete_align: u32 = 1;
    for variant in &decl.variants {
        let layout = define_variant_complete(ctx, decl, variant);
        let complete_basic: BasicTypeEnum<'ctx> = layout.complete.into();
        max_complete_size =
            max_complete_size.max(ctx.layouts.target_data.get_abi_size(&complete_basic));
        max_complete_align =
            max_complete_align.max(ctx.layouts.target_data.get_abi_alignment(&complete_basic));
        variants.push(layout);
    }
    define_outer_body(ctx, decl, max_complete_size, max_complete_align);
    ctx.layouts
        .register_enum_layout(decl.symbol.clone(), EnumLayout { variants });
    let payloads = decl.variants.iter().map(|v| v.payload.clone()).collect();
    ctx.layouts
        .register_enum_variant_payloads(decl.symbol.clone(), payloads);
    Ok(())
}

fn define_variant_complete<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IREnumDecl,
    variant: &IREnumVariant,
) -> VariantLayout<'ctx> {
    let payload = lookup_payload_struct(ctx, decl, variant);
    let complete = lookup_named_struct(ctx, &variant_complete_name(decl, variant));
    let body = build_complete_body(ctx, payload);
    complete.set_body(&body, false);
    VariantLayout { complete, payload }
}

fn lookup_payload_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IREnumDecl,
    variant: &IREnumVariant,
) -> Option<StructType<'ctx>> {
    if matches!(variant.payload, IRVariantPayload::Unit) {
        return None;
    }
    Some(lookup_named_struct(
        ctx,
        &variant_payload_name(decl, variant),
    ))
}

fn define_payload_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IREnumDecl,
    variant: &IREnumVariant,
) -> Result<Option<StructType<'ctx>>, LlvmError> {
    match &variant.payload {
        IRVariantPayload::Struct(fields) => {
            let payload = lookup_named_struct(ctx, &variant_payload_name(decl, variant));
            payload.set_body(&payload_body_from_struct(ctx, fields)?, false);
            Ok(Some(payload))
        }
        IRVariantPayload::Tuple(types) => {
            let payload = lookup_named_struct(ctx, &variant_payload_name(decl, variant));
            payload.set_body(&payload_body_from_tuple(ctx, types)?, false);
            Ok(Some(payload))
        }
        IRVariantPayload::Unit => Ok(None),
    }
}

fn payload_body_from_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    fields: &[IRStructField],
) -> Result<Vec<BasicTypeEnum<'ctx>>, LlvmError> {
    let mut body = Vec::with_capacity(fields.len());
    for field in fields {
        body.push(ir_basic_type(ctx, &field.ir_type)?);
    }
    Ok(body)
}

fn payload_body_from_tuple<'ctx>(
    ctx: &EmitContext<'ctx>,
    types: &[IRType],
) -> Result<Vec<BasicTypeEnum<'ctx>>, LlvmError> {
    let mut body = Vec::with_capacity(types.len());
    for ty in types {
        body.push(ir_basic_type(ctx, ty)?);
    }
    Ok(body)
}

fn build_complete_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: Option<StructType<'ctx>>,
) -> Vec<BasicTypeEnum<'ctx>> {
    let i8_type = ctx.context.i8_type();
    let mut body: Vec<BasicTypeEnum<'ctx>> = vec![i8_type.into()];
    if let Some(payload_struct) = payload {
        let payload_basic: BasicTypeEnum<'ctx> = payload_struct.into();
        let payload_align = ctx.layouts.target_data.get_abi_alignment(&payload_basic);
        let pad = payload_align.saturating_sub(1);
        body.push(i8_type.array_type(pad).into());
        body.push(payload_basic);
    }
    body
}

fn define_outer_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IREnumDecl,
    max_complete_size: u64,
    max_complete_align: u32,
) -> StructType<'ctx> {
    let outer = lookup_named_struct(ctx, decl.symbol.mangled());
    let chunk_bits = max_complete_align * 8;
    let chunk_type = ctx.context.custom_width_int_type(chunk_bits);
    let align_u64 = u64::from(max_complete_align);
    // Round size up to a multiple of alignment so the chunk_count
    // matches Rust's enum-size convention. LLVM rounds struct sizes
    // up regardless, but doing it explicitly keeps the count
    // faithful to the layout we'll observe later.
    let outer_size = max_complete_size.div_ceil(align_u64) * align_u64;
    let chunk_count = (outer_size / align_u64) as u32;
    outer.set_body(&[chunk_type.array_type(chunk_count).into()], false);
    outer
}

fn lookup_named_struct<'ctx>(ctx: &EmitContext<'ctx>, name: &str) -> StructType<'ctx> {
    ctx.context.get_struct_type(name).unwrap_or_else(|| {
        panic!(
            "alpha LLVM emit: named struct `{name}` not declared — \
             declare_enum_type ordering violation",
        )
    })
}

fn variant_complete_name(decl: &IREnumDecl, variant: &IREnumVariant) -> String {
    format!("{}.{}", decl.symbol, variant.name)
}

fn variant_payload_name(decl: &IREnumDecl, variant: &IREnumVariant) -> String {
    format!("{}.{}.payload", decl.symbol, variant.name)
}
