//! Hash table infrastructure shared by `Map<K,V>` and `Set<T>`.
//!
//! Provides shared helpers for probing, resizing, and calling hash/eq on
//! arbitrary key types. Intrinsic LLVM IR emission for primitive types
//! lives in the `intrinsics` module.

use expo_typecheck::types::{Primitive, Type, mangle_name};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::types::StructType;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

// ---------------------------------------------------------------------------
// Shared struct layout / method helpers for Map and Set
// ---------------------------------------------------------------------------

/// Both Map<K,V> and Set<T> use the same LLVM struct layout:
/// `{ entries_ptr: i8*, states_ptr: i8*, length: i64, capacity: i64 }`
pub fn monomorphize_hashtable_struct<'ctx>(
    compiler: &mut Compiler<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let struct_type = compiler.context.opaque_struct_type(mangled);
    let ptr_type = compiler.context.ptr_type(AddressSpace::default());
    let i64_type = compiler.context.i64_type();
    struct_type.set_body(
        &[
            ptr_type.into(),
            ptr_type.into(),
            i64_type.into(),
            i64_type.into(),
        ],
        false,
    );
    compiler
        .types
        .register_monomorphized(mangled.to_string(), struct_type);
    compiler.types.mono_struct_info.insert(
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
/// Allocates entries buffer (`capacity * entry_size` bytes) and states buffer
/// (`capacity` bytes, zeroed), returns the 4-field struct.
pub fn emit_hashtable_new<'ctx>(
    compiler: &mut Compiler<'ctx>,
    mangled_fn: &str,
    collection_struct: StructType<'ctx>,
    entry_size: u64,
) -> Result<(), String> {
    let i64_type = compiler.context.i64_type();
    let i32_type = compiler.context.i32_type();

    let fn_type = collection_struct.fn_type(&[], false);
    let fn_value = compiler.module.add_function(mangled_fn, fn_type, None);
    compiler.functions.insert(mangled_fn.to_string(), fn_value);

    let entry = compiler.context.append_basic_block(fn_value, "entry");
    let saved_block = compiler.builder.get_insert_block();
    compiler.builder.position_at_end(entry);

    let capacity = i64_type.const_int(8, false);
    let entries_bytes = compiler
        .builder
        .build_int_mul(
            capacity,
            i64_type.const_int(entry_size, false),
            "entries_bytes",
        )
        .unwrap();
    let malloc = *compiler.functions.get("malloc").unwrap();
    let entries_ptr = compiler
        .call(malloc, &[entries_bytes.into()], "entries")
        .unwrap()
        .into_pointer_value();
    let states_ptr = compiler
        .call(malloc, &[capacity.into()], "states")
        .unwrap()
        .into_pointer_value();
    let memset = *compiler.functions.get("memset").unwrap();
    compiler.call_void(
        memset,
        &[
            states_ptr.into(),
            i32_type.const_int(0, false).into(),
            capacity.into(),
        ],
        "clear_states",
    );

    let result = collection_struct.get_undef();
    let result = compiler
        .builder
        .build_insert_value(result, entries_ptr, 0, "insert_entries")
        .unwrap()
        .into_struct_value();
    let result = compiler
        .builder
        .build_insert_value(result, states_ptr, 1, "insert_states")
        .unwrap()
        .into_struct_value();
    let result = compiler
        .builder
        .build_insert_value(result, i64_type.const_int(0, false), 2, "insert_length")
        .unwrap()
        .into_struct_value();
    let result = compiler
        .builder
        .build_insert_value(result, capacity, 3, "insert_capacity")
        .unwrap()
        .into_struct_value();
    compiler.builder.build_return(Some(&result)).unwrap();

    if let Some(block) = saved_block {
        compiler.builder.position_at_end(block);
    }
    Ok(())
}

/// Emits `fn length(self) -> Int` for a hash-table-backed collection.
pub fn emit_hashtable_length<'ctx>(
    compiler: &mut Compiler<'ctx>,
    mangled_fn: &str,
    collection_struct: StructType<'ctx>,
) -> Result<(), String> {
    let i64_type = compiler.context.i64_type();

    let fn_type = i64_type.fn_type(&[collection_struct.into()], false);
    let fn_value = compiler.module.add_function(mangled_fn, fn_type, None);
    compiler.functions.insert(mangled_fn.to_string(), fn_value);

    let entry = compiler.context.append_basic_block(fn_value, "entry");
    let saved_block = compiler.builder.get_insert_block();
    compiler.builder.position_at_end(entry);

    let self_value = fn_value.get_nth_param(0).unwrap().into_struct_value();
    let length = compiler
        .builder
        .build_extract_value(self_value, 2, "length")
        .unwrap();
    compiler.builder.build_return(Some(&length)).unwrap();

    if let Some(block) = saved_block {
        compiler.builder.position_at_end(block);
    }
    Ok(())
}

/// Emits `fn empty?(self) -> Bool` for a hash-table-backed collection.
/// Returns true when field 2 (length) is zero.
pub fn emit_hashtable_empty<'ctx>(
    compiler: &mut Compiler<'ctx>,
    mangled_fn: &str,
    collection_struct: StructType<'ctx>,
) -> Result<(), String> {
    let i64_type = compiler.context.i64_type();
    let bool_type = compiler.context.bool_type();

    let fn_type = bool_type.fn_type(&[collection_struct.into()], false);
    let fn_value = compiler.module.add_function(mangled_fn, fn_type, None);
    compiler.functions.insert(mangled_fn.to_string(), fn_value);

    let entry = compiler.context.append_basic_block(fn_value, "entry");
    let saved_block = compiler.builder.get_insert_block();
    compiler.builder.position_at_end(entry);

    let self_value = fn_value.get_nth_param(0).unwrap().into_struct_value();
    let length = compiler
        .builder
        .build_extract_value(self_value, 2, "length")
        .unwrap()
        .into_int_value();
    let is_empty = compiler
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            length,
            i64_type.const_int(0, false),
            "is_empty",
        )
        .unwrap();
    compiler.builder.build_return(Some(&is_empty)).unwrap();

    if let Some(block) = saved_block {
        compiler.builder.position_at_end(block);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Hash/eq function lookup helpers
// ---------------------------------------------------------------------------

pub fn ensure_hash_fn<'ctx>(
    compiler: &Compiler<'ctx>,
    key_type: &Type,
) -> Result<FunctionValue<'ctx>, String> {
    let type_name = type_display_name(key_type);
    let fn_name = format!("{type_name}_hash");
    if let Some(function) = compiler.functions.get(&fn_name) {
        return Ok(*function);
    }
    Err(format!(
        "type `{type_name}` does not implement Hash (no `{fn_name}` found)"
    ))
}

pub fn ensure_eq_fn<'ctx>(
    compiler: &Compiler<'ctx>,
    key_type: &Type,
) -> Result<FunctionValue<'ctx>, String> {
    let type_name = type_display_name(key_type);
    let fn_name = format!("{type_name}_eq");
    if let Some(function) = compiler.functions.get(&fn_name) {
        return Ok(*function);
    }
    Err(format!(
        "type `{type_name}` does not implement Equality (no `{fn_name}` found)"
    ))
}

pub fn type_display_name(ty: &Type) -> String {
    match ty {
        Type::Primitive(primitive) => match primitive {
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
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                identifier.name.clone()
            } else {
                mangle_name(identifier, type_args)
            }
        }
        _ => format!("{ty:?}"),
    }
}
