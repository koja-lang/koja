//! Hash table infrastructure shared by `Map<K,V>` and `Set<T>`.
//!
//! Provides intrinsic LLVM IR emission for the `Hash` and `Equality`
//! protocol methods on primitive types, plus shared helpers for probing,
//! resizing, and calling hash/eq on arbitrary key types.

use expo_typecheck::types::{GenericKind, Primitive, Type, mangle_name};
use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;
use crate::debug::snprintf_to_expo_string;
use crate::util::bool_to_string_ptr;

const PRIMITIVE_TYPES: &[&str] = &[
    "Bool", "Int", "Int8", "Int16", "Int32", "String", "UInt8", "UInt16", "UInt32", "UInt64",
];

const BITWISE_OPS: &[&str] = &["band", "bor", "bxor", "bnot", "bsl", "bsr"];

const BITWISE_TYPES: &[&str] = &[
    "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
];

const CONVERSION_INTRINSICS: &[&str] = &[
    "String_to_binary",
    "Binary_to_bits",
    "Binary_to_string",
    "Bits_to_binary",
];

const STRING_INTRINSICS: &[&str] = &[
    "String_length",
    "String_get",
    "String_byte_length",
    "String_slice",
];

const DEBUG_TYPES: &[&str] = &[
    "Bool", "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64", "Float",
    "Float32",
];

const PARSE_INTRINSICS: &[&str] = &["Int_parse", "Float_parse"];

const FD_INTRINSICS: &[&str] = &["Fd_read", "Fd_write", "Fd_close"];

const FILE_INTRINSICS: &[&str] = &["File_open", "File_read"];

const SOCKET_INTRINSICS: &[&str] = &[
    "Socket_create",
    "Socket_bind",
    "Socket_listen",
    "Socket_accept",
    "Socket_set_reuse_addr",
];

pub fn is_primitive_intrinsic(mangled: &str) -> bool {
    for prim in PRIMITIVE_TYPES {
        if mangled == format!("{prim}_hash") || mangled == format!("{prim}_eq") {
            return true;
        }
    }
    for prim in BITWISE_TYPES {
        for op in BITWISE_OPS {
            if mangled == format!("{prim}_{op}") {
                return true;
            }
        }
    }
    for prim in DEBUG_TYPES {
        if mangled == format!("{prim}_format") {
            return true;
        }
    }
    if CONVERSION_INTRINSICS.contains(&mangled)
        || STRING_INTRINSICS.contains(&mangled)
        || PARSE_INTRINSICS.contains(&mangled)
        || FD_INTRINSICS.contains(&mangled)
        || FILE_INTRINSICS.contains(&mangled)
        || SOCKET_INTRINSICS.contains(&mangled)
    {
        return true;
    }
    false
}

pub fn emit_primitive_intrinsic<'ctx>(c: &mut Compiler<'ctx>, mangled: &str) -> Result<(), String> {
    let fn_val = *c
        .functions
        .get(mangled)
        .ok_or_else(|| format!("undeclared intrinsic: {mangled}"))?;

    if CONVERSION_INTRINSICS.contains(&mangled) {
        return emit_conversion_intrinsic(c, fn_val, mangled);
    }

    if STRING_INTRINSICS.contains(&mangled) {
        return emit_string_intrinsic(c, fn_val, mangled);
    }

    if PARSE_INTRINSICS.contains(&mangled) {
        return emit_parse_intrinsic(c, fn_val, mangled);
    }

    if FD_INTRINSICS.contains(&mangled) {
        return emit_fd_intrinsic(c, fn_val, mangled);
    }

    if FILE_INTRINSICS.contains(&mangled) {
        return emit_file_intrinsic(c, fn_val, mangled);
    }

    if SOCKET_INTRINSICS.contains(&mangled) {
        return emit_socket_intrinsic(c, fn_val, mangled);
    }

    if let Some(type_name) = mangled.strip_suffix("_format") {
        return emit_debug_format_intrinsic(c, fn_val, type_name);
    }

    if let Some(type_name) = mangled.strip_suffix("_hash") {
        emit_hash_intrinsic(c, fn_val, type_name)
    } else if let Some(type_name) = mangled.strip_suffix("_eq") {
        emit_eq_intrinsic(c, fn_val, type_name)
    } else {
        for op in BITWISE_OPS {
            if let Some(type_name) = mangled.strip_suffix(&format!("_{op}")) {
                return emit_bitwise_intrinsic(c, fn_val, type_name, op);
            }
        }
        Err(format!("unknown primitive intrinsic: {mangled}"))
    }
}

