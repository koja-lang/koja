//! Hash table infrastructure shared by `Map<K,V>` and `Set<T>`.
//!
//! Provides shared helpers for probing, resizing, and calling hash/eq on
//! arbitrary key types. Intrinsic LLVM IR emission for primitive types
//! lives in the `intrinsics` module.

use expo_typecheck::types::{Primitive, Type, mangle_name};
use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

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
        Type::Enum(name) | Type::Struct(name) => name.clone(),
        Type::GenericInstance {
            base, type_args, ..
        } => mangle_name(base, type_args),
        _ => format!("{ty:?}"),
    }
}
