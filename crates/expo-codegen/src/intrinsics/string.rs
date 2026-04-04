use expo_typecheck::types::{GenericKind, Primitive, Type};
use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;
use crate::generics::ensure_types_exist;

use super::{
    OPTION_NONE_TAG, OPTION_SOME_TAG, STRING_HEADER_BYTES, build_result_err, build_result_ok,
};

pub fn emit_conversion_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    match mangled {
        "String_to_binary" | "Binary_to_bits" => {
            let self_val = fn_val.get_nth_param(0).unwrap();
            c.builder.build_return(Some(&self_val)).unwrap();
        }
        "Binary_to_string" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let i8_ty = c.context.i8_type();
            let i64_ty = c.context.i64_type();

            let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg_hdr], "hdr")
                    .unwrap()
            };
            let bit_length = c
                .builder
                .build_load(i64_ty, hdr_ptr, "bit_len")
                .unwrap()
                .into_int_value();
            let byte_count = c
                .builder
                .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "bytes")
                .unwrap();

            let validate_fn = *c
                .functions
                .get("expo_utf8_validate")
                .ok_or("expo_utf8_validate not declared")?;
            let is_valid = c
                .call(
                    validate_fn,
                    &[self_ptr.into(), byte_count.into()],
                    "utf8_ok",
                )
                .unwrap()
                .into_int_value();

            let valid_bb = c.context.append_basic_block(fn_val, "valid");
            let invalid_bb = c.context.append_basic_block(fn_val, "invalid");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            let cond = c
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    is_valid,
                    i64_ty.const_int(0, false),
                    "is_valid",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(cond, valid_bb, invalid_bb)
                .unwrap();

            c.builder.position_at_end(valid_bb);
            let malloc_fn = *c.functions.get("malloc").ok_or("malloc not declared")?;
            let memcpy_fn = *c.functions.get("memcpy").ok_or("memcpy not declared")?;
            let alloc_size = c
                .builder
                .build_int_add(byte_count, i64_ty.const_int(9, false), "alloc_sz")
                .unwrap();
            let new_base = c
                .call(malloc_fn, &[alloc_size.into()], "new_base")
                .unwrap()
                .into_pointer_value();
            c.builder.build_store(new_base, bit_length).unwrap();
            let new_payload = unsafe {
                c.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        new_base,
                        &[i64_ty.const_int(STRING_HEADER_BYTES, false)],
                        "new_payload",
                    )
                    .unwrap()
            };
            c.call_void(
                memcpy_fn,
                &[new_payload.into(), self_ptr.into(), byte_count.into()],
                "cpy",
            );
            let nul_ptr = unsafe {
                c.builder
                    .build_in_bounds_gep(i8_ty, new_payload, &[byte_count], "nul")
                    .unwrap()
            };
            c.builder
                .build_store(nul_ptr, i8_ty.const_int(0, false))
                .unwrap();

            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let ok_result = build_result_ok(c, new_payload.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let valid_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(invalid_bb);
            let err_msg = c.create_string_global(b"invalid UTF-8", "utf8_err_msg");
            let err_result = build_result_err(c, err_msg.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let invalid_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, valid_end), (&err_result, invalid_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "Binary_byte_size" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let i8_ty = c.context.i8_type();
            let i64_ty = c.context.i64_type();

            let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg_hdr], "hdr")
                    .unwrap()
            };
            let bit_length = c
                .builder
                .build_load(i64_ty, hdr_ptr, "bit_len")
                .unwrap()
                .into_int_value();
            let byte_count = c
                .builder
                .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "bytes")
                .unwrap();
            c.builder.build_return(Some(&byte_count)).unwrap();
        }
        "Bits_to_binary" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let i8_ty = c.context.i8_type();
            let i64_ty = c.context.i64_type();

            let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg_hdr], "hdr")
                    .unwrap()
            };
            let bit_length = c
                .builder
                .build_load(i64_ty, hdr_ptr, "bit_len")
                .unwrap()
                .into_int_value();

            let remainder = c
                .builder
                .build_and(bit_length, i64_ty.const_int(7, false), "rem")
                .unwrap();
            let is_aligned = c
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    remainder,
                    i64_ty.const_int(0, false),
                    "aligned",
                )
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_aligned, ok_bb, err_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let ok_result = build_result_ok(c, self_ptr.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_msg =
                c.create_string_global(b"bit length is not byte-aligned", "align_err_msg");
            let err_result = build_result_err(c, err_msg.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        _ => return Err(format!("unknown conversion intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

pub fn emit_string_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    match mangled {
        "String_length" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_string_length")
                .ok_or("expo_string_length not declared")?;
            let result = c.call(rt_fn, &[self_ptr.into()], "len").unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        "String_get" => {
            let option_mangled = "Option_$String$";
            ensure_types_exist(
                c,
                &Type::GenericInstance {
                    base: "Option".to_string(),
                    type_args: vec![Type::Primitive(Primitive::String)],
                    kind: GenericKind::Enum,
                },
            )?;
            let option_struct = *c
                .types
                .structs
                .get(option_mangled)
                .ok_or("no LLVM type for Option_$String$")?;

            let self_ptr = fn_val.get_nth_param(0).unwrap();
            let index = fn_val.get_nth_param(1).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_string_get")
                .ok_or("expo_string_get not declared")?;
            let raw_ptr = c
                .call(rt_fn, &[self_ptr.into(), index.into()], "ch")
                .unwrap()
                .into_pointer_value();

            let i8_ty = c.context.i8_type();
            let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
            let is_null = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    raw_ptr,
                    ptr_ty.const_null(),
                    "is_null",
                )
                .unwrap();

            let some_bb = c.context.append_basic_block(fn_val, "some");
            let none_bb = c.context.append_basic_block(fn_val, "none");
            c.builder
                .build_conditional_branch(is_null, none_bb, some_bb)
                .unwrap();

            c.builder.position_at_end(some_bb);
            let alloca_some = c
                .builder
                .build_alloca(option_struct, "some_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_some, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(OPTION_SOME_TAG, false))
                .unwrap();
            let payload_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_some, 1, "payload_ptr")
                .unwrap();
            c.builder.build_store(payload_ptr, raw_ptr).unwrap();
            let result = c
                .builder
                .build_load(option_struct, alloca_some, "some_val")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();

            c.builder.position_at_end(none_bb);
            let alloca_none = c
                .builder
                .build_alloca(option_struct, "none_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_none, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(OPTION_NONE_TAG, false))
                .unwrap();
            let result = c
                .builder
                .build_load(option_struct, alloca_none, "none_val")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        "String_byte_length" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let i8_ty = c.context.i8_type();
            let i64_ty = c.context.i64_type();
            let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg_hdr], "hdr")
                    .unwrap()
            };
            let bit_length = c
                .builder
                .build_load(i64_ty, hdr_ptr, "bit_len")
                .unwrap()
                .into_int_value();
            let byte_count = c
                .builder
                .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "bytes")
                .unwrap();
            c.builder.build_return(Some(&byte_count)).unwrap();
        }
        "String_slice" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap();
            let range_val = fn_val.get_nth_param(1).unwrap().into_struct_value();
            let start = c
                .builder
                .build_extract_value(range_val, 0, "start")
                .unwrap();
            let stop = c.builder.build_extract_value(range_val, 1, "stop").unwrap();
            let rt_fn = *c
                .functions
                .get("expo_string_slice")
                .ok_or("expo_string_slice not declared")?;
            let result = c
                .call(
                    rt_fn,
                    &[self_ptr.into(), start.into(), stop.into()],
                    "sliced",
                )
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        _ => return Err(format!("unknown string intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

pub fn emit_parse_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let input_ptr = fn_val.get_nth_param(0).unwrap();
    let result_type = fn_val
        .get_type()
        .get_return_type()
        .unwrap()
        .into_struct_type();

    match mangled {
        "Int_parse" => {
            let i64_ty = c.context.i64_type();
            let out_alloca = c.builder.build_alloca(i64_ty, "out").unwrap();
            let rt_fn = *c
                .functions
                .get("expo_int_parse")
                .ok_or("expo_int_parse not declared")?;
            let ok = c
                .call(rt_fn, &[input_ptr.into(), out_alloca.into()], "ok")
                .unwrap()
                .into_int_value();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            let cond = c
                .builder
                .build_int_compare(IntPredicate::NE, ok, i64_ty.const_int(0, false), "parsed")
                .unwrap();
            c.builder
                .build_conditional_branch(cond, ok_bb, err_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let parsed = c.builder.build_load(i64_ty, out_alloca, "val").unwrap();
            let ok_result = build_result_ok(c, parsed, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_msg = c.create_string_global(b"invalid integer", "int_parse_err");
            let err_result = build_result_err(c, err_msg.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "Float_parse" => {
            let i64_ty = c.context.i64_type();
            let f64_ty = c.context.f64_type();
            let out_alloca = c.builder.build_alloca(f64_ty, "out").unwrap();
            let rt_fn = *c
                .functions
                .get("expo_float_parse")
                .ok_or("expo_float_parse not declared")?;
            let ok = c
                .call(rt_fn, &[input_ptr.into(), out_alloca.into()], "ok")
                .unwrap()
                .into_int_value();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            let cond = c
                .builder
                .build_int_compare(IntPredicate::NE, ok, i64_ty.const_int(0, false), "parsed")
                .unwrap();
            c.builder
                .build_conditional_branch(cond, ok_bb, err_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let parsed = c.builder.build_load(f64_ty, out_alloca, "val").unwrap();
            let ok_result = build_result_ok(c, parsed, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_msg = c.create_string_global(b"invalid float", "float_parse_err");
            let err_result = build_result_err(c, err_msg.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        _ => return Err(format!("unknown parse intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
