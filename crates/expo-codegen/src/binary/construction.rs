//! Binary literal construction: compiles `<<segments...>>` expressions into
//! LLVM IR that allocates a length-prefixed buffer and packs segment values.

use expo_ast::ast::{BinaryEndianness, BinarySegment};
use expo_typecheck::types::{Primitive, Type};
use inkwell::values::FunctionValue;

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;

use super::{is_float_segment, segment_bit_width, string_segment_bit_width};

/// The resolved kind of a single binary segment, determined without LLVM.
enum BinarySegmentKind {
    Float,
    Integer { little_endian: bool },
    String,
}

/// A single resolved binary segment with its kind and byte-aligned bit width.
struct ResolvedBinarySegment {
    bit_width: u64,
    kind: BinarySegmentKind,
}

/// The fully resolved layout of a binary literal.
struct ResolvedBinaryLayout {
    segments: Vec<ResolvedBinarySegment>,
    total_bits: u64,
}

/// Resolves the layout of a binary literal by computing each segment's kind
/// and bit width. Validates that all segments are byte-aligned.
fn resolve_binary_segments(segments: &[BinarySegment]) -> Result<ResolvedBinaryLayout, String> {
    let mut resolved = Vec::with_capacity(segments.len());
    let mut total_bits: u64 = 0;

    for seg in segments {
        let bits = segment_bit_width(seg)?;
        if !bits.is_multiple_of(8) {
            return Err(format!(
                "sub-byte segment ({bits} bits) not yet supported in codegen"
            ));
        }
        total_bits += bits;

        let kind = if string_segment_bit_width(seg).is_some() {
            BinarySegmentKind::String
        } else if is_float_segment(seg) {
            BinarySegmentKind::Float
        } else {
            let little_endian = matches!(seg.endianness, Some(BinaryEndianness::Little));
            BinarySegmentKind::Integer { little_endian }
        };

        resolved.push(ResolvedBinarySegment {
            bit_width: bits,
            kind,
        });
    }

    Ok(ResolvedBinaryLayout {
        segments: resolved,
        total_bits,
    })
}

/// Compiles a `<<seg1, seg2, ...>>` binary literal into a heap-allocated,
/// length-prefixed byte buffer. Returns a pointer to the payload (past the
/// 8-byte length prefix). Only byte-aligned segments are supported;
/// sub-byte bit packing is deferred.
pub(crate) fn compile_binary_literal<'ctx>(
    c: &mut Compiler<'ctx>,
    segments: &[BinarySegment],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let layout = resolve_binary_segments(segments)?;

    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let total_bytes = layout.total_bits / 8;

    let alloc_size = i64_type.const_int(8 + total_bytes, false);
    let malloc = *c.functions.get("malloc").expect("malloc not declared");
    let base_ptr = c
        .call(malloc, &[alloc_size.into()], "bin_alloc")
        .unwrap()
        .into_pointer_value();

    c.builder
        .build_store(base_ptr, i64_type.const_int(layout.total_bits, false))
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

    let mut byte_offset: u64 = 0;
    for (seg, resolved) in segments.iter().zip(&layout.segments) {
        let num_bytes = resolved.bit_width / 8;

        match &resolved.kind {
            BinarySegmentKind::String => {
                let str_ptr = compile_expr(c, &seg.value, function)?
                    .ok_or("string segment produced no value")?
                    .value
                    .into_pointer_value();
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
                let memcpy = *c.functions.get("memcpy").expect("memcpy not declared");
                c.call_void(
                    memcpy,
                    &[
                        dest.into(),
                        str_ptr.into(),
                        i64_type.const_int(num_bytes, false).into(),
                    ],
                    "str_seg_cpy",
                );
                byte_offset += num_bytes;
                continue;
            }
            BinarySegmentKind::Float => {
                let value = compile_expr(c, &seg.value, function)?
                    .ok_or("binary segment value produced no value")?
                    .value;
                let val_i64 = if resolved.bit_width == 32 {
                    let f32_val = c
                        .builder
                        .build_float_trunc(
                            value.into_float_value(),
                            c.context.f32_type(),
                            "f32_trunc",
                        )
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
                emit_byte_packing(c, val_i64, num_bytes, false, payload_ptr, byte_offset);
            }
            BinarySegmentKind::Integer { little_endian } => {
                let value = compile_expr(c, &seg.value, function)?
                    .ok_or("binary segment value produced no value")?
                    .value;
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
                emit_byte_packing(
                    c,
                    val_i64,
                    num_bytes,
                    *little_endian,
                    payload_ptr,
                    byte_offset,
                );
            }
        }

        byte_offset += num_bytes;
    }

    Ok(Some(TypedValue::new(
        payload_ptr.into(),
        Type::Primitive(Primitive::Binary),
    )))
}

/// Emits the byte-by-byte packing loop for a single integer/float segment.
fn emit_byte_packing<'ctx>(
    c: &mut Compiler<'ctx>,
    val_i64: inkwell::values::IntValue<'ctx>,
    num_bytes: u64,
    little_endian: bool,
    payload_ptr: inkwell::values::PointerValue<'ctx>,
    byte_offset: u64,
) {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();

    for i in 0..num_bytes {
        let shift_amount = if little_endian {
            i * 8
        } else {
            (num_bytes - 1 - i) * 8
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
