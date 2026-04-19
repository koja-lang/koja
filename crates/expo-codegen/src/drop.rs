//! Drop insertion: emits cleanup calls for live move-type variables at scope
//! boundaries. Calls `free()` for heap-allocated types when they go out of
//! scope.

use inkwell::values::PointerValue;

use expo_ast::identifier::{Package, TypeIdentifier};
use expo_ir::lower::fields::resolve_indirect_field_indices;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Primitive, Type, mangle_name};

use crate::compiler::Compiler;
use crate::types::to_llvm_type;

/// Tracks whether a variable owns its backing memory and is responsible for
/// freeing it. Used to distinguish heap-allocated strings (interpolated,
/// received from mailbox) from static/global string pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    Unowned,
}

/// Emits drop calls for all live move-type variables in reverse declaration
/// order. Called before function returns and at scope exits. When `skip` is
/// `Some(name)`, the variable with that name is excluded — its ownership is
/// being transferred to the caller via `return`.
pub fn drop_live_variables<'ctx>(c: &mut Compiler<'ctx>, skip: Option<&str>) {
    let vars: Vec<(String, PointerValue<'ctx>, Type, Ownership)> = c
        .fn_state
        .variables
        .iter()
        .map(|(name, (ptr, ty, own))| (name.clone(), *ptr, ty.clone(), *own))
        .collect();

    for (name, ptr, ty, ownership) in vars.iter().rev() {
        if skip == Some(name.as_str()) {
            continue;
        }
        if matches!(ty, Type::Function { .. }) {
            if *ownership == Ownership::Owned {
                emit_drop_closure(c, *ptr);
            }
            continue;
        }
        if ty.is_copy() {
            continue;
        }
        if matches!(
            ty,
            Type::Primitive(Primitive::String)
                | Type::Primitive(Primitive::Binary)
                | Type::Primitive(Primitive::Bits)
        ) && *ownership == Ownership::Owned
        {
            let free_fn = *c
                .functions
                .get("free")
                .expect("free not declared in builtins");
            let i8_type = c.context.i8_type();
            let i64_type = c.context.i64_type();
            let payload_ptr = c
                .builder
                .build_load(
                    c.context.ptr_type(inkwell::AddressSpace::default()),
                    *ptr,
                    "heap_drop",
                )
                .unwrap()
                .into_pointer_value();
            let base_ptr = unsafe {
                c.builder
                    .build_gep(
                        i8_type,
                        payload_ptr,
                        &[i64_type.const_int((-8i64) as u64, true)],
                        "heap_base",
                    )
                    .unwrap()
            };
            c.call_void(free_fn, &[base_ptr.into()], "drop_free_heap");
            continue;
        }
        if *ownership == Ownership::Owned {
            emit_drop(c, *ptr, ty);
        }
    }
}

fn is_list_type(ty: &Type) -> bool {
    match ty {
        Type::Named { identifier, .. } if identifier.name == "List" => true,
        Type::Named { identifier, .. } if identifier.name.starts_with("List_$") => true,
        _ => false,
    }
}

fn is_map_type(ty: &Type) -> bool {
    match ty {
        Type::Named { identifier, .. } if identifier.name == "Map" => true,
        Type::Named { identifier, .. } if identifier.name.starts_with("Map_$") => true,
        _ => false,
    }
}

fn is_set_type(ty: &Type) -> bool {
    match ty {
        Type::Named { identifier, .. } if identifier.name == "Set" => true,
        Type::Named { identifier, .. } if identifier.name.starts_with("Set_$") => true,
        _ => false,
    }
}

/// Returns true if a type requires heap deallocation at scope exit.
fn needs_heap_drop(c: &Compiler, ty: &Type) -> bool {
    is_list_type(ty) || is_map_type(ty) || is_set_type(ty) || has_indirect_fields(c, ty)
}

/// Checks whether a struct or enum type contains any [`Type::Indirect`] fields.
fn has_indirect_fields(c: &Compiler, ty: &Type) -> bool {
    match ty {
        Type::Indirect(_) => true,
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let mangled = mangle_name(identifier, type_args);
            has_indirect_fields_by_mono(c, &mangled) || has_indirect_fields_by_id(c, identifier)
        }
        Type::Named { identifier, .. } => has_indirect_fields_by_id(c, identifier),
        _ => false,
    }
}

