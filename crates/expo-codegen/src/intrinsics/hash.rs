use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;
use crate::intrinsics::STRING_HEADER_BYTES;
use expo_ir::identity::FunctionIdentifier;

pub fn emit_hash_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    type_name: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();
    let self_val = fn_val.get_nth_param(0).unwrap();

    let result = if type_name == "String" {
        emit_fnv1a_hash(c, self_val.into_pointer_value())
    } else if type_name == "Bool" {
        c.builder
            .build_int_z_extend(self_val.into_int_value(), i64_ty, "bool_ext")
            .unwrap()
            .into()
    } else {
        let iv = self_val.into_int_value();
        let width = iv.get_type().get_bit_width();
        let extended = if width < 64 {
            c.builder.build_int_z_extend(iv, i64_ty, "ext").unwrap()
        } else {
            iv
        };
        emit_splitmix64(c, extended).into()
    };

    c.builder.build_return(Some(&result)).unwrap();
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

pub fn emit_eq_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    type_name: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap();
    let other_val = fn_val.get_nth_param(1).unwrap();

    let result: inkwell::values::IntValue<'ctx> = if type_name == "String" {
        let strcmp = *c
            .functions
            .get(&FunctionIdentifier::new("strcmp"))
            .expect("strcmp not declared");
        let cmp_result = c
            .call(
                strcmp,
                &[self_val.into(), other_val.into()],
                "strcmp_result",
            )
            .unwrap()
            .into_int_value();
        c.builder
            .build_int_compare(
                IntPredicate::EQ,
                cmp_result,
                c.context.i32_type().const_int(0, false),
                "str_eq",
            )
            .unwrap()
    } else {
        c.builder
            .build_int_compare(
                IntPredicate::EQ,
                self_val.into_int_value(),
                other_val.into_int_value(),
                "int_eq",
            )
            .unwrap()
    };

    c.builder.build_return(Some(&result)).unwrap();
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

pub fn emit_bitwise_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    type_name: &str,
    op: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap().into_int_value();
    let is_unsigned = type_name.starts_with('U');

    let result = match op {
        "band" => {
            let other = fn_val.get_nth_param(1).unwrap().into_int_value();
            c.builder.build_and(self_val, other, "band").unwrap()
        }
        "bor" => {
            let other = fn_val.get_nth_param(1).unwrap().into_int_value();
            c.builder.build_or(self_val, other, "bor").unwrap()
        }
        "bxor" => {
            let other = fn_val.get_nth_param(1).unwrap().into_int_value();
            c.builder.build_xor(self_val, other, "bxor").unwrap()
        }
        "bnot" => c.builder.build_not(self_val, "bnot").unwrap(),
        "bsl" => {
            let n = fn_val.get_nth_param(1).unwrap().into_int_value();
            let n_cast = c
                .builder
                .build_int_truncate_or_bit_cast(n, self_val.get_type(), "bsl_n")
                .unwrap();
            c.builder.build_left_shift(self_val, n_cast, "bsl").unwrap()
        }
        "bsr" => {
            let n = fn_val.get_nth_param(1).unwrap().into_int_value();
            let n_cast = c
                .builder
                .build_int_truncate_or_bit_cast(n, self_val.get_type(), "bsr_n")
                .unwrap();
            c.builder
                .build_right_shift(self_val, n_cast, !is_unsigned, "bsr")
                .unwrap()
        }
        _ => return Err(format!("unknown bitwise op: {op}")),
    };

    c.builder.build_return(Some(&result)).unwrap();
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

