//! LLVM type layouts and the type-creation pre-emit phase.
//!
//! Owns the host [`TargetData`] plus `IRSymbol -> StructType` /
//! `IRSymbol -> EnumLayout` registries, and the per-shape pre-emit
//! submodules ([`structs`], [`enums`]) that mint LLVM types from
//! sealed IR decls. `crate::emit` is reserved for IR-instruction
//! lowering; type creation lives here.
//!
//! `TypeLayouts::new` runs once from [`crate::ctx::EmitContext::new`];
//! `pin_module_data_layout` then aligns the module's data layout
//! with the host triple so `get_abi_size` / `get_abi_alignment`
//! match what the object emitter eventually pins. Every panic in
//! this file marks an invariant violation upstream (lower / merge
//! produced a duplicate symbol, or pre-emit ordering missed a
//! decl); none are recoverable.

use std::cell::RefCell;
use std::collections::BTreeMap;

use expo_alpha_ir::{IRSymbol, IRType, IRVariantPayload, IRVariantTag};
use inkwell::OptimizationLevel;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetData, TargetMachine,
};
use inkwell::types::StructType;

pub(crate) mod enum_order;
pub(crate) mod enums;
pub(crate) mod structs;
pub(crate) mod unions;

/// LLVM layout of a single enum decl. `variants` is indexed by
/// [`IRVariantTag`].0 so construction can recover the per-variant
/// types in O(1). The outer `StructType` lives on the LLVM
/// context's named-type table (minted in [`enums::declare_enum_type`])
/// and is fetched by name via [`crate::ctx::EmitContext::enum_outer_type`];
/// holding it here too would just duplicate that registry.
pub(crate) struct EnumLayout<'ctx> {
    pub(crate) variants: Vec<VariantLayout<'ctx>>,
}

/// `complete` is `{ i8 tag, [pad x i8], payload }` (or `{ i8 }`
/// for Unit); `payload` is the inner field struct, `None` for
/// Unit. See [`enums`] for layout details.
pub(crate) struct VariantLayout<'ctx> {
    pub(crate) complete: StructType<'ctx>,
    pub(crate) payload: Option<StructType<'ctx>>,
}

/// LLVM layout of a single union decl. `outer` is the
/// `{ i8 tag, [N x i8] payload }` named struct; `payload_size`
/// is `N` (the byte width of the largest member, cached on
/// [`expo_alpha_ir::IRUnionDecl::max_payload_size`]). See
/// [`unions`] for layout details.
pub(crate) struct UnionLayout<'ctx> {
    pub(crate) outer: StructType<'ctx>,
    pub(crate) payload_size: u32,
}

/// Type-layout registry held as [`crate::ctx::EmitContext::layouts`].
/// `target_data` is `pub(crate)` because [`enums::define_enum_bodies`]
/// and [`crate::types`] consult it directly; the registries stay
/// private behind accessors so `RefCell` borrows can't leak.
pub(crate) struct TypeLayouts<'ctx> {
    pub(crate) target_data: TargetData,
    struct_types: RefCell<BTreeMap<IRSymbol, StructType<'ctx>>>,
    /// IR-level field types for every declared struct, indexed by
    /// the same symbol as `struct_types`. Retained post-layout so
    /// intrinsic emitters can resolve "field `i` of `Pair_$T,U$` is
    /// `Option<T>`" without re-deriving from mangled names. Mirrors
    /// the role `enum_layouts` plays for enum-shaped data.
    struct_fields: RefCell<BTreeMap<IRSymbol, Vec<IRType>>>,
    enum_layouts: RefCell<BTreeMap<IRSymbol, EnumLayout<'ctx>>>,
    /// IR-level per-variant payload shapes for every declared enum,
    /// indexed by the same symbol as `enum_layouts`. Retained
    /// post-layout so intrinsic emitters can resolve "the `Ok`
    /// variant's first field of `Result_$R.E$` is `R`" without
    /// reaching back into the program-level [`expo_alpha_ir::IREnumDecl`]
    /// registry. Mirrors `struct_fields` for struct-shaped data.
    enum_variant_payloads: RefCell<BTreeMap<IRSymbol, Vec<IRVariantPayload>>>,
    union_layouts: RefCell<BTreeMap<IRSymbol, UnionLayout<'ctx>>>,
}

impl<'ctx> TypeLayouts<'ctx> {
    pub(crate) fn new() -> Self {
        Self {
            target_data: host_target_data(),
            struct_types: RefCell::new(BTreeMap::new()),
            struct_fields: RefCell::new(BTreeMap::new()),
            enum_layouts: RefCell::new(BTreeMap::new()),
            enum_variant_payloads: RefCell::new(BTreeMap::new()),
            union_layouts: RefCell::new(BTreeMap::new()),
        }
    }

    /// Pin `module`'s data layout to the host target's so subsequent
    /// `get_abi_size` / `get_abi_alignment` queries match what the
    /// object emitter will eventually pin.
    pub(crate) fn pin_module_data_layout(&self, module: &Module<'ctx>) {
        module.set_data_layout(&self.target_data.get_data_layout());
    }

    pub(crate) fn register_struct_type(&self, symbol: IRSymbol, ty: StructType<'ctx>) {
        let mut map = self.struct_types.borrow_mut();
        if map.insert(symbol.clone(), ty).is_some() {
            panic!(
                "alpha LLVM emit: struct type `{symbol}` registered twice — \
                 lower / merge invariant violation",
            );
        }
    }