/// Lookup by a monomorphized name (generics only). Only consults the keyed
/// tables; typecheck's bare-name resolution is handled by `by_id` for
/// non-generics.
fn has_indirect_fields_by_mono(c: &Compiler, mangled: &str) -> bool {
    if let Some(fields) = c.layouts.struct_layout(mangled) {
        return fields
            .iter()
            .any(|(_, fty)| matches!(fty, Type::Indirect(_)));
    }
    if let Some(variants) = c.layouts.enum_variants(mangled) {
        return variants
            .iter()
            .any(|(_, vdata)| variant_has_indirect(vdata));
    }
    false
}

/// Strict, TypeIdentifier-keyed indirect-field check for non-generic types.
/// Skips bare-name lookups entirely so cross-package collisions can't return
/// a foreign package's layout by accident.
fn has_indirect_fields_by_id(c: &Compiler, id: &TypeIdentifier) -> bool {
    if id.package == Package::Unresolved {
        return false;
    }
    let qualified = id.qualified_name();
    if let Some(fields) = c.layouts.struct_layout(&qualified) {
        return fields
            .iter()
            .any(|(_, fty)| matches!(fty, Type::Indirect(_)));
    }
    if let Some(info) = c.type_ctx.get_type(id) {
        if let Some(fields) = info.fields() {
            return fields
                .iter()
                .any(|(_, fty)| matches!(fty, Type::Indirect(_)));
        }
        if let Some(vs) = info.variants() {
            return vs.iter().any(|v| variant_has_indirect(&v.data));
        }
    }
    false
}

fn variant_has_indirect(vdata: &VariantData) -> bool {
    match vdata {
        VariantData::Tuple(types) => types.iter().any(|t| matches!(t, Type::Indirect(_))),
        VariantData::Struct(fields) => fields.iter().any(|(_, t)| matches!(t, Type::Indirect(_))),
        VariantData::Unit => false,
    }
}

fn emit_drop<'ctx>(c: &mut Compiler<'ctx>, ptr: PointerValue<'ctx>, ty: &Type) {
    if !needs_heap_drop(c, ty) {
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

    if has_indirect_fields(c, ty) {
        emit_drop_indirect_fields(c, ptr, ty);
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
    c.call_void(free, &[val.into()], "drop_free");
}

/// Frees heap pointers for each [`Type::Indirect`] field in a struct.
/// Handles the first level of indirection; deeper recursive nodes are freed
/// when they themselves go out of scope or are explicitly dropped.
fn emit_drop_indirect_fields<'ctx>(c: &mut Compiler<'ctx>, alloca: PointerValue<'ctx>, ty: &Type) {
    let indirect_fields = resolve_indirect_field_indices(&c.lower_ctx(), ty);
    if indirect_fields.is_empty() {
        return;
    }

    let free_fn = *c
        .functions
        .get("free")
        .expect("free not declared in builtins");
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let Some(struct_type) =
        to_llvm_type(ty, c.context, &c.llvm_types).map(|t| t.into_struct_type())
    else {
        return;
    };

    for (idx, _field_ty) in &indirect_fields {
        let field_ptr = c
            .builder
            .build_struct_gep(
                struct_type,
                alloca,
                *idx as u32,
                &format!("drop_field_{idx}"),
            )
            .unwrap();
        let heap_ptr = c
            .builder
            .build_load(ptr_ty, field_ptr, &format!("drop_heap_{idx}"))
            .unwrap();
        c.call_void(free_fn, &[heap_ptr.into()], &format!("drop_free_{idx}"));
    }
}

/// Drops a List value: extracts the data pointer (field 0) and frees it.
fn emit_drop_list<'ctx>(c: &mut Compiler<'ctx>, alloca: PointerValue<'ctx>, ty: &Type) {
    let Some(list_struct) =
        to_llvm_type(ty, c.context, &c.llvm_types).map(|t| t.into_struct_type())
    else {
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
    c.call_void(free, &[data_ptr.into()], "drop_list_free");
}

/// Drops a Map or Set value: frees entries_ptr (field 0) and states_ptr (field 1).
fn emit_drop_hash_collection<'ctx>(c: &mut Compiler<'ctx>, alloca: PointerValue<'ctx>, ty: &Type) {
    let Some(coll_struct) =
        to_llvm_type(ty, c.context, &c.llvm_types).map(|t| t.into_struct_type())
    else {
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
    c.call_void(free, &[entries_ptr.into()], "drop_free_entries");
    c.call_void(free, &[states_ptr.into()], "drop_free_states");
}

/// Drops a closure fat pointer: extracts env_ptr, null-checks, and frees.
fn emit_drop_closure<'ctx>(c: &mut Compiler<'ctx>, alloca: PointerValue<'ctx>) {
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
    c.call_void(free, &[env_ptr.into()], "free_env");
    c.builder.build_unconditional_branch(cont_bb).unwrap();

    c.builder.position_at_end(cont_bb);
}
