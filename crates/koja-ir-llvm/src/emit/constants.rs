//! Constant emission. Covers scalar `ConstValue`s, the heap-payload
//! header shape (`String` / `Binary` / `Bits` literals), and the
//! `LoadConst` cache that materializes pooled aggregate constants
//! through [`emit_ir_constant_aggregate`].

use inkwell::module::Linkage;
use inkwell::types::{ArrayType, IntType};
use inkwell::values::{BasicValueEnum, PointerValue};
use koja_ir::{ConstValue, IRConstantValue, IRSymbol, IRVariantTag};

use crate::ctx::EmitContext;
use crate::error::LlvmError;

use super::heap_layout::{HEADER_BYTES, RC_IMMORTAL};

/// Materialize the LLVM SSA value for `LoadConst`, using
/// [`EmitContext::load_const_cache`] so repeat references reuse a
/// single materialization. The constant pool snapshot must have
/// been attached before codegen (see
/// [`crate::ctx::EmitContext::attach_constant_pool`]). A missing
/// pool is a compiler-bug surface.
pub(super) fn emit_load_const<'ctx>(
    ctx: &EmitContext<'ctx>,
    const_id: &IRSymbol,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if let Some(v) = ctx.load_const_cache.borrow().get(const_id).copied() {
        return Ok(v);
    }
    let pool = ctx.constant_pool.borrow();
    let pool = pool.as_ref().ok_or_else(|| {
        LlvmError::Codegen(
            "LoadConst emitted without ConstantPoolSnapshot \
             (`attach_constant_pool` must precede codegen)"
                .into(),
        )
    })?;
    let entry = pool.get(const_id).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LoadConst references missing pooled constant `{const_id}` (IR seal invariant \
             violated or pool attachment bug)",
        ))
    })?;
    let materialized = emit_ir_constant_aggregate(ctx, entry)?;
    ctx.load_const_cache
        .borrow_mut()
        .insert(const_id.clone(), materialized);
    Ok(materialized)
}

/// Recursively materialize an [`IRConstantValue`] pool entry into a
/// true LLVM constant (`StructValue`, enum outer aggregate, string
/// payload pointer). Every branch must yield a constant and never an
/// instruction. The cache hands the value to every function that
/// reads the constant, and a constant struct embeds its fields by
/// value, so an instruction here would either refer across
/// functions or fail `const_named_struct`.
fn emit_ir_constant_aggregate<'ctx>(
    ctx: &EmitContext<'ctx>,
    cv: &IRConstantValue,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match cv {
        IRConstantValue::Primitive(inner) => emit_const(ctx, inner),
        IRConstantValue::EnumVariant { tag, ty } => Ok(emit_unit_variant_constant(ctx, *tag, ty)),
        IRConstantValue::Struct { fields, ty } => {
            let struct_type = ctx.layouts.struct_type(ty.mangled());
            let comps: Vec<BasicValueEnum<'ctx>> = fields
                .iter()
                .map(|f| emit_ir_constant_aggregate(ctx, f))
                .collect::<Result<_, _>>()?;
            Ok(struct_type.const_named_struct(&comps).into())
        }
    }
}

/// Build a unit variant of enum `ty` as an LLVM constant. The outer
/// is `{ [count x iN] }` (see [`crate::layout::enums`]) and the tag
/// is the first byte of the value, so chunk 0 carries the tag and
/// every other chunk is zero. Placing the tag in the low byte of
/// chunk 0 assumes a little-endian target, which holds for every
/// target the backend emits. Constant enum values are unit variants
/// only (the typecheck lift enforces this), so no payload is
/// written.
fn emit_unit_variant_constant<'ctx>(
    ctx: &EmitContext<'ctx>,
    tag: IRVariantTag,
    ty: &IRSymbol,
) -> BasicValueEnum<'ctx> {
    let outer = ctx.enum_outer_type(ty.mangled());
    let chunks = outer
        .get_field_type_at_index(0)
        .and_then(|field| field.try_into().ok())
        .unwrap_or_else(|| {
            panic!("LLVM emit: enum outer `{ty}` is not `{{ [count x iN] }}` (layout invariant)")
        });
    let chunks: ArrayType<'ctx> = chunks;
    let chunk_ty: IntType<'ctx> = chunks.get_element_type().try_into().unwrap_or_else(|_| {
        panic!("LLVM emit: enum outer `{ty}` chunk element is not an integer (layout invariant)")
    });
    let mut values = vec![chunk_ty.const_zero(); chunks.len() as usize];
    if let Some(first) = values.first_mut() {
        *first = chunk_ty.const_int(u64::from(tag.0), false);
    }
    outer
        .const_named_struct(&[chunk_ty.const_array(&values).into()])
        .into()
}