// ---------------------------------------------------------------------------
// Shared struct layout / method helpers for Map and Set
// ---------------------------------------------------------------------------

/// Both Map<K,V> and Set<T> use the same LLVM struct layout:
/// `{ entries_ptr: i8*, states_ptr: i8*, length: i64, capacity: i64 }`
pub fn monomorphize_hashtable_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let st = c.context.opaque_struct_type(mangled);
    let ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());
    let i64_type = c.context.i64_type();
    st.set_body(
        &[
            ptr_type.into(),
            ptr_type.into(),
            i64_type.into(),
            i64_type.into(),
        ],
        false,
    );
    c.types.structs.insert(mangled.to_string(), st);
    c.types.mono_struct_info.insert(
        mangled.to_string(),
        vec![
            (
                "entries_ptr".to_string(),
                Type::Primitive(Primitive::String),
            ),
            ("states_ptr".to_string(), Type::Primitive(Primitive::String)),
            ("length".to_string(), Type::Primitive(Primitive::I64)),
            ("capacity".to_string(), Type::Primitive(Primitive::I64)),
        ],
    );
    Ok(())
}

/// Emits `fn new() -> CollectionStruct` for a hash-table-backed collection.
/// Allocates entries buffer (`cap * entry_size` bytes) and states buffer
/// (`cap` bytes, zeroed), returns the 4-field struct.
pub fn emit_hashtable_new<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_fn: &str,
    collection_struct: inkwell::types::StructType<'ctx>,
    entry_size: u64,
) -> Result<(), String> {
    let i64_ty = c.context.i64_type();
    let i32_ty = c.context.i32_type();

    let fn_type = collection_struct.fn_type(&[], false);
    let fn_val = c.module.add_function(mangled_fn, fn_type, None);
    c.functions.insert(mangled_fn.to_string(), fn_val);

    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let cap = i64_ty.const_int(8, false);
    let entries_bytes = c
        .builder
        .build_int_mul(cap, i64_ty.const_int(entry_size, false), "entries_bytes")
        .unwrap();
    let malloc = *c.functions.get("malloc").unwrap();
    let entries_ptr = c
        .call(malloc, &[entries_bytes.into()], "entries")
        .unwrap()
        .into_pointer_value();
    let states_ptr = c
        .call(malloc, &[cap.into()], "states")
        .unwrap()
        .into_pointer_value();
    let memset = *c.functions.get("memset").unwrap();
    c.call_void(
        memset,
        &[
            states_ptr.into(),
            i32_ty.const_int(0, false).into(),
            cap.into(),
        ],
        "clear_states",
    );

    let result = collection_struct.get_undef();
    let result = c
        .builder
        .build_insert_value(result, entries_ptr, 0, "r_e")
        .unwrap()
        .into_struct_value();
    let result = c
        .builder
        .build_insert_value(result, states_ptr, 1, "r_s")
        .unwrap()
        .into_struct_value();
    let result = c
        .builder
        .build_insert_value(result, i64_ty.const_int(0, false), 2, "r_l")
        .unwrap()
        .into_struct_value();
    let result = c
        .builder
        .build_insert_value(result, cap, 3, "r_c")
        .unwrap()
        .into_struct_value();
    c.builder.build_return(Some(&result)).unwrap();

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

/// Emits `fn length(self) -> Int` for a hash-table-backed collection.
pub fn emit_hashtable_length<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_fn: &str,
    collection_struct: inkwell::types::StructType<'ctx>,
) -> Result<(), String> {
    let i64_ty = c.context.i64_type();

    let fn_type = i64_ty.fn_type(&[collection_struct.into()], false);
    let fn_val = c.module.add_function(mangled_fn, fn_type, None);
    c.functions.insert(mangled_fn.to_string(), fn_val);

    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
    let len = c.builder.build_extract_value(self_val, 2, "len").unwrap();
    c.builder.build_return(Some(&len)).unwrap();

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

/// Emits `fn empty?(self) -> Bool` for a hash-table-backed collection.
/// Returns true when field 2 (length) is zero.
pub fn emit_hashtable_empty<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_fn: &str,
    collection_struct: inkwell::types::StructType<'ctx>,
) -> Result<(), String> {
    let i64_ty = c.context.i64_type();
    let i1_ty = c.context.bool_type();

    let fn_type = i1_ty.fn_type(&[collection_struct.into()], false);
    let fn_val = c.module.add_function(mangled_fn, fn_type, None);
    c.functions.insert(mangled_fn.to_string(), fn_val);

    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
    let len = c
        .builder
        .build_extract_value(self_val, 2, "len")
        .unwrap()
        .into_int_value();
    let is_empty = c
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            len,
            i64_ty.const_int(0, false),
            "is_empty",
        )
        .unwrap();
    c.builder.build_return(Some(&is_empty)).unwrap();

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Hash/eq function lookup helpers
// ---------------------------------------------------------------------------

