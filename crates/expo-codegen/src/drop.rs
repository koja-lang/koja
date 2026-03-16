//! Drop insertion: emits cleanup calls for live move-type variables at scope
//! boundaries. Calls `free()` for heap-allocated types when they go out of
//! scope.

use inkwell::values::PointerValue;

use expo_typecheck::types::Type;

use crate::compiler::Compiler;

/// Emits drop calls for all live move-type variables in reverse declaration
/// order. Called before function returns and at scope exits.
pub fn drop_live_variables(c: &mut Compiler) {
    let vars: Vec<(PointerValue, Type)> = c
        .variables
        .iter()
        .map(|(_, (ptr, ty))| (*ptr, ty.clone()))
        .collect();

    for (ptr, ty) in vars.iter().rev() {
        if matches!(ty, Type::Function { .. }) {
            emit_drop_closure(c, *ptr);
            continue;
        }
        if ty.is_copy() {
            continue;
        }
        emit_drop(c, *ptr, ty);
    }
}

/// Returns true if a type requires heap deallocation at scope exit.
fn needs_heap_drop(_ty: &Type) -> bool {
    false
}

fn emit_drop(c: &mut Compiler, ptr: PointerValue, ty: &Type) {
    if !needs_heap_drop(ty) {
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
