//! Bundle of the inkwell handles every emission step needs (the
//! borrowed [`Context`], a fresh [`Module`], and a [`Builder`] tied
//! to the same `'ctx` lifetime), the per-emission counters and
//! per-function slot table, and a handle on the type-layout
//! registry [`crate::layout::TypeLayouts`].
//!
//! Deliberately a passive bundle — no business logic. Every
//! orchestration concern (program / script entry, function emission,
//! main-wrapper synthesis, instruction-level emission) lives in its
//! own module and takes `&EmitContext` as a parameter, so this struct
//! never grows into a god object. Type-layout machinery (struct +
//! enum registries, host `TargetData`) lives in [`crate::layout`]
//! and is accessed through the [`Self::layouts`] field; emission
//! call sites that need it go through `ctx.layouts.<method>(…)`
//! so the layered design stays visible at every reference.
//!
//! Named [`EmitContext`] rather than `LlvmCtx` because the role is
//! "context threaded through every emit operation," and to avoid
//! visual collision with [`inkwell::context::Context`] (which we
//! borrow inside).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use expo_alpha_ir::{IRLocalId, IRSymbol};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::BasicType;
use inkwell::values::BasicValueEnum;
use inkwell::values::PointerValue;

use crate::constant_pool::ConstantPoolSnapshot;
use crate::layout::TypeLayouts;

/// Fields are `pub(crate)` so sibling emission modules can borrow
/// them directly; outside the crate the bundle is opaque.
pub(crate) struct EmitContext<'ctx> {
    pub(crate) builder: Builder<'ctx>,
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    /// Type-layout registry: struct + enum LLVM type handles plus
    /// the host [`inkwell::targets::TargetData`] used by the enum
    /// layout computation. See [`crate::layout`].
    pub(crate) layouts: TypeLayouts<'ctx>,
    /// Counter for `alpha_str.<n>` global names. `Cell<u32>` because
    /// emission walks `&EmitContext` immutably; mirrors v1's
    /// `string_const.<n>` pattern in `expo-codegen`.
    string_counter: Cell<u32>,
    /// Per-function local-variable slot map: `IRLocalId ->
    /// PointerValue` (the LLVM `alloca` materializing the slot).
    /// Populated as `LocalDecl` instructions emit; consumed by
    /// `LocalRead` / `LocalWrite` to recover the stack pointer for
    /// `load` / `store`. Reset between functions through
    /// [`Self::reset_locals`] — slot identity is per-function.
    local_slots: RefCell<HashMap<IRLocalId, PointerValue<'ctx>>>,
    /// Merged `IRPackage::constants` from the input program / script.
    /// Set by [`Self::attach_constant_pool`] before any instruction
    /// emission; [`IRInstruction::LoadConst`] requires it.
    pub(crate) constant_pool: RefCell<Option<Arc<ConstantPoolSnapshot>>>,
    /// One LLVM SSA value per pooled constant [`IRSymbol`] — first
    /// `LoadConst` materializes (enum / struct aggregate or string
    /// global); later references reuse the cached handle.
    pub(crate) load_const_cache: RefCell<BTreeMap<IRSymbol, BasicValueEnum<'ctx>>>,
}

impl<'ctx> EmitContext<'ctx> {
    /// Build a fresh emit context against `context`, with an LLVM
    /// module named `module_name`. Convention is to pass the app
    /// name (matching the `__expo_app_name` global the runtime
    /// printer reads) so the generated IR's `; ModuleID = …` line
    /// identifies the binary that produced it.
    pub(crate) fn new(context: &'ctx Context, module_name: &str) -> Self {
        let layouts = TypeLayouts::new();
        let module = context.create_module(module_name);
        layouts.pin_module_data_layout(&module);
        Self {
            builder: context.create_builder(),
            context,
            module,
            layouts,
            string_counter: Cell::new(0),
            local_slots: RefCell::new(HashMap::new()),
            constant_pool: RefCell::new(None),
            load_const_cache: RefCell::new(BTreeMap::new()),
        }
    }

    /// Wire the flattened constant pool built from input packages.
    /// Must run before emitting any IR that can contain [`LoadConst`].
    pub(crate) fn attach_constant_pool(&self, pool: Arc<ConstantPoolSnapshot>) {
        *self.constant_pool.borrow_mut() = Some(pool);
    }

    pub(crate) fn next_string_symbol(&self) -> String {
        let n = self.string_counter.get();
        self.string_counter.set(n + 1);
        format!("alpha_str.{n}")
    }

    /// Register an `alloca` for `local`. Panics on duplicate keys —
    /// the IR seal pass guarantees one `LocalDecl` per `IRLocalId`
    /// per function, so a collision indicates an upstream bug.
    pub(crate) fn register_local_slot(&self, local: IRLocalId, ptr: PointerValue<'ctx>) {
        let mut slots = self.local_slots.borrow_mut();
        if slots.insert(local, ptr).is_some() {
            panic!(
                "alpha LLVM emit: local slot `{local}` registered twice — \
                 IR seal invariant violation",
            );
        }
    }

    /// Resolve `local` to its registered `alloca`. Misses panic — the
    /// IR seal guarantees every `LocalRead` / `LocalWrite` is preceded
    /// by a matching `LocalDecl` in the same function.
    pub(crate) fn local_slot(&self, local: IRLocalId) -> PointerValue<'ctx> {
        *self.local_slots.borrow().get(&local).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: local slot `{local}` not registered — \
                 IR seal / lower invariant violation",
            )
        })
    }

    /// Drop every registered slot. Called between function emissions
    /// so the per-function slot table doesn't bleed across `IRSymbol`
    /// boundaries.
    pub(crate) fn reset_locals(&self) {
        self.local_slots.borrow_mut().clear();
    }

    /// Build an alloca at the head of the current function's entry
    /// block, regardless of where the builder is currently
    /// positioned. Mirrors v1 codegen's `Compiler::build_entry_alloca`:
    /// pulling the alloca to the entry block keeps a per-iteration
    /// alloca inside a TCO loop from leaking stack across iterations.
    /// Restores the builder's position before returning.
    pub(crate) fn build_entry_alloca<T: BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> PointerValue<'ctx> {
        let saved = self
            .builder
            .get_insert_block()
            .expect("EmitContext::build_entry_alloca called with no insertion block");
        let function = saved.get_parent().expect(
            "EmitContext::build_entry_alloca called from a basic block with no parent function",
        );
        let entry = function
            .get_first_basic_block()
            .expect("EmitContext::build_entry_alloca expects the function to have an entry block");
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let alloca = self
            .builder
            .build_alloca(ty, name)
            .expect("inkwell rejected build_alloca in entry block");
        self.builder.position_at_end(saved);
        alloca
    }
}
