use expo_typecheck::types::Type;
use inkwell::IntPredicate;
use inkwell::types::BasicType;
use inkwell::values::FunctionValue;

use super::STRING_HEADER_BYTES;
use crate::compiler::{Compiler, EmitResult};
use crate::types::to_llvm_type;

const CPTR_METHODS: &[&str] = &[
    "null",
    "alloc",
    "free",
    "offset",
    "read",
    "write",
    "null?",
    "to_binary",
];

pub fn is_cptr_intrinsic(mangled: &str) -> bool {
    if !mangled.starts_with("CPtr_$") {
        return false;
    }
    for method in CPTR_METHODS {
        if mangled.ends_with(&format!("_{method}")) {
            return true;
        }
    }
    false
}

fn extract_method(mangled: &str) -> &str {
    for method in CPTR_METHODS {
        if mangled.ends_with(&format!("_{method}")) {
            return method;
        }
    }
    ""
}

/// Resolves the inner type T from a mangled CPtr intrinsic name like
/// `CPtr_$UInt8$_null` and returns its LLVM type.
fn resolve_pointee_llvm_type<'ctx>(
    c: &Compiler<'ctx>,
    mangled: &str,
) -> Option<inkwell::types::BasicTypeEnum<'ctx>> {
    let after_prefix = mangled.strip_prefix("CPtr_$")?;
    let dollar_idx = after_prefix.find('$')?;
    let type_name = &after_prefix[..dollar_idx];

    let expo_ty = crate::types::primitive_name_to_type(type_name);
    to_llvm_type(&expo_ty, c.context, &c.types)
}

