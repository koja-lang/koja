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
use inkwell::types::{BasicType, StructType};
use inkwell::values::BasicValueEnum;
use inkwell::values::FunctionValue;
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
    /// Counter for `alpha_<prefix>.<n>` global names — strings,
    /// binary, bits constants all share a single sequence so each
    /// emitted global symbol is unique. `Cell<u32>` because emission
    /// walks `&EmitContext` immutably; mirrors v1's
    /// `string_const.<n>` pattern in `expo-codegen`.
    payload_counter: Cell<u32>,
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
    /// `IRSymbol -> FunctionValue` index populated at function
    /// declare time. Decouples call-site resolution from the LLVM
    /// symbol name — `@extern "C"` declarations may declare under a
    /// `link_name` alias (`fn cosf` → `@cos`), so `module.get_function`
    /// keyed at the IR's mangled name would miss. Instruction
    /// emission goes through [`Self::declared_function`] /
    /// [`Self::register_declared_function`] instead.
    declared_functions: RefCell<BTreeMap<IRSymbol, FunctionValue<'ctx>>>,
    /// Per-function closure-emit frame, set while a
    /// `FunctionKind::Closure` body is being defined and cleared
    /// when it returns. `LoadCapture` reads `env_ptr` + `env_struct`
    /// to GEP its slot; non-closure bodies see `None`.
    closure_frame: RefCell<Option<ClosureFrame<'ctx>>>,
}

/// Borrowed env handle used by the closure-body emit path.
/// `env_ptr` is the body's first LLVM parameter (the env pointer
/// the caller's `MakeClosure` malloc'd); `env_struct` is the LLVM
/// type assembled from the body's `FunctionKind::Closure::env_layout`
/// so [`crate::emit::instruction`] can GEP into the right field.
#[derive(Clone, Copy)]
pub(crate) struct ClosureFrame<'ctx> {
    pub(crate) env_ptr: PointerValue<'ctx>,
    pub(crate) env_struct: StructType<'ctx>,
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
            payload_counter: Cell::new(0),
            local_slots: RefCell::new(HashMap::new()),
            constant_pool: RefCell::new(None),
            load_const_cache: RefCell::new(BTreeMap::new()),
            declared_functions: RefCell::new(BTreeMap::new()),
            closure_frame: RefCell::new(None),
        }
    }

    /// Set the active [`ClosureFrame`] for the body currently being
    /// defined. Pairs with [`Self::clear_closure_frame`]; calling
    /// twice without a clear in between panics so the per-function
    /// scope stays explicit.
    pub(crate) fn set_closure_frame(&self, frame: ClosureFrame<'ctx>) {
        let mut slot = self.closure_frame.borrow_mut();
        if slot.is_some() {
            panic!(
                "alpha LLVM emit: nested closure frame set without clearing the previous one — \
                 caller must clear before re-entering",
            );
        }
        *slot = Some(frame);
    }

    pub(crate) fn clear_closure_frame(&self) {
        *self.closure_frame.borrow_mut() = None;
    }

    /// Active closure frame for the body being emitted, or `None` in
    /// non-closure bodies. `LoadCapture` panics on `None` since the
    /// IR seal pass forbids it outside `FunctionKind::Closure`.
    pub(crate) fn closure_frame(&self) -> Option<ClosureFrame<'ctx>> {
        *self.closure_frame.borrow()
    }

    /// Insert a freshly-declared function into the
    /// `IRSymbol -> FunctionValue` index. Idempotent on a per-symbol
    /// basis; the second call for the same `symbol` overwrites with
    /// the (presumed-equal) handle, mirroring the inkwell module's
    /// own dedup behavior for symbols already present in the LLVM
    /// module.
    pub(crate) fn register_declared_function(&self, symbol: IRSymbol, value: FunctionValue<'ctx>) {
        self.declared_functions.borrow_mut().insert(symbol, value);
    }

    /// Resolve `symbol` to its registered LLVM function. `None` when
    /// no declare step has run for this symbol yet — call sites
    /// surface that as a codegen error since the declare phase is
    /// supposed to run before any body emission.
    pub(crate) fn declared_function(&self, symbol: &IRSymbol) -> Option<FunctionValue<'ctx>> {
        self.declared_functions.borrow().get(symbol).copied()
    }

    /// Wire the flattened constant pool built from input packages.
    /// Must run before emitting any IR that can contain [`LoadConst`].
    pub(crate) fn attach_constant_pool(&self, pool: Arc<ConstantPoolSnapshot>) {
        *self.constant_pool.borrow_mut() = Some(pool);
    }

    /// Mint a fresh module-unique symbol name for a heap-payload
    /// global. Callers pass `"str"` for strings, `"bin"` for binary,
    /// `"bits"` for bits — the prefix is purely cosmetic (helps
    /// reading raw LLVM IR) but the counter is shared so two
    /// different prefixes can't collide.
    pub(crate) fn next_payload_symbol(&self, prefix: &str) -> String {
        let n = self.payload_counter.get();
        self.payload_counter.set(n + 1);
        format!("alpha_{prefix}.{n}")
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

    /// Resolve the opaque outer `StructType` for an enum by its
    /// mangled name. Outer types are minted (and so registered in the
    /// LLVM context's name table) by [`crate::layout::enums::declare_enum_type`];
    /// this accessor is a thin alias over [`Context::get_struct_type`]
    /// so emission sites read with intent — "the enum outer for
    /// `<symbol>`" rather than "named LLVM struct by string." Bodies
    /// only land later in [`crate::layout::enums::define_enum_bodies`],
    /// but the opaque handle is stable across both phases, which is
    /// what struct field / enum payload positions need before the
    /// body-define pass runs.
    pub(crate) fn enum_outer_type(&self, mangled: &str) -> StructType<'ctx> {
        self.context.get_struct_type(mangled).unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: enum outer `{mangled}` not declared — \
                 declare_enum_type ordering violation (must run before \
                 any struct/enum body references this symbol)",
            )
        })
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
