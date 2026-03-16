//! Drop insertion: emits cleanup calls for live move-type variables at scope
//! boundaries.  Currently a no-op for most types because the codegen
//! stack-allocates everything; the infrastructure is in place for when
//! heap allocation (String, collections) is added.

use expo_typecheck::types::Type;

use crate::compiler::Compiler;

/// Emits drop calls for all live move-type variables in reverse declaration
/// order.  Called before function returns and at scope exits.
///
/// For now this is a stub — nothing in the current codegen is heap-allocated,
/// so there is nothing to free.  When `String` becomes a heap-allocated type
/// and collections land, this function will emit `free()` (or destructor
/// dispatch through protocols).
pub fn drop_live_variables(c: &mut Compiler) {
    let vars: Vec<(String, Type)> = c
        .variables
        .iter()
        .map(|(name, (_, ty))| (name.clone(), ty.clone()))
        .collect();

    for (_name, ty) in vars.iter().rev() {
        if ty.is_copy() {
            continue;
        }
        emit_drop(c, ty);
    }
}

fn emit_drop(_c: &mut Compiler, _ty: &Type) {
    // Stub: no heap-allocated types yet.
    // When String becomes heap-allocated: call free() on the pointer.
    // When protocols land: dispatch through a Drop protocol.
}
