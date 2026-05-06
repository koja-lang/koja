//! Bundle of the three inkwell handles every emission step needs:
//! the borrowed [`Context`], a fresh [`Module`], and a [`Builder`]
//! tied to the same `'ctx` lifetime.
//!
//! Deliberately a passive bundle — no business logic. Every
//! orchestration concern (program / script entry, function emission,
//! main-wrapper synthesis, instruction-level emission) lives in its
//! own module and takes `&EmitCtx` as a parameter, so this struct
//! never grows into a god object.
//!
//! Named [`EmitCtx`] rather than `LlvmCtx` because the role is
//! "context threaded through every emit operation," and to avoid
//! visual collision with [`inkwell::context::Context`] (which we
//! borrow inside).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use expo_alpha_ir::{IRLocalId, IRSymbol};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicType, StructType};
use inkwell::values::PointerValue;

/// Fields are `pub(crate)` so sibling emission modules can borrow
/// them directly; outside the crate the bundle is opaque.
pub(crate) struct EmitCtx<'ctx> {
    pub(crate) builder: Builder<'ctx>,
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    /// Counter for `alpha_str.<n>` global names. `Cell<u32>` because
    /// emission walks `&EmitCtx` immutably; mirrors v1's
    /// `string_const.<n>` pattern in `expo-codegen`.
    string_counter: Cell<u32>,
    /// `IRSymbol -> StructType` lookup populated by
    /// [`crate::emit::structs::emit_struct_decls`] before any
    /// function bodies are walked. A `RefCell` because emission
    /// otherwise threads through `&EmitCtx`; only the pre-emit phase
    /// borrows mutably and every later read is a shared borrow.
    struct_types: RefCell<BTreeMap<IRSymbol, StructType<'ctx>>>,
    /// Per-function local-variable slot map: `IRLocalId ->
    /// PointerValue` (the LLVM `alloca` materializing the slot).
    /// Populated as `LocalDecl` instructions emit; consumed by
    /// `LocalRead` / `LocalWrite` to recover the stack pointer for
    /// `load` / `store`. Reset between functions through
    /// [`Self::reset_locals`] — slot identity is per-function.
    local_slots: RefCell<HashMap<IRLocalId, PointerValue<'ctx>>>,
}

impl<'ctx> EmitCtx<'ctx> {
    pub(crate) fn new(context: &'ctx Context) -> Self {
        Self {
            builder: context.create_builder(),
            context,
            module: context.create_module("expo_alpha_module"),
            string_counter: Cell::new(0),
            struct_types: RefCell::new(BTreeMap::new()),
            local_slots: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn next_string_symbol(&self) -> String {
        let n = self.string_counter.get();
        self.string_counter.set(n + 1);
        format!("alpha_str.{n}")
    }

    /// Insert a `StructType` under `symbol`. Panics on a duplicate
    /// key — every package emits its struct decls exactly once and
    /// `merge` deduplicates across packages, so a collision here is
    /// a compiler bug.
    pub(crate) fn register_struct_type(&self, symbol: IRSymbol, ty: StructType<'ctx>) {
        let mut map = self.struct_types.borrow_mut();
        if map.insert(symbol.clone(), ty).is_some() {
            panic!(
                "alpha LLVM emit: struct type `{symbol}` registered twice — \
                 lower / merge invariant violation",
            );
        }
    }

    /// Lookup a registered `StructType` by its mangled symbol.
    /// Misses panic — every `IRType::Struct(_)` /
    /// `IRInstruction::StructInit` / `IRInstruction::FieldGet`
    /// reaches emission only after the pre-emit phase has registered
    /// every package's structs.
    pub(crate) fn struct_type(&self, mangled: &str) -> StructType<'ctx> {
        *self.struct_types.borrow().get(mangled).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: struct type `{mangled}` not registered — \
                     pre-emit ordering violation",
            )
        })
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
            .expect("EmitCtx::build_entry_alloca called with no insertion block");
        let function = saved.get_parent().expect(
            "EmitCtx::build_entry_alloca called from a basic block with no parent function",
        );
        let entry = function
            .get_first_basic_block()
            .expect("EmitCtx::build_entry_alloca expects the function to have an entry block");
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