/// SplitMix64 finalizer: produces well-distributed hash from any i64 input.
fn emit_splitmix64<'ctx>(
    c: &Compiler<'ctx>,
    val: inkwell::values::IntValue<'ctx>,
) -> inkwell::values::IntValue<'ctx> {
    let i64_ty = c.context.i64_type();

    let shifted = c
        .builder
        .build_right_shift(val, i64_ty.const_int(30, false), false, "shr30")
        .unwrap();
    let x1 = c.builder.build_xor(val, shifted, "xor1").unwrap();

    let mul1 = c
        .builder
        .build_int_mul(x1, i64_ty.const_int(0xbf58476d1ce4e5b9, false), "mul1")
        .unwrap();

    let shifted2 = c
        .builder
        .build_right_shift(mul1, i64_ty.const_int(27, false), false, "shr27")
        .unwrap();
    let x2 = c.builder.build_xor(mul1, shifted2, "xor2").unwrap();

    let mul2 = c
        .builder
        .build_int_mul(x2, i64_ty.const_int(0x94d049bb133111eb, false), "mul2")
        .unwrap();

    let shifted3 = c
        .builder
        .build_right_shift(mul2, i64_ty.const_int(31, false), false, "shr31")
        .unwrap();
    c.builder.build_xor(mul2, shifted3, "xor3").unwrap()
}

/// FNV-1a hash over a length-prefixed string (reads byte count from header).
fn emit_fnv1a_hash<'ctx>(
    c: &mut Compiler<'ctx>,
    str_ptr: inkwell::values::PointerValue<'ctx>,
) -> inkwell::values::BasicValueEnum<'ctx> {
    let fn_val = c.builder.get_insert_block().unwrap().get_parent().unwrap();
    let i64_ty = c.context.i64_type();
    let i8_ty = c.context.i8_type();

    let offset_basis = i64_ty.const_int(0xcbf29ce484222325, false);
    let fnv_prime = i64_ty.const_int(0x100000001b3, false);

    let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
    let hdr_ptr = unsafe {
        c.builder
            .build_gep(i8_ty, str_ptr, &[neg_hdr], "hdr_ptr")
            .unwrap()
    };
    let bit_length = c
        .builder
        .build_load(i64_ty, hdr_ptr, "bit_length")
        .unwrap()
        .into_int_value();
    let byte_count = c
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .unwrap();

    let header_bb = c.context.append_basic_block(fn_val, "fnv_header");
    let body_bb = c.context.append_basic_block(fn_val, "fnv_body");
    let done_bb = c.context.append_basic_block(fn_val, "fnv_done");
    let entry_bb = c.builder.get_insert_block().unwrap();

    c.builder.build_unconditional_branch(header_bb).unwrap();

    c.builder.position_at_end(header_bb);
    let phi_hash = c.builder.build_phi(i64_ty, "hash").unwrap();
    let phi_idx = c.builder.build_phi(i64_ty, "idx").unwrap();
    phi_hash.add_incoming(&[(&offset_basis, entry_bb)]);
    phi_idx.add_incoming(&[(&i64_ty.const_int(0, false), entry_bb)]);

    let current_hash = phi_hash.as_basic_value().into_int_value();
    let current_idx = phi_idx.as_basic_value().into_int_value();

    let at_end = c
        .builder
        .build_int_compare(IntPredicate::UGE, current_idx, byte_count, "at_end")
        .unwrap();
    c.builder
        .build_conditional_branch(at_end, done_bb, body_bb)
        .unwrap();

    c.builder.position_at_end(body_bb);
    let byte_ptr = unsafe {
        c.builder
            .build_gep(i8_ty, str_ptr, &[current_idx], "byte_ptr")
            .unwrap()
    };
    let byte = c
        .builder
        .build_load(i8_ty, byte_ptr, "byte")
        .unwrap()
        .into_int_value();
    let byte_ext = c
        .builder
        .build_int_z_extend(byte, i64_ty, "byte_ext")
        .unwrap();
    let xored = c
        .builder
        .build_xor(current_hash, byte_ext, "xor_byte")
        .unwrap();
    let hashed = c
        .builder
        .build_int_mul(xored, fnv_prime, "fnv_mul")
        .unwrap();
    let next_idx = c
        .builder
        .build_int_add(current_idx, i64_ty.const_int(1, false), "next_idx")
        .unwrap();
    c.builder.build_unconditional_branch(header_bb).unwrap();

    phi_hash.add_incoming(&[(&hashed, body_bb)]);
    phi_idx.add_incoming(&[(&next_idx, body_bb)]);

    c.builder.position_at_end(done_bb);
    current_hash.into()
}