    pub(crate) fn struct_type(&self, mangled: &str) -> StructType<'ctx> {
        *self.struct_types.borrow().get(mangled).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: struct type `{mangled}` not registered — \
                     pre-emit ordering violation",
            )
        })
    }

    pub(crate) fn register_struct_fields(&self, symbol: IRSymbol, fields: Vec<IRType>) {
        let mut map = self.struct_fields.borrow_mut();
        if map.insert(symbol.clone(), fields).is_some() {
            panic!(
                "alpha LLVM emit: struct fields for `{symbol}` registered twice — \
                 lower / merge invariant violation",
            );
        }
    }

    /// IR type of `struct_symbol`'s field at `index`. Panics on
    /// unregistered symbol / out-of-range index — both indicate a
    /// pre-emit ordering or IR-seal violation upstream.
    pub(crate) fn struct_field_ir_type(&self, struct_symbol: &IRSymbol, index: usize) -> IRType {
        let map = self.struct_fields.borrow();
        let fields = map.get(struct_symbol).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: struct fields for `{struct_symbol}` not registered — \
                 pre-emit ordering violation",
            )
        });
        fields.get(index).cloned().unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: struct `{struct_symbol}` has no field at index {index} — \
                 IR seal invariant violation",
            )
        })
    }

    pub(crate) fn register_enum_layout(&self, symbol: IRSymbol, layout: EnumLayout<'ctx>) {
        let mut map = self.enum_layouts.borrow_mut();
        if map.insert(symbol.clone(), layout).is_some() {
            panic!(
                "alpha LLVM emit: enum layout `{symbol}` registered twice — \
                 lower / merge invariant violation",
            );
        }
    }

    pub(crate) fn register_enum_variant_payloads(
        &self,
        symbol: IRSymbol,
        payloads: Vec<IRVariantPayload>,
    ) {
        let mut map = self.enum_variant_payloads.borrow_mut();
        if map.insert(symbol.clone(), payloads).is_some() {
            panic!(
                "alpha LLVM emit: enum variant payloads for `{symbol}` registered twice — \
                 lower / merge invariant violation",
            );
        }
    }

    /// IR-level payload of `enum_symbol`'s variant at `tag`. Panics
    /// on unregistered symbol / out-of-range tag — both indicate a
    /// pre-emit ordering or IR-seal violation upstream.
    pub(crate) fn enum_variant_payload(
        &self,
        enum_symbol: &IRSymbol,
        tag: IRVariantTag,
    ) -> IRVariantPayload {
        let map = self.enum_variant_payloads.borrow();
        let payloads = map.get(enum_symbol).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: enum variant payloads for `{enum_symbol}` not registered — \
                 pre-emit ordering violation",
            )
        });
        payloads
            .get(usize::from(tag.0))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "alpha LLVM emit: enum `{enum_symbol}` has no variant at tag {tag} — \
                     IR seal invariant violation",
                )
            })
    }

    /// Closure-borrow over the `RefCell` so callers can't hold a
    /// long-lived `Ref` across other emit operations.
    pub(crate) fn with_enum_layout<R>(
        &self,
        mangled: &str,
        f: impl FnOnce(&EnumLayout<'ctx>) -> R,
    ) -> R {
        let map = self.enum_layouts.borrow();
        let layout = map.get(mangled).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: enum layout `{mangled}` not registered — \
                 pre-emit ordering violation",
            )
        });
        f(layout)
    }

    /// `(complete, payload)` for the variant at `tag`. Copies the
    /// inkwell handles out of the borrow so the caller can build
    /// allocas / GEPs without holding the `RefCell` open.
    pub(crate) fn enum_variant_types(
        &self,
        mangled: &str,
        tag: IRVariantTag,
    ) -> (StructType<'ctx>, Option<StructType<'ctx>>) {
        self.with_enum_layout(mangled, |layout| {
            let variant = layout.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
                panic!(
                    "alpha LLVM emit: enum `{mangled}` has no variant at tag {tag} — \
                     IR seal invariant violation",
                )
            });
            (variant.complete, variant.payload)
        })
    }

    pub(crate) fn register_union_layout(&self, symbol: IRSymbol, layout: UnionLayout<'ctx>) {
        let mut map = self.union_layouts.borrow_mut();
        if map.insert(symbol.clone(), layout).is_some() {
            panic!(
                "alpha LLVM emit: union layout `{symbol}` registered twice — \
                 lower / merge invariant violation",
            );
        }
    }

    /// `(outer, payload_size)` for the union at `mangled`. Copies
    /// out so the caller can build allocas / GEPs without holding
    /// the `RefCell` open.
    pub(crate) fn union_outer(&self, mangled: &str) -> (StructType<'ctx>, u32) {
        let map = self.union_layouts.borrow();
        let layout = map.get(mangled).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: union layout `{mangled}` not registered — \
                 pre-emit ordering violation",
            )
        });
        (layout.outer, layout.payload_size)
    }
}

/// Host-triple [`TargetData`] matching the CPU / features /
/// reloc-mode used by [`crate::object::emit_object_file`] so layout
/// numbers fed into enum sizing match the eventual object output.
fn host_target_data() -> TargetData {
    Target::initialize_native(&InitializationConfig::default())
        .expect("alpha LLVM emit: failed to initialize native target");
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple)
        .expect("alpha LLVM emit: failed to resolve native target from triple");
    let cpu = TargetMachine::get_host_cpu_name().to_string();
    let features = TargetMachine::get_host_cpu_features().to_string();
    let machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .expect("alpha LLVM emit: failed to create native target machine");
    machine.get_target_data()
}