pub fn ensure_hash_fn<'ctx>(
    c: &Compiler<'ctx>,
    key_type: &Type,
) -> Result<FunctionValue<'ctx>, String> {
    let type_name = type_display_name(key_type);
    let fn_name = format!("{type_name}_hash");
    if let Some(fv) = c.functions.get(&fn_name) {
        return Ok(*fv);
    }
    Err(format!(
        "type `{type_name}` does not implement Hash (no `{fn_name}` found)"
    ))
}

pub fn ensure_eq_fn<'ctx>(
    c: &Compiler<'ctx>,
    key_type: &Type,
) -> Result<FunctionValue<'ctx>, String> {
    let type_name = type_display_name(key_type);
    let fn_name = format!("{type_name}_eq");
    if let Some(fv) = c.functions.get(&fn_name) {
        return Ok(*fv);
    }
    Err(format!(
        "type `{type_name}` does not implement Equality (no `{fn_name}` found)"
    ))
}

pub fn type_display_name(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => match p {
            Primitive::Binary => "Binary".to_string(),
            Primitive::Bits => "Bits".to_string(),
            Primitive::Bool => "Bool".to_string(),
            Primitive::I8 => "Int8".to_string(),
            Primitive::I16 => "Int16".to_string(),
            Primitive::I32 => "Int32".to_string(),
            Primitive::I64 => "Int".to_string(),
            Primitive::U8 => "UInt8".to_string(),
            Primitive::U16 => "UInt16".to_string(),
            Primitive::U32 => "UInt32".to_string(),
            Primitive::U64 => "UInt64".to_string(),
            Primitive::F32 => "Float32".to_string(),
            Primitive::F64 => "Float".to_string(),
            Primitive::String => "String".to_string(),
        },
        Type::Struct(name) => name.clone(),
        Type::GenericInstance {
            base, type_args, ..
        } => mangle_name(base, type_args),
        _ => format!("{ty:?}"),
    }
}

// ---------------------------------------------------------------------------
// Primitive hash/eq intrinsic implementations
// ---------------------------------------------------------------------------

fn emit_hash_intrinsic<'ctx>(
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

fn emit_eq_intrinsic<'ctx>(
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
        let strcmp = *c.functions.get("strcmp").expect("strcmp not declared");
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

fn emit_bitwise_intrinsic<'ctx>(
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