/// Lower a scalar [`ConstValue`] to a const LLVM value.
/// `ConstValue::Unit` binds the inert `i8 0` placeholder that pairs
/// with the `i8` slots minted by [`crate::types::ir_basic_type`].
pub(super) fn emit_const<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: &ConstValue,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match value {
        ConstValue::Binary(bytes) => {
            Ok(emit_const_payload(ctx, bytes, (bytes.len() as u64) * 8, false, "bin").into())
        }
        ConstValue::Bits { bytes, bit_length } => {
            Ok(emit_const_payload(ctx, bytes, *bit_length, false, "bits").into())
        }
        ConstValue::Bool(b) => Ok(ctx
            .context
            .bool_type()
            .const_int(u64::from(*b), false)
            .into()),
        // `const_float` always takes f64. The f32 type narrows on
        // its own (bit-exact since f32 widens losslessly).
        ConstValue::Float32(v) => Ok(ctx.context.f32_type().const_float(f64::from(*v)).into()),
        ConstValue::Float64(v) => Ok(ctx.context.f64_type().const_float(*v).into()),
        ConstValue::Int8(v) => Ok(ctx.context.i8_type().const_int(*v as u64, true).into()),
        ConstValue::Int16(v) => Ok(ctx.context.i16_type().const_int(*v as u64, true).into()),
        ConstValue::Int32(v) => Ok(ctx.context.i32_type().const_int(*v as u64, true).into()),
        ConstValue::Int64(v) => Ok(ctx.context.i64_type().const_int(*v as u64, true).into()),
        ConstValue::String(s) => {
            Ok(emit_const_payload(ctx, s.as_bytes(), (s.len() as u64) * 8, true, "str").into())
        }
        ConstValue::UInt8(v) => Ok(ctx.context.i8_type().const_int(u64::from(*v), false).into()),
        ConstValue::UInt16(v) => Ok(ctx
            .context
            .i16_type()
            .const_int(u64::from(*v), false)
            .into()),
        ConstValue::UInt32(v) => Ok(ctx
            .context
            .i32_type()
            .const_int(u64::from(*v), false)
            .into()),
        ConstValue::UInt64(v) => Ok(ctx.context.i64_type().const_int(*v, false).into()),
        ConstValue::Unit => Ok(ctx.context.i8_type().const_zero().into()),
    }
}

/// Emit a `String`-shaped payload literal (8-byte bit-length
/// header + NUL-terminated bytes) and return the payload pointer.
/// Intrinsic emitters that need to mint a literal error message
/// (`Int.parse` / `Float.parse` etc.) call this to get a String SSA
/// value compatible with the rest of the pipeline, the same
/// shape every `ConstValue::String` materializes through.
pub(crate) fn emit_string_literal_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    bytes: &[u8],
    prefix: &str,
) -> PointerValue<'ctx> {
    emit_const_payload(ctx, bytes, (bytes.len() as u64) * 8, true, prefix)
}

/// Emit a heap-payload literal as a private constant global in the
/// [`super::heap_layout`] block shape, with `rc` set to
/// [`RC_IMMORTAL`] so the runtime never frees the rodata block.
/// Returns a const-GEP to the payload.
///
/// `with_nul` adds a trailing `\0` byte to the payload array, used
/// by `String` for libc compat. `Binary` and `Bits` pass `false`:
/// no terminator. `bytes.len()` is the source-byte count. For
/// `Bits` whose `bit_length` is a non-multiple of 8, the producer
/// is responsible for zero-padding the trailing partial byte.
///
/// `prefix` becomes `koja_<prefix>.<n>` in the LLVM IR, purely
/// cosmetic but helps reading raw IR (`str` / `bin` / `bits`).
fn emit_const_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    bytes: &[u8],
    bit_length: u64,
    with_nul: bool,
    prefix: &str,
) -> PointerValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let array_len = bytes.len() as u32 + if with_nul { 1 } else { 0 };
    let payload_ty = i8_ty.array_type(array_len);
    let header_ty = ctx
        .context
        .struct_type(&[i64_ty.into(), i64_ty.into(), payload_ty.into()], false);
    let bytes_const = ctx.context.const_string(bytes, with_nul);
    let initializer = header_ty.const_named_struct(&[
        i64_ty.const_int(RC_IMMORTAL as u64, false).into(),
        i64_ty.const_int(bit_length, false).into(),
        bytes_const.into(),
    ]);
    let symbol = ctx.next_payload_symbol(prefix);
    let global = ctx.module.add_global(header_ty, None, &symbol);
    global.set_initializer(&initializer);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    // SAFETY: the global was just defined with the header, so the
    // payload starts `HEADER_BYTES` in.
    unsafe {
        global
            .as_pointer_value()
            .const_gep(i8_ty, &[i64_ty.const_int(HEADER_BYTES, false)])
    }
}
