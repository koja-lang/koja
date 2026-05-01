//! Binary literal construction: compiles `<<segments...>>` expressions into
//! LLVM IR that allocates a length-prefixed buffer and packs segment values.

use expo_ast::ast::{BinaryEndianness, BinarySegment};
use expo_ir::lower::binary::resolve_binary_segments;
use expo_ir::resolved::construction::ResolvedBinarySegmentKind;
use expo_typecheck::types::{Primitive, Type};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use expo_ir::identity::FunctionIdentifier;

/// AST-level emitter for `<<seg1, seg2, ...>>` binary literals.
/// Compiles into a heap-allocated, length-prefixed byte buffer and
/// returns a pointer to the payload (past the 8-byte length prefix).
/// Only byte-aligned segments are supported; sub-byte bit packing is
/// deferred.
///
/// Top-level binary literals now lift to
/// `IRInstruction::BinaryConstruct` (see `emit_binary_construct` in
/// `control/instructions.rs`), so this path only runs when a `<<...>>`
/// expression is nested inside a Stub'd parent AST (closures, list
/// literals, match operands, ...). Not currently exercised by the
/// test suite -- retained until those Stub'd parents lift, at which
/// point this collapses with `emit_binary_construct`'s shared
/// `emit_byte_packing` helper.
pub(crate) fn compile_binary_literal<'ctx>(
    compiler: &mut Compiler<'ctx>,
    segments: &[BinarySegment],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let layout = resolve_binary_segments(segments)?;

    let (_base_ptr, payload_ptr) = allocate_binary_buffer(compiler, layout.total_bits)?;

    let mut byte_offset: u64 = 0;
    for (seg, resolved) in segments.iter().zip(&layout.segments) {
        let value = compile_expr(compiler, &seg.value, function)?
            .ok_or("binary segment produced no value")?
            .value;
        pack_segment(
            compiler,
            resolved.kind,
            resolved.bit_width,
            value,
            payload_ptr,
            byte_offset,
        )?;
        byte_offset += resolved.bit_width / 8;
    }

    Ok(Some(TypedValue::new(
        payload_ptr.into(),
        Type::Primitive(Primitive::Binary),
    )))
}

/// Allocate the heap-backed binary buffer used by both
/// [`compile_binary_literal`] (AST) and
/// [`crate::control::instructions::emit_binary_construct`] (IR).
///
/// Layout: `[i64 total_bits | payload[total_bits / 8]]`. Returns
/// `(base_ptr, payload_ptr)` so callers can either keep the base
/// (if they need to revisit the length prefix) or just take the
/// payload pointer that gets handed back to user code as the value
/// of a `Binary`.
pub(crate) fn allocate_binary_buffer<'ctx>(
    c: &mut Compiler<'ctx>,
    total_bits: u64,
) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>), String> {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let total_bytes = total_bits / 8;

    let alloc_size = i64_type.const_int(8 + total_bytes, false);
    let malloc = *c
        .functions
        .get(&FunctionIdentifier::new("malloc"))
        .ok_or("malloc not declared")?;
    let base_ptr = c
        .call(malloc, &[alloc_size.into()], "bin_alloc")
        .ok_or("malloc did not return a value")?
        .into_pointer_value();

    c.builder
        .build_store(base_ptr, i64_type.const_int(total_bits, false))
        .unwrap();

    let payload_ptr = unsafe {
        c.builder
            .build_in_bounds_gep(
                i8_type,
                base_ptr,
                &[i64_type.const_int(8, false)],
                "bin_payload",
            )
            .unwrap()
    };

    Ok((base_ptr, payload_ptr))
}