fn emit_conversion_intrinsic<'ctx>(
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

            let neg8 = i64_ty.const_int((-8i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg8], "hdr")
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
                        &[i64_ty.const_int(8, false)],
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
        "Bits_to_binary" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let i8_ty = c.context.i8_type();
            let i64_ty = c.context.i64_type();

            let neg8 = i64_ty.const_int((-8i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg8], "hdr")
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

/// Constructs a `Result.Ok(value)` struct: tag=0, payload=value.
fn build_result_ok<'ctx>(
    c: &Compiler<'ctx>,
    value: inkwell::values::BasicValueEnum<'ctx>,
    result_type: inkwell::types::StructType<'ctx>,
) -> inkwell::values::BasicValueEnum<'ctx> {
    let alloca = c.builder.build_alloca(result_type, "ok_buf").unwrap();
    let tag_ptr = c
        .builder
        .build_struct_gep(result_type, alloca, 0, "ok_tag_ptr")
        .unwrap();
    c.builder
        .build_store(tag_ptr, c.context.i8_type().const_int(0, false))
        .unwrap();
    if result_type.count_fields() > 1 {
        let payload_ptr = c
            .builder
            .build_struct_gep(result_type, alloca, 1, "ok_payload_ptr")
            .unwrap();
        c.builder.build_store(payload_ptr, value).unwrap();
    }
    c.builder.build_load(result_type, alloca, "ok_val").unwrap()
}

/// Constructs a `Result.Err(value)` struct: tag=1, payload=value.
fn build_result_err<'ctx>(
    c: &Compiler<'ctx>,
    value: inkwell::values::BasicValueEnum<'ctx>,
    result_type: inkwell::types::StructType<'ctx>,
) -> inkwell::values::BasicValueEnum<'ctx> {
    let alloca = c.builder.build_alloca(result_type, "err_buf").unwrap();
    let tag_ptr = c
        .builder
        .build_struct_gep(result_type, alloca, 0, "err_tag_ptr")
        .unwrap();
    c.builder
        .build_store(tag_ptr, c.context.i8_type().const_int(1, false))
        .unwrap();
    if result_type.count_fields() > 1 {
        let payload_ptr = c
            .builder
            .build_struct_gep(result_type, alloca, 1, "err_payload_ptr")
            .unwrap();
        c.builder.build_store(payload_ptr, value).unwrap();
    }
    c.builder
        .build_load(result_type, alloca, "err_val")
        .unwrap()
}

