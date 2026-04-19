//! Type layout tables: LLVM-free semantic information about monomorphized
//! types, used during lowering and emission.
//!
//! This is the destination crate for the multi-wave migration of
//! `expo-codegen`'s historical `TypeRegistry` (now `LLVMTypeCache`).
//! `TypeRegistry` conflated semantic layout data (which fields a struct has,
//! in what order, with what Expo types) with backend-specific caches (the
//! `inkwell::StructType<'ctx>` handles). The semantic side belongs in
//! `expo-ir` so future lowering functions can take `&TypeLayouts` instead of
//! borrowing the whole `Compiler<'ctx>`.
//!
//! ## Migration waves
//!
//! - **Wave 1** — relocated `mono_struct_info` from `TypeRegistry` and folded
//!   the two field-lookup helpers (`field_index`, `field_type`) onto
//!   `TypeLayouts` as inherent methods.
//! - **Wave 2** — added the `expo-typecheck` dep and relocated
//!   `mono_enum_variants`. The new methods (`register_enum_variants`,
//!   `enum_variants`, `contains_enum`) cover every former direct-field
//!   reader. Companion change: `Compiler::struct_field_lookup` and the two
//!   `concrete_field_*` helpers move into `crate::lower::fields` as the
//!   first true lowering functions hosted in `expo-ir`.
//! - **Wave 3** — pure rename in `expo-codegen`: `TypeRegistry` →
//!   `LLVMTypeCache` and `Compiler.types` → `Compiler.llvm_types`, making the
//!   surviving registry's LLVM-only role explicit at every call site.
//! - **Wave 4** — split `enum_variant_payloads` along the semantic /
//!   LLVM seam. Variant order (= tag value) moves entirely here as
//!   `variant_index`, with the non-generic enum side backfilled into
//!   `mono_enum_variants` so every enum has a single source of truth. The
//!   LLVM payload table on `LLVMTypeCache` is rekeyed by `expo_ir::VariantId`
//!   — an identity, not a position — so the two stores have no positional
//!   contract and `LLVMTypeCache` no longer reaches across to `TypeLayouts`.
//!   `VariantId` is a transitional `(String, String)` today; in the IR
//!   end-state (Phase 5+) it becomes an opaque `(EnumId, u8)` with no
//!   call-site changes.
//! - **Wave 5** — extract per-function semantic state out of
//!   `expo-codegen`'s `FnState` into [`crate::FnLowerState`]
//!   (`process_msg_type`, `return_type_hint`, `self_type_name`, `type_subst`,
//!   plus the TCO ambient flags `current_fn`/`tail_position` and their seven
//!   methods). `TailCallCtx` is dissolved entirely — its LLVM half
//!   (`loop_header`, `param_allocas`, `set_loop`, `restore_loop`) is inlined
//!   directly onto the trimmed `FnState`. Sister to `TypeLayouts`:
//!   `TypeLayouts` is the type-scoped semantic store, `FnLowerState` is the
//!   function-scoped semantic store.
//! - **Wave 6** — lift the nine LLVM-free semantic helpers off
//!   `Compiler` (`resolve_type_expr`, `monomorphize_type`,
//!   `resolve_name_current`, `find_type_current`, `id_for`,
//!   `type_name_from_expr`, `method_symbol_prefix`,
//!   `current_method_symbol_prefix`, `closure_info_at`) into
//!   [`crate::lower::types`], [`crate::lower::naming`], and
//!   [`crate::lower::closures`] as free functions taking a
//!   [`crate::lower::LowerCtx`] borrow bundle. `Compiler` is now LLVM-only:
//!   it holds state and emits IR but no longer owns semantic-decision
//!   methods. The bridge is `Compiler::lower_ctx()`, the gateway from the
//!   LLVM-bound driver to the LLVM-free lowering surface.
//! - **Wave 7 (current)** — Phase 4a sweep. ~28 pure-semantic
//!   `resolve_*`/`lower_*` helpers move out of `expo-codegen` into ten new
//!   [`crate::lower`] modules (`binary`, `constants`, `debug`, `enums`,
//!   `methods`, `patterns`, `processes`, `stmt`, `strings`, `structs`,
//!   plus additions to existing `closures`, `fields`, `mangling`).
//!   [`crate::lower::LowerCtx`] grows a `&TypeLayouts` field so layout-
//!   aware lowering can run as free functions; LLVM-bound state
//!   (variable type maps, function caches) is threaded in via small
//!   closures rather than coupling `expo-ir` to a backend. The
//!   `Resolved*`/`Format*` decision types those helpers return move
//!   alongside into [`crate::resolved`].
//! - **Wave 8+** — `variables`, `loop_exit_stack`, and `closure_counter`
//!   are still on `FnState` because they're either LLVM-bound
//!   (`PointerValue`/`BasicBlock`) or fused with LLVM emission state.
//!   The remaining `<'ctx>`-bound resolvers (`resolve_call`,
//!   `resolve_method_call`, `resolve_struct_name`, `resolve_static_call`,
//!   `resolve_spawn_info`, `resolve_field_ptr`, `resolve_union_member`,
//!   `resolve_payload_info`, `resolve_enumerable_info`, `resolve_closure`)
//!   need a decision/LLVM split before they can move; this is the
//!   Phase 4b backlog.

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
