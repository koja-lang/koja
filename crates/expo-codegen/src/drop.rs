//! Drop insertion: emits cleanup calls for live move-type variables at scope
//! boundaries. Calls `free()` for heap-allocated types when they go out of
//! scope.

use inkwell::values::PointerValue;

use expo_typecheck::types::{Primitive, Type};

use crate::compiler::Compiler;

/// Tracks whether a variable owns its backing memory and is responsible for
/// freeing it. Used to distinguish heap-allocated strings (interpolated,
/// received from mailbox) from static/global string pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    Unowned,
}

/// Emits drop calls for all live move-type variables in reverse declaration
/// order. Called before function returns and at scope exits.
pub fn drop_live_variables(c: &mut Compiler) {
    let vars: Vec<(PointerValue, Type, Ownership)> = c
        .variables
        .iter()
        .map(|(_, (ptr, ty, own))| (*ptr, ty.clone(), *own))
        .collect();

    for (ptr, ty, ownership) in vars.iter().rev() {
        if matches!(ty, Type::Function { .. }) {
            emit_drop_closure(c, *ptr);
            continue;
        }
        if ty.is_copy() {
            continue;
        }
        if matches!(ty, Type::Primitive(Primitive::String)) && *ownership == Ownership::Owned {
            let free_fn = *c
                .functions
                .get("free")
                .expect("free not declared in builtins");
            let str_val = c
                .builder
                .build_load(
                    c.context.ptr_type(inkwell::AddressSpace::default()),
                    *ptr,
                    "str_drop",
                )
                .unwrap();
            c.builder
                .build_call(free_fn, &[str_val.into()], "drop_free_str")
                .unwrap();
            continue;
        }
        emit_drop(c, *ptr, ty);
    }
}

fn is_list_type(ty: &Type) -> bool {
    match ty {
        Type::GenericInstance { base, .. } if base == "List" => true,
        Type::Struct(name) if name.starts_with("List_$") => true,
        _ => false,
    }
}

fn is_map_type(ty: &Type) -> bool {
    match ty {
        Type::GenericInstance { base, .. } if base == "Map" => true,
        Type::Struct(name) if name.starts_with("Map_$") => true,
        _ => false,
    }
}

fn is_set_type(ty: &Type) -> bool {
    match ty {
        Type::GenericInstance { base, .. } if base == "Set" => true,
        Type::Struct(name) if name.starts_with("Set_$") => true,
        _ => false,
    }
}

/// Returns true if a type requires heap deallocation at scope exit.
fn needs_heap_drop(ty: &Type) -> bool {
    is_list_type(ty) || is_map_type(ty) || is_set_type(ty)
}

fn emit_drop(c: &mut Compiler, ptr: PointerValue, ty: &Type) {
    if !needs_heap_drop(ty) {
        return;
    }

    if is_list_type(ty) {
        emit_drop_list(c, ptr, ty);
        return;
    }

    if is_map_type(ty) || is_set_type(ty) {
        emit_drop_hash_collection(c, ptr, ty);
        return;
    }

    let free = *c
        .functions
        .get("free")
        .expect("free not declared in builtins");
    let val = c
        .builder
        .build_load(
            c.context.ptr_type(inkwell::AddressSpace::default()),
            ptr,
            "drop_load",
        )
        .unwrap();
    c.builder
        .build_call(free, &[val.into()], "drop_free")
        .unwrap();
}

/// Drops a List value: extracts the data pointer (field 0) and frees it.
fn emit_drop_list(c: &mut Compiler, alloca: PointerValue, ty: &Type) {
    let mangled = match ty {
        Type::GenericInstance {
            base, type_args, ..
        } => expo_typecheck::types::mangle_name(base, type_args),
        Type::Struct(name) => name.clone(),
        _ => return,
    };
    let Some(list_struct) = c.struct_types.get(&mangled).copied() else {
        return;
    };

    let list_val = c
        .builder
        .build_load(list_struct, alloca, "drop_list_load")
        .unwrap()
        .into_struct_value();
    let data_ptr = c
        .builder
        .build_extract_value(list_val, 0, "drop_list_ptr")
        .unwrap();

    let free = *c
        .functions
        .get("free")
        .expect("free not declared in builtins");
    c.builder
        .build_call(free, &[data_ptr.into()], "drop_list_free")
        .unwrap();
}

/// Drops a Map or Set value: frees entries_ptr (field 0) and states_ptr (field 1).
fn emit_drop_hash_collection(c: &mut Compiler, alloca: PointerValue, ty: &Type) {
    let mangled = match ty {
        Type::GenericInstance {
            base, type_args, ..
        } => expo_typecheck::types::mangle_name(base, type_args),
        Type::Struct(name) => name.clone(),
        _ => return,
    };
    let Some(coll_struct) = c.struct_types.get(&mangled).copied() else {
        return;
    };

    let coll_val = c
        .builder
        .build_load(coll_struct, alloca, "drop_coll_load")
        .unwrap()
        .into_struct_value();
    let entries_ptr = c
        .builder
        .build_extract_value(coll_val, 0, "drop_entries_ptr")
        .unwrap();
    let states_ptr = c
        .builder
        .build_extract_value(coll_val, 1, "drop_states_ptr")
        .unwrap();

    let free = *c
        .functions
        .get("free")
        .expect("free not declared in builtins");
    c.builder
        .build_call(free, &[entries_ptr.into()], "drop_free_entries")
        .unwrap();
    c.builder
        .build_call(free, &[states_ptr.into()], "drop_free_states")
        .unwrap();
}

/// Drops a closure fat pointer: extracts env_ptr, null-checks, and frees.
fn emit_drop_closure(c: &mut Compiler, alloca: PointerValue) {
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let closure_struct_ty = c
        .context
        .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);

    let fat_ptr = c
        .builder
        .build_load(closure_struct_ty, alloca, "drop_closure_load")
        .unwrap()
        .into_struct_value();

    let env_ptr = c
        .builder
        .build_extract_value(fat_ptr, 1, "drop_env_ptr")
        .unwrap()
        .into_pointer_value();

    let is_null = c.builder.build_is_null(env_ptr, "env_is_null").unwrap();

    let current_fn = c.builder.get_insert_block().unwrap().get_parent().unwrap();
    let free_bb = c.context.append_basic_block(current_fn, "drop_free_env");
    let cont_bb = c.context.append_basic_block(current_fn, "drop_cont");

    c.builder
        .build_conditional_branch(is_null, cont_bb, free_bb)
        .unwrap();

    c.builder.position_at_end(free_bb);
    let free = *c
        .functions
        .get("free")
        .expect("free not declared in builtins");
    c.builder
        .build_call(free, &[env_ptr.into()], "free_env")
        .unwrap();
    c.builder.build_unconditional_branch(cont_bb).unwrap();

    c.builder.position_at_end(cont_bb);
}