fn emit_string_intrinsic<'ctx>(
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
            c.ensure_types_exist(&Type::GenericInstance {
                base: "Option".to_string(),
                type_args: vec![Type::Primitive(Primitive::String)],
                kind: GenericKind::Enum,
            })?;
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
                .build_store(tag_ptr, i8_ty.const_int(0, false))
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
                .build_store(tag_ptr, i8_ty.const_int(1, false))
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
            let neg8 = i64_ty.const_int((-8i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, self_ptr, &[neg8], "hdr")
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

fn emit_parse_intrinsic<'ctx>(
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

fn emit_fd_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();
    let result_type = fn_val
        .get_type()
        .get_return_type()
        .unwrap()
        .into_struct_type();

    match mangled {
        "Fd_read" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd = c.builder.build_extract_value(self_val, 0, "fd").unwrap();
            let count = fn_val.get_nth_param(1).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_fd_read")
                .ok_or("expo_fd_read not declared")?;
            let ptr = c
                .call(rt_fn, &[fd.into(), count.into()], "read_ptr")
                .unwrap()
                .into_pointer_value();

            let is_null = c.builder.build_is_null(ptr, "is_null").unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, ptr.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "Fd_write" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd = c.builder.build_extract_value(self_val, 0, "fd").unwrap();
            let data = fn_val.get_nth_param(1).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_fd_write")
                .ok_or("expo_fd_write not declared")?;
            let written = c
                .call(rt_fn, &[fd.into(), data.into()], "written")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, written, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, written.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "Fd_close" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd = c.builder.build_extract_value(self_val, 0, "fd").unwrap();
            let rt_fn = *c
                .functions
                .get("expo_fd_close")
                .ok_or("expo_fd_close not declared")?;
            let ret = c
                .call(rt_fn, &[fd.into()], "close_ret")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, ret, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_msg = c.create_string_global(b"ok", "close_ok_msg");
            let ok_result = build_result_ok(c, ok_msg.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        _ => return Err(format!("unknown fd intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

fn emit_file_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();
    let result_type = fn_val
        .get_type()
        .get_return_type()
        .unwrap()
        .into_struct_type();

    match mangled {
        "File_open" => {
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            let mode = i64_ty.const_int(0, false);
            let rt_fn = *c
                .functions
                .get("expo_file_open")
                .ok_or("expo_file_open not declared")?;
            let fd_val = c
                .call(rt_fn, &[path_ptr.into(), mode.into()], "fd_val")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, fd_val, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let file_struct_ty = c
                .types
                .structs
                .get("File")
                .copied()
                .ok_or("File struct type not found")?;
            let alloca = c.builder.build_alloca(file_struct_ty, "file_tmp").unwrap();
            let fd_field_ptr = c
                .builder
                .build_struct_gep(file_struct_ty, alloca, 0, "fd_field")
                .unwrap();
            let fd_struct_ty = c
                .types
                .structs
                .get("Fd")
                .copied()
                .ok_or("Fd struct type not found")?;
            let fd_desc_ptr = c
                .builder
                .build_struct_gep(fd_struct_ty, fd_field_ptr, 0, "fd_desc")
                .unwrap();
            c.builder.build_store(fd_desc_ptr, fd_val).unwrap();
            let file_val = c
                .builder
                .build_load(file_struct_ty, alloca, "file_val")
                .unwrap();
            let ok_result = build_result_ok(c, file_val, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "File_read" => {
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_file_read_all")
                .ok_or("expo_file_read_all not declared")?;
            let ptr = c
                .call(rt_fn, &[path_ptr.into()], "read_ptr")
                .unwrap()
                .into_pointer_value();

            let is_null = c.builder.build_is_null(ptr, "is_null").unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, ptr.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        _ => return Err(format!("unknown file intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

fn emit_socket_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();
    let result_type = fn_val
        .get_type()
        .get_return_type()
        .unwrap()
        .into_struct_type();

    match mangled {
        "Socket_create" => {
            let rt_fn = *c
                .functions
                .get("expo_socket_create")
                .ok_or("expo_socket_create not declared")?;
            let fd_val = c.call(rt_fn, &[], "fd_val").unwrap().into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, fd_val, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let socket_struct_ty = c
                .types
                .structs
                .get("Socket")
                .copied()
                .ok_or("Socket struct type not found")?;
            let alloca = c
                .builder
                .build_alloca(socket_struct_ty, "sock_tmp")
                .unwrap();
            let fd_field_ptr = c
                .builder
                .build_struct_gep(socket_struct_ty, alloca, 0, "fd_field")
                .unwrap();
            let fd_struct_ty = c
                .types
                .structs
                .get("Fd")
                .copied()
                .ok_or("Fd struct type not found")?;
            let fd_desc_ptr = c
                .builder
                .build_struct_gep(fd_struct_ty, fd_field_ptr, 0, "fd_desc")
                .unwrap();
            c.builder.build_store(fd_desc_ptr, fd_val).unwrap();
            let sock_val = c
                .builder
                .build_load(socket_struct_ty, alloca, "sock_val")
                .unwrap();
            let ok_result = build_result_ok(c, sock_val, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "Socket_bind" | "Socket_listen" | "Socket_set_reuse_addr" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd_inner = c
                .builder
                .build_extract_value(self_val, 0, "fd_struct")
                .unwrap();
            let fd = c
                .builder
                .build_extract_value(fd_inner.into_struct_value(), 0, "fd")
                .unwrap();

            let (rt_name, args): (&str, Vec<inkwell::values::BasicMetadataValueEnum>) =
                match mangled {
                    "Socket_bind" => {
                        let port = fn_val.get_nth_param(1).unwrap();
                        ("expo_socket_bind", vec![fd.into(), port.into()])
                    }
                    "Socket_listen" => {
                        let backlog = fn_val.get_nth_param(1).unwrap();
                        ("expo_socket_listen", vec![fd.into(), backlog.into()])
                    }
                    "Socket_set_reuse_addr" => ("expo_socket_setsockopt_reuse", vec![fd.into()]),
                    _ => unreachable!(),
                };

            let rt_fn = *c
                .functions
                .get(rt_name)
                .ok_or(format!("{rt_name} not declared"))?;
            let ret = c.call(rt_fn, &args, "ret").unwrap().into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, ret, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, self_val.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "Socket_accept" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd_inner = c
                .builder
                .build_extract_value(self_val, 0, "fd_struct")
                .unwrap();
            let fd = c
                .builder
                .build_extract_value(fd_inner.into_struct_value(), 0, "fd")
                .unwrap();

            let rt_fn = *c
                .functions
                .get("expo_socket_accept")
                .ok_or("expo_socket_accept not declared")?;
            let client_fd = c
                .call(rt_fn, &[fd.into()], "client_fd")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, client_fd, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let fd_struct_ty = c
                .types
                .structs
                .get("Fd")
                .copied()
                .ok_or("Fd struct type not found")?;
            let alloca = c.builder.build_alloca(fd_struct_ty, "fd_tmp").unwrap();
            let fd_desc_ptr = c
                .builder
                .build_struct_gep(fd_struct_ty, alloca, 0, "fd_desc")
                .unwrap();
            c.builder.build_store(fd_desc_ptr, client_fd).unwrap();
            let fd_val = c
                .builder
                .build_load(fd_struct_ty, alloca, "fd_val")
                .unwrap();
            let ok_result = build_result_ok(c, fd_val, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        _ => return Err(format!("unknown socket intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Debug format intrinsics
// ---------------------------------------------------------------------------

fn emit_debug_format_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    type_name: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap();

    match type_name {
        "Bool" => {
            let str_ptr = bool_to_string_ptr(c, self_val.into_int_value());
            c.builder.build_return(Some(&str_ptr)).unwrap();
        }
        "Int" | "Int8" | "Int16" | "Int32" | "UInt8" | "UInt16" | "UInt32" | "UInt64" => {
            let fmt_spec = match type_name {
                "Int" | "UInt64" => "%lld",
                "Int32" | "UInt32" => "%d",
                "Int16" | "UInt16" => "%hd",
                "Int8" | "UInt8" => "%hhd",
                _ => "%lld",
            };
            let result = emit_snprintf_to_string(c, fmt_spec, self_val);
            c.builder.build_return(Some(&result)).unwrap();
        }
        "Float" | "Float32" => {
            let f64_ty = c.context.f64_type();
            let val = if type_name == "Float32" {
                let ext = c
                    .builder
                    .build_float_ext(self_val.into_float_value(), f64_ty, "f64_ext")
                    .unwrap();
                ext.into()
            } else {
                self_val
            };
            let result = emit_snprintf_to_string(c, "%f", val);
            c.builder.build_return(Some(&result)).unwrap();
        }
        _ => return Err(format!("unknown debug format intrinsic type: {type_name}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

fn emit_snprintf_to_string<'ctx>(
    c: &mut Compiler<'ctx>,
    fmt_spec: &str,
    val: inkwell::values::BasicValueEnum<'ctx>,
) -> inkwell::values::BasicValueEnum<'ctx> {
    snprintf_to_expo_string(c, fmt_spec, &[val.into()], "dbg").into()
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

    let neg8 = i64_ty.const_int((-8i64) as u64, true);
    let hdr_ptr = unsafe {
        c.builder
            .build_gep(i8_ty, str_ptr, &[neg8], "hdr_ptr")
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
