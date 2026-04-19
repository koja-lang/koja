//! Type layout tables: LLVM-free semantic information about monomorphized
//! types, used during lowering and emission.
//!
//! This is the destination crate for the multi-wave migration of
//! `expo-codegen`'s `TypeRegistry`. `TypeRegistry` historically conflated
//! semantic layout data (which fields a struct has, in what order, with what
//! Expo types) with backend-specific caches (the `inkwell::StructType<'ctx>`
//! handles). The semantic side belongs in `expo-ir` so future lowering
//! functions can take `&TypeLayouts` instead of borrowing the whole
//! `Compiler<'ctx>`.
//!
//! ## Migration waves
//!
//! - **Wave 1 (this module's first contents)** — relocate `mono_struct_info`
//!   from `TypeRegistry`. Pure semantic data (`HashMap<String, Vec<(String,
//!   Type)>>`). The two field-lookup helpers that previously lived on
//!   `Compiler` (`get_mono_field_index`, `get_mono_field_type`) move here as
//!   inherent methods on `TypeLayouts`; the `Compiler` versions become
//!   one-line wrappers.
//! - **Wave 2** — add `expo-typecheck` dep and relocate `mono_enum_variants`;
//!   lift `Compiler::struct_field_lookup` into a free function in `expo-ir`.
//! - **Wave 3+** — handled in `expo-codegen`.

use std::collections::HashMap;

use expo_ast::types::Type;

/// LLVM-free semantic layouts for monomorphized types. Populated during
/// type registration; consulted during lowering and emission whenever code
/// needs to know the field order or field types of a generic instantiation.
#[derive(Default)]
pub struct TypeLayouts {
    /// Per-mangled-key field layout: `Vec<(field_name, field_type)>` in
    /// declaration (and therefore GEP-index) order. The key matches what
    /// `expo-codegen`'s monomorphization registers (e.g. `"List_$Int32$"`
    /// for generics, the package-qualified name for non-generics that opt
    /// into mono-style lookup).
    mono_struct_info: HashMap<String, Vec<(String, Type)>>,
}

impl TypeLayouts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the field layout for a monomorphized struct under `mangled`.
    /// Subsequent inserts overwrite, matching the pre-migration behaviour
    /// of direct `HashMap::insert` calls.
    pub fn register_struct_layout(&mut self, mangled: String, fields: Vec<(String, Type)>) {
        self.mono_struct_info.insert(mangled, fields);
    }

    /// Borrow the field layout for `mangled`, if registered.
    pub fn struct_layout(&self, mangled: &str) -> Option<&[(String, Type)]> {
        self.mono_struct_info.get(mangled).map(Vec::as_slice)
    }

    /// Field index for `field_name` in the struct registered under `mangled`.
    /// Returns `None` if either the struct or the field is unknown.
    pub fn field_index(&self, mangled: &str, field_name: &str) -> Option<u32> {
        let fields = self.mono_struct_info.get(mangled)?;
        fields
            .iter()
            .position(|(name, _)| name == field_name)
            .map(|i| i as u32)
    }

    /// Field type for `field_name` in the struct registered under `mangled`.
    pub fn field_type(&self, mangled: &str, field_name: &str) -> Option<Type> {
        let fields = self.mono_struct_info.get(mangled)?;
        fields
            .iter()
            .find(|(name, _)| name == field_name)
            .map(|(_, ty)| ty.clone())
    }
}
