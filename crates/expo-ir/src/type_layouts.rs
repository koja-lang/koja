//! Type layout tables: LLVM-free semantic information about monomorphized
//! types, used during lowering and emission.
//!
//! Sister to [`crate::FnLowerState`]: `TypeLayouts` is the type-scoped
//! semantic store, `FnLowerState` is the function-scoped one. Both are
//! borrowed through [`crate::lower::LowerCtx`] so lowering functions can
//! run as free functions in [`crate::lower`] without reaching into the
//! `Compiler<'ctx>` god-object.
//!
//! The companion LLVM-only cache (`LLVMTypeCache` in `expo-codegen`)
//! holds the `inkwell::StructType<'ctx>` handles for the same set of
//! monomorphized keys. Keep the two in sync at registration time; never
//! reach across from one to the other at lookup time.

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

    /// Whether `mangled` is registered as a monomorphized struct or enum.
    /// Mirrors `LLVMTypeCache::contains_monomorphized` for lowering paths
    /// that need to know "have we seen this generic instantiation?" without
    /// reaching into the LLVM-bound cache.
    pub fn contains_monomorphized(&self, mangled: &str) -> bool {
        self.mono_enum_variants.contains_key(mangled) || self.mono_struct_info.contains_key(mangled)
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
    /// Subsequent inserts overwrite.
    pub fn register_enum_variants(
        &mut self,
        mangled: String,
        variants: Vec<(String, VariantData)>,
    ) {
        self.mono_enum_variants.insert(mangled, variants);
    }

    /// Record the field layout for a monomorphized struct under `mangled`.
    /// Subsequent inserts overwrite.
    pub fn register_struct_layout(&mut self, mangled: String, fields: Vec<(String, Type)>) {
        self.mono_struct_info.insert(mangled, fields);
    }

    /// Borrow the field layout for `mangled`, if registered.
    pub fn struct_layout(&self, mangled: &str) -> Option<&[(String, Type)]> {
        self.mono_struct_info.get(mangled).map(Vec::as_slice)
    }

    /// 0-based position of `variant` within the enum registered under
    /// `mangled`, equal to the tag value used at codegen.
    pub fn variant_index(&self, mangled: &str, variant: &str) -> Option<u8> {
        let variants = self.mono_enum_variants.get(mangled)?;
        variants
            .iter()
            .position(|(n, _)| n == variant)
            .map(|i| i as u8)
    }
}
