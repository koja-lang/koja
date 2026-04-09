//! Binary pattern matching codegen: compiles `<<seg1, seg2, ...>>` patterns
//! in `match` arms into LLVM IR length checks, segment extraction, literal
//! comparison, variable binding, and greedy rest capture.

use expo_ast::ast::{BinaryEndianness, BinarySegment, BinaryUnit, ExprKind, TypeExpr};
use expo_typecheck::types::{Primitive, Type};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::Compiler;
use crate::drop::Ownership;
use crate::expr::compile_expr;

use super::{is_float_segment, segment_bit_width, string_segment_bit_width};

/// Compiles a binary pattern (`<<seg1, seg2, ...>>`) into an i1 condition.
/// Emits a length check against the total fixed prefix, then extracts each
/// segment by reading bytes and assembling integers with the correct byte order.
pub(crate) fn compile_binary_pattern<'ctx>(
    c: &mut Compiler<'ctx>,
    segments: &[BinarySegment],
    subject_ptr: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());

    let payload_ptr = c
        .builder
        .build_load(ptr_type, subject_ptr, "bin_payload")
        .unwrap()
        .into_pointer_value();

    let len_ptr = unsafe {
        c.builder
            .build_gep(
                i8_type,
                payload_ptr,
                &[i64_type.const_int((-8i64) as u64, true)],
                "bin_len_ptr",
            )
            .unwrap()
    };
    let bit_length = c
        .builder
        .build_load(i64_type, len_ptr, "bin_bit_len")
        .unwrap()
        .into_int_value();
    let byte_length = c
        .builder
        .build_right_shift(
            bit_length,
            i64_type.const_int(3, false),
            false,
            "bin_byte_len",
        )
        .unwrap();

    let mut total_fixed_bits: u64 = 0;
    let has_greedy = segments.iter().any(is_greedy_rest);
    for seg in segments {
        if is_greedy_rest(seg) {
            continue;
        }
        let bits = segment_bit_width(seg)?;
        if !bits.is_multiple_of(8) {
            return Err(format!(
                "sub-byte segment ({bits} bits) not yet supported in binary pattern codegen"
            ));
        }
        total_fixed_bits += bits;
    }
    let required_bytes = total_fixed_bits / 8;

    // With greedy rest: length >= required (rest captures remainder)
    // Without greedy rest: length == required (exact match)
    let cmp_pred = if has_greedy {
        IntPredicate::UGE
    } else {
        IntPredicate::EQ
    };
    let mut result = c
        .builder
        .build_int_compare(
            cmp_pred,
            byte_length,
            i64_type.const_int(required_bytes, false),
            "bin_len_ok",
        )
        .unwrap();

    let mut byte_offset: u64 = 0;
    for seg in segments {
        if is_greedy_rest(seg) {
            compile_greedy_rest(c, seg, payload_ptr, byte_length, byte_offset, function)?;
            continue;
        }

        let bits = segment_bit_width(seg)?;
        let num_bytes = bits / 8;

        if string_segment_bit_width(seg).is_some() {
            let str_ptr = compile_expr(c, &seg.value, function)?
                .ok_or("string segment produced no value")?
                .value
                .into_pointer_value();
            let buf_ptr = unsafe {
                c.builder
                    .build_in_bounds_gep(
                        i8_type,
                        payload_ptr,
                        &[i64_type.const_int(byte_offset, false)],
                        "str_pat_ptr",
                    )
                    .unwrap()
            };
            let memcmp = *c.functions.get("memcmp").expect("memcmp not declared");
            let cmp_result = c
                .call(
                    memcmp,
                    &[
                        buf_ptr.into(),
                        str_ptr.into(),
                        i64_type.const_int(num_bytes, false).into(),
                    ],
                    "str_pat_cmp",
                )
                .unwrap()
                .into_int_value();
            let cmp = c
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    cmp_result,
                    c.context.i32_type().const_int(0, false),
                    "str_pat_eq",
                )
                .unwrap();
            result = c.builder.build_and(result, cmp, "str_seg_and").unwrap();
            byte_offset += num_bytes;
            continue;
        }

        let is_little = matches!(seg.endianness, Some(BinaryEndianness::Little));
        let is_literal = matches!(
            seg.value.kind,
            ExprKind::Literal { .. } | ExprKind::Unary { .. }
        );
        let is_binding = matches!(&seg.value.kind, ExprKind::Ident { name, .. } if name != "_");
        let is_discard = matches!(&seg.value.kind, ExprKind::Ident { name, .. } if name == "_");

        if is_discard {
            byte_offset += num_bytes;
            continue;
        }

        let extracted = extract_segment_value(c, payload_ptr, byte_offset, num_bytes, is_little);

        if is_literal {
            let lit_val = compile_expr(c, &seg.value, function)?
                .ok_or("literal segment produced no value")?
                .value
                .into_int_value();
            let lit_i64 = if lit_val.get_type().get_bit_width() < 64 {
                c.builder
                    .build_int_z_extend(lit_val, i64_type, "lit_ext")
                    .unwrap()
            } else {
                lit_val
            };
            let mask = if bits >= 64 {
                i64_type.const_all_ones()
            } else {
                i64_type.const_int((1u64 << bits) - 1, false)
            };
            let masked_lit = c.builder.build_and(lit_i64, mask, "lit_mask").unwrap();
            let masked_ext = c.builder.build_and(extracted, mask, "ext_mask").unwrap();
            let cmp = c
                .builder
                .build_int_compare(IntPredicate::EQ, masked_ext, masked_lit, "seg_eq")
                .unwrap();
            result = c.builder.build_and(result, cmp, "seg_and").unwrap();
        } else if is_binding && let ExprKind::Ident { name, .. } = &seg.value.kind {
            let binding_ty = binding_type(seg);
            let is_float = is_float_segment(seg);

            let bind_val: BasicValueEnum = if is_float {
                if bits == 32 {
                    let trunc = c
                        .builder
                        .build_int_truncate(extracted, c.context.i32_type(), "f32_int")
                        .unwrap();
                    c.builder
                        .build_bit_cast(trunc, c.context.f32_type(), "f32_val")
                        .unwrap()
                } else {
                    c.builder
                        .build_bit_cast(extracted, c.context.f64_type(), "f64_val")
                        .unwrap()
                }
            } else {
                extracted.into()
            };

            let llvm_ty = bind_val.get_type();
            let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
            c.builder.build_store(alloca, bind_val).unwrap();
            c.fn_state
                .variables
                .insert(name.clone(), (alloca, binding_ty, Ownership::Unowned));
        }

        byte_offset += num_bytes;
    }

    Ok(result)
}