/// Pack one already-evaluated binary segment value into the payload
/// at `byte_offset`. Shared by [`compile_binary_literal`] (AST) and
/// [`crate::control::instructions::emit_binary_construct`] (IR);
/// once the value has been materialized the dispatch on
/// `String` / `Float` / `Integer` is identical, so it lives here.
///
/// `kind` carries any per-segment knobs (e.g. integer endianness);
/// floats are always big-endian per the language semantics.
pub(crate) fn pack_segment<'ctx>(
    c: &mut Compiler<'ctx>,
    kind: ResolvedBinarySegmentKind,
    bit_width: u64,
    value: BasicValueEnum<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    byte_offset: u64,
) -> Result<(), String> {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let num_bytes = bit_width / 8;

    match kind {
        ResolvedBinarySegmentKind::String => {
            let str_ptr = value.into_pointer_value();
            let dest = unsafe {
                c.builder
                    .build_in_bounds_gep(
                        i8_type,
                        payload_ptr,
                        &[i64_type.const_int(byte_offset, false)],
                        "str_seg_dest",
                    )
                    .unwrap()
            };
            let memcpy = *c
                .functions
                .get(&FunctionIdentifier::new("memcpy"))
                .ok_or("memcpy not declared")?;
            c.call_void(
                memcpy,
                &[
                    dest.into(),
                    str_ptr.into(),
                    i64_type.const_int(num_bytes, false).into(),
                ],
                "str_seg_cpy",
            );
        }
        ResolvedBinarySegmentKind::Float => {
            let val_i64 = if bit_width == 32 {
                let f32_val = c
                    .builder
                    .build_float_trunc(value.into_float_value(), c.context.f32_type(), "f32_trunc")
                    .unwrap();
                let i32_bits = c
                    .builder
                    .build_bit_cast(f32_val, c.context.i32_type(), "f32_bits")
                    .unwrap()
                    .into_int_value();
                c.builder
                    .build_int_z_extend(i32_bits, i64_type, "f32_ext")
                    .unwrap()
            } else {
                c.builder
                    .build_bit_cast(value, i64_type, "f64_bits")
                    .unwrap()
                    .into_int_value()
            };
            emit_byte_packing(
                c,
                val_i64,
                num_bytes,
                BinaryEndianness::Big,
                payload_ptr,
                byte_offset,
            );
        }
        ResolvedBinarySegmentKind::Integer { endianness } => {
            let int_val = value.into_int_value();
            let width = int_val.get_type().get_bit_width();
            let val_i64 = if width < 64 {
                c.builder
                    .build_int_z_extend(int_val, i64_type, "seg_ext")
                    .unwrap()
            } else if width > 64 {
                c.builder
                    .build_int_truncate(int_val, i64_type, "seg_trunc")
                    .unwrap()
            } else {
                int_val
            };
            emit_byte_packing(c, val_i64, num_bytes, endianness, payload_ptr, byte_offset);
        }
    }

    Ok(())
}

/// Emits the byte-by-byte packing loop for a single integer/float segment.
///
/// Shared between the AST-level [`compile_binary_literal`] (still
/// reachable via `Stub`-nested AST expressions) and the typed-IR
/// [`crate::control::instructions::emit_binary_construct`] executor.
pub(crate) fn emit_byte_packing<'ctx>(
    c: &mut Compiler<'ctx>,
    val_i64: inkwell::values::IntValue<'ctx>,
    num_bytes: u64,
    endianness: BinaryEndianness,
    payload_ptr: inkwell::values::PointerValue<'ctx>,
    byte_offset: u64,
) {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();

    for i in 0..num_bytes {
        let shift_amount = match endianness {
            BinaryEndianness::Little => i * 8,
            BinaryEndianness::Big => (num_bytes - 1 - i) * 8,
        };
        let shifted = if shift_amount > 0 {
            c.builder
                .build_right_shift(
                    val_i64,
                    i64_type.const_int(shift_amount, false),
                    false,
                    "shr",
                )
                .unwrap()
        } else {
            val_i64
        };
        let byte_val = c
            .builder
            .build_int_truncate(shifted, i8_type, "byte")
            .unwrap();
        let dest = unsafe {
            c.builder
                .build_in_bounds_gep(
                    i8_type,
                    payload_ptr,
                    &[i64_type.const_int(byte_offset + i, false)],
                    "byte_ptr",
                )
                .unwrap()
        };
        c.builder.build_store(dest, byte_val).unwrap();
    }
}
