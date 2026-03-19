//! Hash table infrastructure shared by `Map<K,V>` and `Set<T>`.
//!
//! Provides intrinsic LLVM IR emission for the `Hash` and `Equality`
//! protocol methods on primitive types, plus shared helpers for probing,
//! resizing, and calling hash/eq on arbitrary key types.

use expo_typecheck::types::{Primitive, Type};
use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

const PRIMITIVE_TYPES: &[&str] = &[
    "Bool", "Int", "Int8", "Int16", "Int32", "String", "UInt8", "UInt16", "UInt32", "UInt64",
];

pub fn is_primitive_intrinsic(mangled: &str) -> bool {
    for prim in PRIMITIVE_TYPES {
        if mangled == format!("{prim}_hash") || mangled == format!("{prim}_eq") {
            return true;
        }
    }
    false
}

pub fn emit_primitive_intrinsic<'ctx>(c: &mut Compiler<'ctx>, mangled: &str) -> Result<(), String> {
    let fn_val = *c
        .functions
        .get(mangled)
        .ok_or_else(|| format!("undeclared intrinsic: {mangled}"))?;

    if let Some(type_name) = mangled.strip_suffix("_hash") {
        emit_hash_intrinsic(c, fn_val, type_name)
    } else if let Some(type_name) = mangled.strip_suffix("_eq") {
        emit_eq_intrinsic(c, fn_val, type_name)
    } else {
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
    c.struct_types.insert(mangled.to_string(), st);
    c.mono_struct_info.insert(
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
        .builder
        .build_call(malloc, &[entries_bytes.into()], "entries")
        .unwrap()
        .try_as_basic_value()
        .left()
        .unwrap()
        .into_pointer_value();
    let states_ptr = c
        .builder
        .build_call(malloc, &[cap.into()], "states")
        .unwrap()
        .try_as_basic_value()
        .left()
        .unwrap()
        .into_pointer_value();
    let memset = *c.functions.get("memset").unwrap();
    c.builder
        .build_call(
            memset,
            &[
                states_ptr.into(),
                i32_ty.const_int(0, false).into(),
                cap.into(),
            ],
            "clear_states",
        )
        .unwrap();

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
        } => expo_typecheck::types::mangle_name(base, type_args),
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
            .builder
            .build_call(
                strcmp,
                &[self_val.into(), other_val.into()],
                "strcmp_result",
            )
            .unwrap()
            .try_as_basic_value()
            .left()
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

/// FNV-1a hash over a null-terminated C string.
fn emit_fnv1a_hash<'ctx>(
    c: &mut Compiler<'ctx>,
    str_ptr: inkwell::values::PointerValue<'ctx>,
) -> inkwell::values::BasicValueEnum<'ctx> {
    let fn_val = c.builder.get_insert_block().unwrap().get_parent().unwrap();
    let i64_ty = c.context.i64_type();
    let i8_ty = c.context.i8_type();

    let offset_basis = i64_ty.const_int(0xcbf29ce484222325, false);
    let fnv_prime = i64_ty.const_int(0x100000001b3, false);

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

    let is_null = c
        .builder
        .build_int_compare(IntPredicate::EQ, byte, i8_ty.const_int(0, false), "is_null")
        .unwrap();
    c.builder
        .build_conditional_branch(is_null, done_bb, body_bb)
        .unwrap();

    c.builder.position_at_end(body_bb);
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