/// Implements greedy rest capture: allocates a new Binary with the remaining
/// bytes after the fixed prefix, copies them via memcpy, and binds the variable.
fn compile_greedy_rest<'ctx>(
    c: &mut Compiler<'ctx>,
    seg: &BinarySegment,
    payload_ptr: PointerValue<'ctx>,
    byte_length: IntValue<'ctx>,
    fixed_offset: u64,
    _function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let name = match &seg.value.kind {
        ExprKind::Ident { name, .. } if name != "_" => name.clone(),
        _ => return Ok(()),
    };

    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();

    let remaining_bytes = c
        .builder
        .build_int_sub(
            byte_length,
            i64_type.const_int(fixed_offset, false),
            "rest_bytes",
        )
        .unwrap();
    let remaining_bits = c
        .builder
        .build_int_mul(remaining_bytes, i64_type.const_int(8, false), "rest_bits")
        .unwrap();

    let eight = i64_type.const_int(8, false);
    let alloc_size = c
        .builder
        .build_int_add(eight, remaining_bytes, "rest_alloc_sz")
        .unwrap();
    let malloc = *c.functions.get("malloc").expect("malloc not declared");
    let base_ptr = c
        .call(malloc, &[alloc_size.into()], "rest_alloc")
        .unwrap()
        .into_pointer_value();

    c.builder.build_store(base_ptr, remaining_bits).unwrap();

    let rest_payload = unsafe {
        c.builder
            .build_in_bounds_gep(i8_type, base_ptr, &[eight], "rest_payload")
            .unwrap()
    };

    let src_ptr = unsafe {
        c.builder
            .build_in_bounds_gep(
                i8_type,
                payload_ptr,
                &[i64_type.const_int(fixed_offset, false)],
                "rest_src",
            )
            .unwrap()
    };

    let memcpy = *c.functions.get("memcpy").expect("memcpy not declared");
    c.call_void(
        memcpy,
        &[rest_payload.into(), src_ptr.into(), remaining_bytes.into()],
        "rest_cpy",
    );

    let ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());
    let alloca = c.builder.build_alloca(ptr_type, &name).unwrap();
    c.builder.build_store(alloca, rest_payload).unwrap();

    let rest_ty = if let Some(TypeExpr::Named { path, .. }) = &seg.type_ann {
        if path.last().is_some_and(|n| n == "Bits") {
            Type::Primitive(Primitive::Bits)
        } else {
            Type::Primitive(Primitive::Binary)
        }
    } else {
        Type::Primitive(Primitive::Binary)
    };

    c.fn_state
        .variables
        .insert(name, (alloca, rest_ty, Ownership::Owned));

    Ok(())
}

