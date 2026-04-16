pub(crate) mod cptr;
mod format;
mod hash;
mod socket;
mod string;
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
use self::socket::emit_socket_intrinsic;
use self::string::{emit_conversion_intrinsic, emit_parse_intrinsic, emit_string_intrinsic};
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

const SOCKET_INTRINSICS: &[&str] = &["Socket_resolve", "Socket_recv_from"];

const CSTRING_INTRINSICS: &[&str] = &["String_to_cstring", "CString_to_string"];

/// Strips a leading `package.` prefix from a mangled symbol so intrinsic
/// dispatch can match on the bare `Type_method` form regardless of which
/// package declared the type. `Int_hash` stays `Int_hash`; `net.Socket_resolve`
/// becomes `Socket_resolve`.
fn intrinsic_key(mangled: &str) -> &str {
    mangled
        .split_once('.')
        .map(|(_, rest)| rest)
        .unwrap_or(mangled)
}

pub fn is_primitive_intrinsic(mangled: &str) -> bool {
    let key = intrinsic_key(mangled);
    for prim in PRIMITIVE_TYPES {
        if key == format!("{prim}_hash") || key == format!("{prim}_eq") {
            return true;
        }
    }
    for prim in BITWISE_TYPES {
        for op in BITWISE_OPS {
            if key == format!("{prim}_{op}") {
                return true;
            }
        }
    }
    for prim in DEBUG_TYPES {
        if key == format!("{prim}_format") {
            return true;
        }
    }
    if CONVERSION_INTRINSICS.contains(&key)
        || STRING_INTRINSICS.contains(&key)
        || PARSE_INTRINSICS.contains(&key)
        || SOCKET_INTRINSICS.contains(&key)
        || is_cptr_intrinsic(key)
        || CSTRING_INTRINSICS.contains(&key)
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
    let key = intrinsic_key(mangled);

    if CONVERSION_INTRINSICS.contains(&key) {
        return emit_conversion_intrinsic(c, fn_val, key);
    }

    if STRING_INTRINSICS.contains(&key) {
        return emit_string_intrinsic(c, fn_val, key);
    }

    if PARSE_INTRINSICS.contains(&key) {
        return emit_parse_intrinsic(c, fn_val, key);
    }

    if SOCKET_INTRINSICS.contains(&key) {
        return emit_socket_intrinsic(c, fn_val, key);
    }

    if is_cptr_intrinsic(key) {
        return emit_cptr_intrinsic(c, fn_val, key);
    }

    if CSTRING_INTRINSICS.contains(&key) {
        return emit_cstring_intrinsic(c, fn_val, key);
    }

    if let Some(type_name) = key.strip_suffix("_format") {
        return emit_debug_format_intrinsic(c, fn_val, type_name);
    }

    if let Some(type_name) = key.strip_suffix("_hash") {
        emit_hash_intrinsic(c, fn_val, type_name)
    } else if let Some(type_name) = key.strip_suffix("_eq") {
        emit_eq_intrinsic(c, fn_val, type_name)
    } else {
        for op in BITWISE_OPS {
            if let Some(type_name) = key.strip_suffix(&format!("_{op}")) {
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
