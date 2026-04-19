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
//! - **Wave 1** — relocated `mono_struct_info` from `TypeRegistry` and folded
//!   the two field-lookup helpers (`field_index`, `field_type`) onto
//!   `TypeLayouts` as inherent methods.
//! - **Wave 2 (current)** — added the `expo-typecheck` dep and relocated
//!   `mono_enum_variants`. The new methods (`register_enum_variants`,
//!   `enum_variants`, `contains_enum`) cover every former direct-field
//!   reader. Companion change: `Compiler::struct_field_lookup` and the two
//!   `concrete_field_*` helpers move into `crate::lower::fields` as the
//!   first true lowering functions hosted in `expo-ir`.
//! - **Wave 3+** — handled in `expo-codegen` (LLVM-only registry rename,
//!   then the `enum_variant_payloads` split, then `FnState`).

use std::collections::HashMap;

use expo_ast::types::Type;
use expo_typecheck::context::VariantData;

/// LLVM-free semantic layouts for monomorphized types. Populated during
/// type registration; consulted during lowering and emission whenever code
/// needs to know the field order or field/variant types of a generic
/// instantiation.
#[derive(Default)]
pub struct TypeLayouts {
    /// Per-mangled-key variant list for monomorphized enums. The order is
    /// the variant declaration order (and therefore the tag order).
    mono_enum_variants: HashMap<String, Vec<(String, VariantData)>>,
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

    /// Whether `mangled` is registered as a monomorphized enum.
    pub fn contains_enum(&self, mangled: &str) -> bool {
        self.mono_enum_variants.contains_key(mangled)
    }

    /// Borrow the variant list for `mangled`, if registered. Returned in
    /// declaration / tag order.
    pub fn enum_variants(&self, mangled: &str) -> Option<&[(String, VariantData)]> {
        self.mono_enum_variants.get(mangled).map(Vec::as_slice)
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

    /// Record the variant list for a monomorphized enum under `mangled`.
    /// Subsequent inserts overwrite, matching the pre-migration behaviour
    /// of direct `HashMap::insert` calls.
    pub fn register_enum_variants(
        &mut self,
        mangled: String,
        variants: Vec<(String, VariantData)>,
    ) {
        self.mono_enum_variants.insert(mangled, variants);
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
}