pub fn emit_cptr_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let method = extract_method(mangled);
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let i64_ty = c.context.i64_type();

    match method {
        "null" => {
            let null = ptr_ty.const_null();
            c.builder.build_return(Some(&null)).unwrap();
        }

        "alloc" => {
            let element_ty = resolve_pointee_llvm_type(c, mangled)
                .ok_or_else(|| format!("cannot resolve pointee type for {mangled}"))?;
            let elem_size = element_ty.size_of().unwrap_or(i64_ty.const_int(1, false));
            let count = fn_val.get_nth_param(0).unwrap().into_int_value();
            let total = c
                .builder
                .build_int_mul(count, elem_size, "total_bytes")
                .unwrap();
            let malloc = *c.functions.get("malloc").ok_or("malloc not declared")?;
            let raw = c.call(malloc, &[total.into()], "ptr").unwrap();
            c.builder.build_return(Some(&raw)).unwrap();
        }

        "free" => {
            let ptr_val = fn_val.get_nth_param(0).unwrap();
            let free_fn = *c.functions.get("free").ok_or("free not declared")?;
            c.call_void(free_fn, &[ptr_val.into()], "");
            c.builder.build_return(None).unwrap();
        }

        "offset" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let n = fn_val.get_nth_param(1).unwrap().into_int_value();
            let element_ty = resolve_pointee_llvm_type(c, mangled)
                .ok_or_else(|| format!("cannot resolve pointee type for {mangled}"))?;
            let gep = unsafe {
                c.builder
                    .build_gep(element_ty, self_ptr, &[n], "offset_ptr")
                    .unwrap()
            };
            c.builder.build_return(Some(&gep)).unwrap();
        }

        "read" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let element_ty = resolve_pointee_llvm_type(c, mangled)
                .ok_or_else(|| format!("cannot resolve pointee type for {mangled}"))?;
            let val = c
                .builder
                .build_load(element_ty, self_ptr, "read_val")
                .unwrap();
            c.builder.build_return(Some(&val)).unwrap();
        }

        "write" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let value = fn_val.get_nth_param(1).unwrap();
            c.builder.build_store(self_ptr, value).unwrap();
            c.builder.build_return(None).unwrap();
        }

        "null?" => {
            let self_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let is_null = c
                .builder
                .build_int_compare(IntPredicate::EQ, self_ptr, ptr_ty.const_null(), "is_null")
                .unwrap();
            c.builder.build_return(Some(&is_null)).unwrap();
        }

        "to_binary" => {
            let src_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();
            let byte_len = fn_val.get_nth_param(1).unwrap().into_int_value();
            let i8_ty = c.context.i8_type();
            let header_size = i64_ty.const_int(STRING_HEADER_BYTES, false);
            let malloc = *c.functions.get("malloc").ok_or("malloc not declared")?;
            let memcpy = *c.functions.get("memcpy").ok_or("memcpy not declared")?;

            let total = c
                .builder
                .build_int_add(header_size, byte_len, "total")
                .unwrap();
            let base_ptr = c
                .call(malloc, &[total.into()], "base_ptr")
                .unwrap()
                .into_pointer_value();

            let bit_len = c
                .builder
                .build_int_mul(byte_len, i64_ty.const_int(8, false), "bit_len")
                .unwrap();
            c.builder.build_store(base_ptr, bit_len).unwrap();

            let payload_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, base_ptr, &[header_size], "payload_ptr")
                    .unwrap()
            };
            c.call(
                memcpy,
                &[payload_ptr.into(), src_ptr.into(), byte_len.into()],
                "",
            );
            c.builder.build_return(Some(&payload_ptr)).unwrap();
        }

        _ => return Err(format!("unknown CPtr intrinsic method: {method}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

/// Emits `String.to_cstring()` and `CString.to_string()` intrinsics.
///
/// Expo String layout: the string pointer points to the UTF-8 **payload**.
/// At offset -8 from the pointer sits an i64 holding the **bit** length.
/// CString layout: struct { ptr: CPtr<UInt8>, len: Int } where len is byte count.
pub fn emit_cstring_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();
    let i8_ty = c.context.i8_type();

    let malloc = *c.functions.get("malloc").ok_or("malloc not declared")?;
    let memcpy = *c.functions.get("memcpy").ok_or("memcpy not declared")?;

    match mangled {
        "String_to_cstring" => {
            // param 0 = expo String ptr (points to UTF-8 payload bytes)
            let payload_ptr = fn_val.get_nth_param(0).unwrap().into_pointer_value();

            // Header is at payload_ptr - 8, stores bit length
            let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
            let hdr_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, payload_ptr, &[neg_hdr], "hdr_ptr")
                    .unwrap()
            };
            let bit_len = c
                .builder
                .build_load(i64_ty, hdr_ptr, "bit_len")
                .unwrap()
                .into_int_value();
            let byte_len = c
                .builder
                .build_int_unsigned_div(bit_len, i64_ty.const_int(8, false), "byte_len")
                .unwrap();

            // Allocate byte_len + 1 for null-terminated C string
            let one = i64_ty.const_int(1, false);
            let alloc_size = c
                .builder
                .build_int_add(byte_len, one, "alloc_size")
                .unwrap();
            let c_buf = c
                .call(malloc, &[alloc_size.into()], "c_buf")
                .unwrap()
                .into_pointer_value();

            // Copy bytes directly from the payload pointer
            c.call(
                memcpy,
                &[c_buf.into(), payload_ptr.into(), byte_len.into()],
                "",
            );

            // Write null terminator
            let null_pos = unsafe {
                c.builder
                    .build_gep(i8_ty, c_buf, &[byte_len], "null_pos")
                    .unwrap()
            };
            c.builder
                .build_store(null_pos, i8_ty.const_int(0, false))
                .unwrap();

            // Build CString struct { ptr, len }
            let cstring_ty = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let alloca = c.builder.build_alloca(cstring_ty, "cs_tmp").unwrap();
            let ptr_field = c
                .builder
                .build_struct_gep(cstring_ty, alloca, 0, "cs_ptr")
                .unwrap();
            c.builder.build_store(ptr_field, c_buf).unwrap();
            let len_field = c
                .builder
                .build_struct_gep(cstring_ty, alloca, 1, "cs_len")
                .unwrap();
            c.builder.build_store(len_field, byte_len).unwrap();

            let result = c.builder.build_load(cstring_ty, alloca, "cstring").unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }

        "CString_to_string" => {
            // param 0 = CString struct { ptr, len }
            let cs_val = fn_val.get_nth_param(0).unwrap().into_struct_value();

            let c_ptr = c
                .builder
                .build_extract_value(cs_val, 0, "c_ptr")
                .unwrap()
                .into_pointer_value();
            let byte_len = c
                .builder
                .build_extract_value(cs_val, 1, "byte_len")
                .unwrap()
                .into_int_value();

            // Allocate: 8-byte header + bytes
            let header_size = i64_ty.const_int(STRING_HEADER_BYTES, false);
            let total = c
                .builder
                .build_int_add(header_size, byte_len, "total")
                .unwrap();
            let base_ptr = c
                .call(malloc, &[total.into()], "base_ptr")
                .unwrap()
                .into_pointer_value();

            // Write bit-length header
            let bit_len = c
                .builder
                .build_int_mul(byte_len, i64_ty.const_int(8, false), "bit_len")
                .unwrap();
            c.builder.build_store(base_ptr, bit_len).unwrap();

            // Payload pointer is base + 8
            let payload_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, base_ptr, &[header_size], "payload_ptr")
                    .unwrap()
            };
            c.call(
                memcpy,
                &[payload_ptr.into(), c_ptr.into(), byte_len.into()],
                "",
            );

            // Return the payload pointer (not the base)
            c.builder.build_return(Some(&payload_ptr)).unwrap();
        }

        _ => return Err(format!("unknown CString intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

/// Declares and emits a monomorphized CPtr method. Called from the generic
/// monomorphization path for `CPtr<T>` method calls.
pub fn emit_cptr_method<'ctx>(
    c: &mut Compiler<'ctx>,
    _mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    if !CPTR_METHODS.contains(&method_name) {
        return Ok(EmitResult::NotIntrinsic);
    }
    if c.functions.contains_key(mangled_fn) {
        return Ok(EmitResult::Emitted);
    }

    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let i64_ty = c.context.i64_type();
    let bool_ty = c.context.bool_type();
    let void_ty = c.context.void_type();

    let inner_ty = type_args.first().ok_or("CPtr needs a type argument")?;
    let inner_llvm = to_llvm_type(inner_ty, c.context, &c.types);

    let fn_type = match method_name {
        "null" => ptr_ty.fn_type(&[], false),
        "alloc" => ptr_ty.fn_type(&[i64_ty.into()], false),
        "free" => void_ty.fn_type(&[ptr_ty.into()], false),
        "offset" => ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false),
        "read" => {
            let ret = inner_llvm.ok_or("CPtr.read: cannot resolve pointee LLVM type")?;
            ret.fn_type(&[ptr_ty.into()], false)
        }
        "write" => {
            let val = inner_llvm.ok_or("CPtr.write: cannot resolve pointee LLVM type")?;
            void_ty.fn_type(&[ptr_ty.into(), val.into()], false)
        }
        "null?" => bool_ty.fn_type(&[ptr_ty.into()], false),
        "to_binary" => ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false),
        _ => return Ok(EmitResult::NotIntrinsic),
    };

    let fn_val = c.module.add_function(mangled_fn, fn_type, None);
    c.functions.insert(mangled_fn.to_string(), fn_val);

    emit_cptr_intrinsic(c, fn_val, mangled_fn)?;
    Ok(EmitResult::Emitted)
}