/// Reads `num_bytes` from `payload_ptr` at `byte_offset` and assembles them
/// into an i64 with the specified byte order. Inverse of construction's packing loop.
fn extract_segment_value<'ctx>(
    c: &mut Compiler<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    byte_offset: u64,
    num_bytes: u64,
    is_little: bool,
) -> IntValue<'ctx> {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let mut result = i64_type.const_int(0, false);

    for i in 0..num_bytes {
        let ptr = unsafe {
            c.builder
                .build_in_bounds_gep(
                    i8_type,
                    payload_ptr,
                    &[i64_type.const_int(byte_offset + i, false)],
                    "seg_byte_ptr",
                )
                .unwrap()
        };
        let byte_val = c
            .builder
            .build_load(i8_type, ptr, "seg_byte")
            .unwrap()
            .into_int_value();
        let ext = c
            .builder
            .build_int_z_extend(byte_val, i64_type, "seg_ext")
            .unwrap();

        let shift_amount = if is_little {
            i * 8
        } else {
            (num_bytes - 1 - i) * 8
        };
        let shifted = if shift_amount > 0 {
            c.builder
                .build_left_shift(ext, i64_type.const_int(shift_amount, false), "seg_shl")
                .unwrap()
        } else {
            ext
        };
        result = c.builder.build_or(result, shifted, "seg_or").unwrap();
    }

    result
}

fn is_greedy_rest(seg: &BinarySegment) -> bool {
    if seg.size.is_some() {
        return false;
    }
    if let Some(TypeExpr::Named { path, .. }) = &seg.type_ann
        && let Some(name) = path.last()
    {
        return matches!(name.as_str(), "Binary" | "Bits");
    }
    false
}

/// Determines the Expo type for a pattern binding based on its segment form.
fn binding_type(seg: &BinarySegment) -> Type {
    if let Some(type_ann) = &seg.type_ann {
        if let TypeExpr::Named { path, .. } = type_ann
            && let Some(name) = path.last()
            && let Some(p) = Primitive::from_name(name)
        {
            return Type::Primitive(p);
        }
        Type::Primitive(Primitive::I64)
    } else if seg.size.is_some() && seg.unit == BinaryUnit::Byte {
        Type::Primitive(Primitive::Binary)
    } else {
        Type::Primitive(Primitive::I64)
    }
}
