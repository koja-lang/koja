pub(crate) mod cptr;
mod format;
mod hash;
mod io;
mod socket;
mod string;
mod system;

use crate::compiler::Compiler;

/// Tag value for `Result.Ok` in the tagged-union layout.
pub(crate) const RESULT_OK_TAG: u64 = 0;
/// Tag value for `Result.Err` in the tagged-union layout.
pub(crate) const RESULT_ERR_TAG: u64 = 1;
/// Tag value for `Option.Some` in the tagged-union layout.
pub(crate) const OPTION_SOME_TAG: u64 = 0;
/// Tag value for `Option.None` in the tagged-union layout.
pub(crate) const OPTION_NONE_TAG: u64 = 1;
/// Size in bytes of the length header prepended to String/Binary payloads.
pub(crate) const STRING_HEADER_BYTES: u64 = 8;

use self::cptr::{emit_cptr_intrinsic, emit_cstring_intrinsic, is_cptr_intrinsic};
use self::format::emit_debug_format_intrinsic;
use self::hash::{emit_bitwise_intrinsic, emit_eq_intrinsic, emit_hash_intrinsic};
use self::io::{emit_fd_intrinsic, emit_file_intrinsic};
use self::socket::emit_socket_intrinsic;
use self::string::{emit_conversion_intrinsic, emit_parse_intrinsic, emit_string_intrinsic};
use self::system::{emit_kernel_intrinsic, emit_random_intrinsic};

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
    "Binary_byte_size",
    "Binary_ptr",
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
    "Float32", "Binary", "Bits",
];

const PARSE_INTRINSICS: &[&str] = &["Int_parse", "Float_parse"];

const FD_INTRINSICS: &[&str] = &["Fd_read", "Fd_watch", "Fd_unwatch"];

const FILE_INTRINSICS: &[&str] = &[
    "File_open",
    "File_read",
    "File_write",
    "File_exists?",
    "File_delete",
    "File_rename",
];

const SOCKET_INTRINSICS: &[&str] = &[
    "Socket_create",
    "Socket_bind",
    "Socket_connect",
    "Socket_listen",
    "Socket_accept",
    "Socket_try_accept_raw",
    "Socket_set_reuse_addr",
    "Socket_resolve",
    "Socket_send_to",
    "Socket_recv_from",
];

const RANDOM_INTRINSICS: &[&str] = &["Random_bytes"];

const KERNEL_INTRINSICS: &[&str] = &["Kernel_exit"];

const CSTRING_INTRINSICS: &[&str] = &["String_to_cstring", "CString_to_string"];

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
        || RANDOM_INTRINSICS.contains(&mangled)
        || KERNEL_INTRINSICS.contains(&mangled)
        || is_cptr_intrinsic(mangled)
        || CSTRING_INTRINSICS.contains(&mangled)
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

    if RANDOM_INTRINSICS.contains(&mangled) {
        return emit_random_intrinsic(c, fn_val, mangled);
    }

    if KERNEL_INTRINSICS.contains(&mangled) {
        return emit_kernel_intrinsic(c, fn_val, mangled);
    }

    if is_cptr_intrinsic(mangled) {
        return emit_cptr_intrinsic(c, fn_val, mangled);
    }

    if CSTRING_INTRINSICS.contains(&mangled) {
        return emit_cstring_intrinsic(c, fn_val, mangled);
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

/// Constructs a `Result.Ok(value)` struct: tag=0, payload=value.
pub(crate) fn build_result_ok<'ctx>(
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
        .build_store(tag_ptr, c.context.i8_type().const_int(RESULT_OK_TAG, false))
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
pub(crate) fn build_result_err<'ctx>(
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
        .build_store(
            tag_ptr,
            c.context.i8_type().const_int(RESULT_ERR_TAG, false),
        )
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

pub use crate::hashtable::type_display_name;
