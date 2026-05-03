//! Compilation driver: holds all LLVM state, registers types, declares and
//! defines functions, and orchestrates emission of native object files.

use std::collections::{BTreeMap, HashMap};
use std::mem;
use std::path::{Path, PathBuf};

use expo_ir::Lowerer;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier, VariantIdentifier};
use expo_ir::lower::LowerCtx;
use expo_ir::lower::constants::{ConstantTables, populate_constants};
use expo_ir::lower::naming::{current_method_symbol_prefix, method_symbol_prefix};
use expo_ir::lower::types::{resolve_name_current, resolve_type_expr, type_name_from_expr};
use expo_ir::{
    ExternAbi, ExternAttrs, FnLowerState, IRBlockId, IRConstantValue, IRFunction, IRFunctionKind,
    IRFunctionMeta, IROperand, IRProgram, TypeLayouts,
};

use crate::drop::Ownership;
use crate::generics::{compile_function_body, compile_method_body, ensure_types_exist};
use crate::registration::{finalize_pending_unions, register_types};
use crate::spawn::{self, ExitCodeCtx};

/// Result of attempting to emit an intrinsic method for a built-in type.
/// `NotIntrinsic` signals the caller to fall through to body compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitResult {
    Emitted,
    NotIntrinsic,
}

use expo_ast::ast::{
    AnnotationValue, Diagnostic, File, Function, ImplMember, Item, Param, Severity,
};
use expo_ast::identifier::TypeIdentifier;
use expo_ast::span::Span;
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Package, Type, named_generic, package_from_str};
use inkwell::OptimizationLevel;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module as LlvmModule;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use crate::debug_info::DebugContext;
use crate::types::{to_llvm_metadata_type, to_llvm_type};

/// An LLVM value paired with its Expo source-level type. Threaded through
/// `compile_expr` so downstream code never needs to reverse-engineer the
/// type from LLVM bit widths or struct names.
#[derive(Debug, Clone)]
pub struct TypedValue<'ctx> {
    pub value: BasicValueEnum<'ctx>,
    pub expo_type: Type,
}

impl<'ctx> TypedValue<'ctx> {
    pub fn new(value: BasicValueEnum<'ctx>, expo_type: Type) -> Self {
        Self { value, expo_type }
    }
}

/// Shorthand for the return type of `compile_expr` and related functions.
pub type ExprResult<'ctx> = Result<Option<TypedValue<'ctx>>, String>;

/// LLVM-only struct type cache: handles for non-generic types, monomorphized
/// generics/unions, and enum variant payloads. Populated during type
/// registration / monomorphisation and read during body compilation.
///
/// Semantic layout data (field order, variant lists) lives in
/// [`expo_ir::TypeLayouts`] on `Compiler.layouts`; this struct holds only
/// `inkwell::StructType<'ctx>` handles. The variant payload table is keyed
/// by an identity ([`VariantIdentifier`]) rather than positionally, so there
/// is no positional contract between this cache and `TypeLayouts` — variant
/// order (= tag value) is owned solely by `TypeLayouts::variant_index`.
pub struct LLVMTypeCache<'ctx> {
    /// Collision-safe map for non-generic types, keyed by package-qualified
    /// `TypeIdentifier`. Used for concrete structs and enums.
    pub concrete: HashMap<TypeIdentifier, StructType<'ctx>>,

    /// Identity-keyed cache of LLVM payload struct handles for each enum
    /// variant. `None` records that a variant has been registered but has
    /// no payload (a nullary variant). Lookups go through
    /// [`Self::variant_payload`].
    pub enum_variant_payloads: HashMap<VariantIdentifier, Option<StructType<'ctx>>>,

    /// Map for monomorphized generic types and unions, keyed by mangled name
    /// (e.g. `"List_$Int32$"`, `"Union_$Int.String$"`).
    pub monomorphized: HashMap<MonomorphizedTypeIdentifier, StructType<'ctx>>,

    /// Union opaque structs registered during type collection but whose
    /// payload layout has been deferred until member bodies are set. Drained
    /// by [`crate::registration::finalize_pending_unions`] after struct/enum
    /// bodies are defined. Holding `(opaque, members)` in insertion order so
    /// dependency order is preserved when finalize runs.
    pub pending_union_layouts: Vec<(StructType<'ctx>, Vec<Type>)>,
}

impl<'ctx> LLVMTypeCache<'ctx> {
    pub fn new() -> Self {
        Self {
            concrete: HashMap::new(),
            enum_variant_payloads: HashMap::new(),
            monomorphized: HashMap::new(),
            pending_union_layouts: Vec::new(),
        }
    }

    /// Check whether a monomorphized type is registered.
    pub fn contains_monomorphized(&self, id: &MonomorphizedTypeIdentifier) -> bool {
        self.monomorphized.contains_key(id)
    }

    /// Look up a non-generic type by its package-qualified identifier.
    /// `Package::Unresolved` identifiers return `None`; callers must supply
    /// a fully-qualified [`TypeIdentifier`].
    pub fn get_concrete(&self, id: &TypeIdentifier) -> Option<StructType<'ctx>> {
        self.concrete.get(id).copied()
    }

    /// Look up a monomorphized generic or union type by its mangled name.
    pub fn get_monomorphized(&self, id: &MonomorphizedTypeIdentifier) -> Option<StructType<'ctx>> {
        self.monomorphized.get(id).copied()
    }

    /// Register a non-generic struct or enum type by its package-qualified
    /// identifier.
    pub fn register_concrete(&mut self, id: &TypeIdentifier, ty: StructType<'ctx>) {
        self.concrete.insert(id.clone(), ty);
    }

    /// Register a monomorphized generic or union type by its mangled name.
    pub fn register_monomorphized(
        &mut self,
        id: MonomorphizedTypeIdentifier,
        ty: StructType<'ctx>,
    ) {
        self.monomorphized.insert(id, ty);
    }

    /// Returns the LLVM payload struct for an enum variant, if it has one.
    /// Returns `None` both for unknown variants and for nullary variants;
    /// callers that need to distinguish these cases can ask `TypeLayouts`.
    pub fn variant_payload(&self, id: &VariantIdentifier) -> Option<StructType<'ctx>> {
        self.enum_variant_payloads.get(id).copied().flatten()
    }
}

/// Per-function LLVM-bound state that is set/reset at each `define_function`
/// call. Holds variable allocas, the fn-wide block table, and tail-call
/// rewrite scaffolding (`loop_header` + `param_allocas`). Pure-semantic
/// per-function state lives in [`expo_ir::FnLowerState`] on
/// `Compiler.fn_lower`.
pub struct FnState<'ctx> {
    /// Function-wide map of [`expo_ir::IRBlockId`] -> LLVM
    /// [`BasicBlock`]. Every per-construct emit walker registers the
    /// blocks it allocates here immediately after `append_basic_block`.
    /// Used as a fallback in [`crate::control::terminator::emit_terminator`]
    /// when an [`expo_ir::IRTerminator::Branch`] / `CondBranch` /
    /// `Return` references an `IRBlockId` minted by an enclosing
    /// construct (e.g. a `break` inside a nested `if` references the
    /// enclosing loop's `exit_block`). Replaced the old
    /// `loop_exit_blocks` stack in Phase 4g Slice 2.
    pub block_table: HashMap<IRBlockId, BasicBlock<'ctx>>,
    /// Loop header block for the current function. When a self-recursive
    /// tail call is detected, codegen stores new arguments into the
    /// parameter allocas and branches here instead of emitting a call.
    pub loop_header: Option<BasicBlock<'ctx>>,
    /// Parameter allocas in call order (self first, then regular params).
    pub param_allocas: Vec<PointerValue<'ctx>>,
    /// Snapshot stack for [`expo_ir::values::IRInstruction::PushTypeSubst`]
    /// / [`expo_ir::values::IRInstruction::PopTypeSubst`]. Each entry
    /// captures the prior `fn_lower.type_subst` value (`Some(prior)`)
    /// or absence (`None`) for every key the matching push shadowed,
    /// so the pop can restore precisely the pre-push state.
    pub type_subst_stack: Vec<HashMap<String, Option<Type>>>,
    pub variables: BTreeMap<String, (PointerValue<'ctx>, Type, Ownership)>,
}

impl<'ctx> FnState<'ctx> {
    pub fn new() -> Self {
        Self {
            block_table: HashMap::new(),
            loop_header: None,
            param_allocas: Vec::new(),
            type_subst_stack: Vec::new(),
            variables: BTreeMap::new(),
        }
    }

    /// Restore the previous loop header and parameter allocas.
    pub fn restore_loop(&mut self, saved: (Option<BasicBlock<'ctx>>, Vec<PointerValue<'ctx>>)) {
        self.loop_header = saved.0;
        self.param_allocas = saved.1;
    }

    /// Set the loop header and parameter allocas for the current function.
    /// Returns the previous values for restoration on exit.
    pub fn set_loop(
        &mut self,
        header: BasicBlock<'ctx>,
        allocas: Vec<PointerValue<'ctx>>,
    ) -> (Option<BasicBlock<'ctx>>, Vec<PointerValue<'ctx>>) {
        let saved_header = self.loop_header.replace(header);
        let saved_allocas = mem::replace(&mut self.param_allocas, allocas);
        (saved_header, saved_allocas)
    }
}

/// Holds all LLVM state needed to compile an Expo file: the LLVM context,
/// module, builder, declared functions, variable bindings, and type mappings.
pub struct Compiler<'ctx> {
    pub context: &'ctx Context,
    pub module: LlvmModule<'ctx>,
    pub builder: Builder<'ctx>,
    /// LLVM-materialized compound constants, indexed by
    /// [`expo_ir::IRConstId`]. Populated by [`Self::declare_constants`]
    /// in IR-pool order. Primitives never appear here -- they inline at
    /// IR-lower time.
    pub constants: Vec<BasicValueEnum<'ctx>>,
    /// Name -> [`expo_ir::IRConstId`] / inline-primitive lookup tables
    /// produced by [`populate_constants`] and consulted by every
    /// [`Lowerer`] this compiler hands out.
    pub const_tables: ConstantTables,
    pub functions: HashMap<FunctionIdentifier, FunctionValue<'ctx>>,
    pub type_ctx: &'ctx TypeContext,
    pub generic_fn_asts: HashMap<String, Function>,
    /// Cache of generated thunk wrappers for bare function references.
    /// Maps original function name to the thunk `FunctionValue`.
    pub fn_ref_thunks: HashMap<FunctionIdentifier, FunctionValue<'ctx>>,
    /// LLVM-free per-function semantic state (lives in `expo-ir`). Hosts
    /// `return_type_hint`, `process_msg_type`, `type_subst`, `self_type_name`,
    /// and the TCO bookkeeping (`current_fn`). Companion to [`Self::layouts`]:
    /// layouts is type-scoped, fn_lower is function-scoped.
    pub fn_lower: FnLowerState,
    /// LLVM type cache: handles for non-generic and monomorphized struct
    /// types, plus identity-keyed enum payload structs. Populated during
    /// type registration; read during body compilation.
    pub llvm_types: LLVMTypeCache<'ctx>,
    /// LLVM-free semantic layout tables (lives in `expo-ir`). Hosts
    /// monomorphized struct field layouts and the canonical enum variant
    /// lists; tag values come from [`TypeLayouts::variant_index`].
    pub layouts: TypeLayouts,
    /// LLVM-free IR-level program: the source of truth for monomorphized
    /// struct, enum, and function declarations awaiting backend emission.
    /// Populated by `expo_ir::lower::monomorphize::*` planners (called
    /// through the `monomorphize_*` shims in `crate::generics`) and
    /// consumed by the `emit_ir_*` family.
    pub ir: IRProgram,
    /// Per-function LLVM-bound state: variable allocas, loop-exit stack,
    /// and tail-call loop scaffolding. Semantic per-function state lives
    /// in [`Self::fn_lower`].
    pub fn_state: FnState<'ctx>,
    /// Source path of the Expo file currently being defined; matches
    /// [`TypeContext::closure_info`] keys during lookup.
    pub closure_site_path: Option<PathBuf>,
    /// DWARF debug info state (always present; emitted in all builds).
    pub debug: DebugContext<'ctx>,
    /// Package of the file whose items are currently being declared/defined.
    /// Set by [`run_codegen`] around each file's declare and define passes so
    /// method symbols can be qualified per package (e.g. `alpha.Config_new`)
    /// and disambiguated across user packages that share a type name.
    pub current_package: Option<Package>,
    /// Counter incremented by the `monomorphize_*` shims in
    /// [`crate::generics`] every time their underlying planner adds a
    /// new IR decl post-closure-pass. Reaches a non-zero value only
    /// when [`expo_ir::closure_program`] missed a generic instantiation
    /// the lazy codegen path is now backfilling. Logged at the end of
    /// [`run_codegen`] for visibility; remains advisory until the
    /// closure pass is proven complete enough to make it a hard
    /// assertion.
    pub lazy_mono_count: usize,
}

impl<'ctx> Compiler<'ctx> {
    /// Creates a new compiler instance with an empty LLVM module.
    pub fn new(
        context: &'ctx Context,
        type_ctx: &'ctx TypeContext,
        filename: &str,
        directory: &str,
        release: bool,
    ) -> Self {
        let module = context.create_module("expo_module");
        let builder = context.create_builder();
        let debug = DebugContext::new(&module, filename, directory, release);
        let llvm_types = LLVMTypeCache::new();
        Self {
            context,
            module,
            builder,
            constants: Vec::new(),
            const_tables: ConstantTables::default(),
            functions: HashMap::new(),
            type_ctx,
            generic_fn_asts: HashMap::new(),
            fn_ref_thunks: HashMap::new(),
            fn_lower: FnLowerState::new(),
            llvm_types,
            layouts: TypeLayouts::new(),
            ir: IRProgram::new(),
            fn_state: FnState::new(),
            closure_site_path: None,
            debug,
            current_package: None,
            lazy_mono_count: 0,
        }
    }

    /// Sets [`Self::current_package`] to `pkg` for the duration of `f`,
    /// restoring whatever scope was previously active. Used by `run_codegen`
    /// to thread per-file package context through declare/define passes.
    pub fn with_package<R>(&mut self, pkg: Package, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.current_package.take();
        self.current_package = Some(pkg);
        let r = f(self);
        self.current_package = prev;
        r
    }

    /// Constructs a read-only [`LowerCtx`] borrow bundle for the LLVM-free
    /// lowering helpers in [`expo_ir::lower`]. Call this at the start of any
    /// site that needs `resolve_type_expr`, `monomorphize_type`,
    /// `resolve_name_current`, etc., and pass `&ctx` (or batch one bundle
    /// across several lowering calls). This method is the only gateway from
    /// the LLVM-bound `Compiler` into the LLVM-free lowering surface --
    /// `Compiler` itself no longer exposes inherent semantic-decision
    /// methods.
    pub fn lower_ctx(&self) -> LowerCtx<'_> {
        LowerCtx {
            closure_site_path: self.closure_site_path.as_deref(),
            fn_lower: &self.fn_lower,
            layouts: &self.layouts,
            locals: &self.fn_lower,
            package: self.current_package.as_ref(),
            type_ctx: self.type_ctx,
        }
    }

    /// Split-borrow companion to [`Self::lower_ctx`] that hands out a
    /// `LowerCtx<'_>` alongside a `&mut IRProgram` from disjoint fields
    /// of `Self`. Use this from monomorphization shims that need to
    /// drive an `expo_ir::lower::monomorphize::*` planner: the planner
    /// reads from the bundle and appends decls to the program in a
    /// single borrow scope. Returning two borrows from one `&mut self`
    /// call is what lets Rust's borrow checker see the field disjointness;
    /// hand-rolling the pair at each call site triggers spurious
    /// conflict errors.
    pub fn lower_ctx_and_ir(&mut self) -> (LowerCtx<'_>, &mut IRProgram) {
        let lower_ctx = LowerCtx {
            closure_site_path: self.closure_site_path.as_deref(),
            fn_lower: &self.fn_lower,
            layouts: &self.layouts,
            locals: &self.fn_lower,
            package: self.current_package.as_ref(),
            type_ctx: self.type_ctx,
        };
        (lower_ctx, &mut self.ir)
    }

    /// Construct a [`Lowerer<'_>`] borrowing the same program-level
    /// references [`Self::lower_ctx`] hands out, plus a mutable
    /// borrow of [`Self::fn_lower`] for SSA / block id minting. Use
    /// from compile shims (e.g. `compile_unless`, `compile_if`) that
    /// drive the operand-lowering surface, which lives as inherent
    /// methods on [`Lowerer`].
    ///
    /// Splits borrows from `&mut self` across disjoint fields so the
    /// returned `Lowerer` carries `&mut self.fn_lower` alongside
    /// shared borrows of the surrounding context. Coexists with
    /// [`Self::lower_ctx`] -- helpers in [`expo_ir::lower`] that
    /// haven't migrated to `Lowerer` methods yet still take
    /// [`LowerCtx`].
    pub fn lowerer(&mut self) -> Lowerer<'_> {
        Lowerer {
            closure_site_path: self.closure_site_path.as_deref(),
            const_tables: &self.const_tables,
            fn_state: &mut self.fn_lower,
            layouts: &self.layouts,
            package: self.current_package.as_ref(),
            program: &self.ir,
            type_ctx: self.type_ctx,
        }
    }

    /// Registers a callable symbol in lockstep across [`Self::ir`]
    /// (the canonical semantic registry) and [`Self::functions`] (the
    /// LLVM-handle map). Every site that adds a symbol the language
    /// can resolve through `resolve_call` / `resolve_method_call` /
    /// `resolve_static_call` must route through this helper so the two
    /// stores cannot drift.
    ///
    /// Most callers should reach for one of the typed helpers
    /// ([`Self::register_extern`], [`Self::register_free`],
    /// [`Self::register_intrinsic`], [`Self::register_main_entry`],
    /// [`Self::register_method`], [`Self::register_thunk`]) which
    /// pre-populate the right [`IRFunctionKind`] for their site.
    pub fn register_function(
        &mut self,
        mangled: FunctionIdentifier,
        param_types: Vec<Type>,
        return_type: Type,
        kind: IRFunctionKind,
        value: FunctionValue<'ctx>,
    ) {
        self.ir.insert_function(IRFunction {
            mangled: mangled.clone(),
            param_types,
            return_type,
            kind,
        });
        self.functions.insert(mangled, value);
    }

    /// Convenience wrapper for foreign-linked symbols: C stdlib
    /// (`printf`, `malloc`, ...), Expo runtime FFI (`expo_rt_*`,
    /// `expo_string_*`, ...), and user-source `@extern "C"`
    /// declarations. The carried [`ExternAttrs`] is sufficient for
    /// any backend to declare and link the symbol without consulting
    /// the LLVM module. Signature is recorded as `Type::Unknown` for
    /// now -- the LLVM `FunctionType` is the source of truth and a
    /// future slice can promote the Expo-source types into IR.
    pub fn register_extern(
        &mut self,
        mangled: FunctionIdentifier,
        value: FunctionValue<'ctx>,
        attrs: ExternAttrs,
    ) {
        self.register_function(
            mangled,
            Vec::new(),
            Type::Unknown,
            IRFunctionKind::Extern(attrs),
            value,
        );
    }

    /// Convenience wrapper for non-generic top-level user functions.
    /// Mirrors the `Free` registration the monomorphize planner
    /// performs for generic instantiations, so the IR carries the
    /// same kind for both populations and a body-walking backend can
    /// treat them uniformly.
    pub fn register_free(
        &mut self,
        mangled: FunctionIdentifier,
        value: FunctionValue<'ctx>,
        func_ast: Function,
    ) {
        let meta = IRFunctionMeta::from_ast(&func_ast);
        self.register_function(
            mangled,
            Vec::new(),
            Type::Unknown,
            IRFunctionKind::Free {
                func_ast,
                meta,
                subst: HashMap::new(),
                blocks: Vec::new(),
            },
            value,
        );
    }

    /// Convenience wrapper for compiler-defined methods whose body is
    /// hand-emitted by the backend: stdlib intrinsics
    /// (`List.append`, `Map.get`, `CPtr.read`, ...) and per-type
    /// debug helpers (`Int.inspect`, `MyStruct.format`, ...). The
    /// `(base_type, method_name)` pair is the minimum dispatch
    /// identity backends need to route a call to their own emitter.
    pub fn register_intrinsic(
        &mut self,
        mangled: FunctionIdentifier,
        value: FunctionValue<'ctx>,
        base_type: &str,
        method_name: &str,
    ) {
        self.register_function(
            mangled,
            Vec::new(),
            Type::Unknown,
            IRFunctionKind::Intrinsic {
                base_type: base_type.to_string(),
                method_name: method_name.to_string(),
            },
            value,
        );
    }

    /// Convenience wrapper for the compiler-synthesized `fn main`
    /// entry pair: the LLVM `main` C entry that calls
    /// `expo_rt_spawn(__expo_user_main, ...)` and `__expo_user_main`
    /// itself. Transitional helper -- see [`IRFunctionKind::MainEntry`].
    pub fn register_main_entry(&mut self, mangled: FunctionIdentifier, value: FunctionValue<'ctx>) {
        self.register_function(
            mangled,
            Vec::new(),
            Type::Unknown,
            IRFunctionKind::MainEntry,
            value,
        );
    }

    /// Convenience wrapper for non-generic user impl methods.
    /// Mirrors the `Method` registration the monomorphize planner
    /// performs for generic instantiations.
    #[allow(clippy::too_many_arguments)]
    pub fn register_method(
        &mut self,
        mangled: FunctionIdentifier,
        value: FunctionValue<'ctx>,
        func_ast: Function,
        base_type: String,
        mangled_type: MonomorphizedTypeIdentifier,
        self_type: Option<Type>,
        is_static: bool,
    ) {
        let meta = IRFunctionMeta::from_ast(&func_ast);
        self.register_function(
            mangled,
            Vec::new(),
            Type::Unknown,
            IRFunctionKind::Method {
                func_ast,
                meta,
                subst: HashMap::new(),
                base_type,
                mangled_type,
                self_type,
                is_static,
                blocks: Vec::new(),
            },
            value,
        );
    }

    /// Registers a generated forwarding thunk in lockstep across
    /// [`Self::ir`] (canonical symbol registry, kind `Thunk { wraps }`
    /// so backends see what the synthetic body adapts), [`Self::functions`]
    /// (LLVM-handle map, keyed by the thunk's own mangled name), and
    /// [`Self::fn_ref_thunks`] (wraps-keyed cache for O(1)
    /// "do we have a thunk for X?" lookups in
    /// [`Self::get_or_create_thunk`]).
    ///
    /// Like [`Self::register_extern`], the IR signature is recorded as
    /// `Type::Unknown` for now -- the wrapped function carries the real
    /// types and the thunk is purely a calling-convention adapter.
    pub fn register_thunk(
        &mut self,
        wraps: FunctionIdentifier,
        thunk_mangled: FunctionIdentifier,
        thunk_value: FunctionValue<'ctx>,
    ) {
        self.register_function(
            thunk_mangled,
            Vec::new(),
            Type::Unknown,
            IRFunctionKind::Thunk {
                wraps: wraps.clone(),
            },
            thunk_value,
        );
        self.fn_ref_thunks.insert(wraps, thunk_value);
    }

    /// Applies `uwtable` and `frame-pointer=all` to every defined function
    /// in the module so the platform unwinder can walk call stacks for backtraces.
    pub fn apply_unwind_attrs(&self) {
        let uwtable_id = Attribute::get_named_enum_kind_id("uwtable");
        let uwtable_attr = self.context.create_enum_attribute(uwtable_id, 2);
        let fp_attr = self.context.create_string_attribute("frame-pointer", "all");

        let mut func = self.module.get_first_function();
        while let Some(f) = func {
            if f.count_basic_blocks() > 0 {
                f.add_attribute(AttributeLoc::Function, uwtable_attr);
                f.add_attribute(AttributeLoc::Function, fp_attr);
            }
            func = f.get_next_function();
        }
    }

    /// Emits an `alloca` in the current function's entry block so that
    /// the allocation happens exactly once, even when the call-site is
    /// inside a loop.
    pub fn build_entry_alloca(
        &self,
        ty: impl inkwell::types::BasicType<'ctx>,
        name: &str,
    ) -> inkwell::values::PointerValue<'ctx> {
        let current_bb = self.builder.get_insert_block().unwrap();
        let fn_val = current_bb.get_parent().unwrap();
        let entry_bb = fn_val.get_first_basic_block().unwrap();

        if let Some(first_instr) = entry_bb.get_first_instruction() {
            self.builder.position_before(&first_instr);
        } else {
            self.builder.position_at_end(entry_bb);
        }

        let alloca = self.builder.build_alloca(ty, name).unwrap();
        self.builder.position_at_end(current_bb);
        alloca
    }

    /// Call a function, ignoring the return value.
    pub fn call_void(
        &self,
        function: FunctionValue<'ctx>,
        args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) {
        self.builder.build_call(function, args, name).unwrap();
    }

    /// Call a function, returning its value or `None` if it returned void.
    pub fn call(
        &self,
        function: FunctionValue<'ctx>,
        args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> Option<BasicValueEnum<'ctx>> {
        self.builder
            .build_call(function, args, name)
            .unwrap()
            .try_as_basic_value()
            .basic()
    }

    /// Returns true if the current basic block already has a terminator
    /// instruction (branch, return, etc.).
    pub fn current_block_terminated(&self) -> bool {
        self.builder
            .get_insert_block()
            .map(|bb| bb.get_terminator().is_some())
            .unwrap_or(true)
    }

    /// Returns (or generates) a thunk wrapper for a top-level function so it
    /// can be used as a closure-compatible fat pointer. The thunk accepts a
    /// leading `env_ptr` (ignored) then forwards remaining args to the real fn.
    pub fn get_or_create_thunk(&mut self, fn_name: &str) -> Result<FunctionValue<'ctx>, String> {
        let wraps = FunctionIdentifier::new(fn_name);
        if let Some(thunk) = self.fn_ref_thunks.get(&wraps) {
            return Ok(*thunk);
        }

        let target_fn = self.module.get_function(fn_name).ok_or_else(|| {
            format!("cannot create thunk: function `{fn_name}` not found in module")
        })?;

        let target_ty = target_fn.get_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());

        let mut thunk_params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptr_ty.into()];
        for i in 0..target_ty.count_param_types() {
            thunk_params.push(target_ty.get_param_types()[i as usize]);
        }
        let thunk_fn_type = match target_ty.get_return_type() {
            Some(ret) => ret.fn_type(&thunk_params, false),
            None => self.context.void_type().fn_type(&thunk_params, false),
        };

        let thunk_name = format!("{fn_name}__thunk");
        let thunk_fn = self.module.add_function(&thunk_name, thunk_fn_type, None);

        let file = self.debug.file();
        self.debug
            .push_function(thunk_fn, fn_name, &thunk_name, file, 0);

        let entry = self.context.append_basic_block(thunk_fn, "entry");

        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);
        self.debug.set_location(self.context, &self.builder, 0, 0);

        let mut forward_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
        for i in 1..thunk_fn.count_params() {
            forward_args.push(thunk_fn.get_nth_param(i).unwrap().into());
        }

        match self.call(target_fn, &forward_args, "fwd") {
            Some(ret) => self.builder.build_return(Some(&ret)).unwrap(),
            None => self.builder.build_return(None).unwrap(),
        };

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        self.debug.pop_scope(self.context, &self.builder);

        self.register_thunk(wraps, FunctionIdentifier::new(&thunk_name), thunk_fn);
        Ok(thunk_fn)
    }

    /// Creates a length-prefixed string global: `{ i64 bit_length, [N x i8] "bytes\0" }`.
    /// Returns a constant pointer to the payload (past the 8-byte header).
    pub fn create_string_global(&self, bytes: &[u8], name: &str) -> PointerValue<'ctx> {
        let byte_count = bytes.len() as u64;
        let bit_length = byte_count * 8;
        let i64_type = self.context.i64_type();
        let i8_type = self.context.i8_type();
        let str_array_type = i8_type.array_type((byte_count + 1) as u32);
        let header_type = self
            .context
            .struct_type(&[i64_type.into(), str_array_type.into()], false);
        let str_bytes = self.context.const_string(bytes, true);
        let struct_val = header_type.const_named_struct(&[
            i64_type.const_int(bit_length, false).into(),
            str_bytes.into(),
        ]);
        let global = self.module.add_global(header_type, None, name);
        global.set_initializer(&struct_val);
        global.set_constant(true);
        unsafe {
            global
                .as_pointer_value()
                .const_gep(i8_type, &[i64_type.const_int(8, false)])
        }
    }

    /// Writes the compiled LLVM module to a native object file at `path`.
    /// When `release` is true, uses aggressive optimization; otherwise none.
    pub fn emit_object_file(&self, path: &Path, release: bool) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize native target: {e}"))?;

        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("failed to get target: {}", e.to_string()))?;

        let opt_level = if release {
            OptimizationLevel::Aggressive
        } else {
            OptimizationLevel::None
        };

        // Tune the produced binary for the build host. LLVM 18's X86 backend
        // requires a real CPU name (not just "generic") because it constructs
        // a fresh `X86Subtarget` per function during emission and indexes into
        // scheduling tables that are only populated for known CPU models;
        // passing "generic" with empty features SIGSEGVs inside
        // `X86ReadAdvanceTable` on Linux x86_64.
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();
        let machine = target
            .create_target_machine(
                &triple,
                &cpu,
                &features,
                opt_level,
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or("failed to create target machine")?;

        machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|e| format!("failed to write object file: {}", e.to_string()))
    }

    /// Strict lookup for a monomorphized struct's field index by its mangled
    /// key. The key is exactly what registration stores via
    /// [`TypeLayouts::register_struct_layout`]: either a `Type_$Arg$` generic
    /// mangling or a non-generic type's LLVM struct name. No fallbacks.
    /// Thin wrapper kept for caller convenience; the logic lives on
    /// [`TypeLayouts`].
    pub fn get_mono_field_index(&self, mangled: &str, field_name: &str) -> Option<u32> {
        self.layouts
            .field_index(&MonomorphizedTypeIdentifier::new(mangled), field_name)
    }

    /// Strict counterpart of [`Self::get_mono_field_index`] that returns the
    /// field type.
    pub fn get_mono_field_type(&self, mangled: &str, field_name: &str) -> Option<Type> {
        self.layouts
            .field_type(&MonomorphizedTypeIdentifier::new(mangled), field_name)
    }

    /// Declares a function at the LLVM level. `mangling_prefix` is the
    /// (possibly package-qualified) prefix prepended before `_{fn_name}`
    /// for the LLVM symbol name; top-level functions pass `None`. The
    /// prefix is *distinct* from the type's bare name because method
    /// symbols are qualified (`alpha.Config_new`) while the `Self` type
    /// lookup still uses the unqualified type name (`Config`), resolved
    /// under the current package's scope.
    fn declare_function(
        &self,
        func: &Function,
        mangling_prefix: Option<&str>,
        self_type_bare_name: Option<&str>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let return_type = func
            .return_type
            .as_ref()
            .map(|t| resolve_type_expr(&self.lower_ctx(), t))
            .unwrap_or(Type::Unit);
        let mut param_types = Vec::new();

        if let Some(name) = self_type_bare_name
            && func
                .params
                .first()
                .is_some_and(|p| matches!(p, Param::Self_ { .. }))
        {
            let resolved_id = self
                .current_package
                .as_ref()
                .and_then(|pkg| self.type_ctx.resolve_name_scoped(name, pkg));
            if let Some(id) = resolved_id
                && let Some(st) = self.llvm_types.get_concrete(id)
            {
                param_types.push(st.into());
            } else {
                let prim_ty = crate::types::primitive_name_to_type(name);
                if let Some(llvm_ty) = to_llvm_type(&prim_ty, self.context, &self.llvm_types) {
                    param_types.push(llvm_ty.into());
                }
            }
        }

        param_types.extend(self.resolve_param_types(&func.params)?);

        let is_extern_c = is_extern_c_decl(&func.annotations);

        let mangled = if is_extern_c {
            extract_extern_attrs(&func.annotations, false)
                .link_name
                .unwrap_or_else(|| func.name.clone())
        } else {
            match mangling_prefix {
                Some(prefix) => format!("{}_{}", prefix, func.name),
                None => func.name.clone(),
            }
        };

        let fn_type = if func.name == "main" && mangling_prefix.is_none() {
            self.context.i32_type().fn_type(&param_types, false)
        } else {
            match to_llvm_type(&return_type, self.context, &self.llvm_types) {
                Some(ret_ty) => ret_ty.fn_type(&param_types, false),
                None => self.context.void_type().fn_type(&param_types, false),
            }
        };

        if is_extern_c && let Some(existing) = self.module.get_function(&mangled) {
            return Ok(existing);
        }

        Ok(self.module.add_function(&mangled, fn_type, None))
    }

    /// Materialize every [`IRProgram::constants`] entry into an LLVM
    /// value and push it into [`Self::constants`] in `IRConstId`
    /// order. Pure emission -- the entry is already fully resolved.
    /// Slots whose enum / struct identity hasn't registered fall back
    /// to a zero placeholder (the executor surfaces a runtime error
    /// if it's ever indexed).
    fn declare_constants(&mut self) -> Result<(), String> {
        let pool = mem::take(&mut self.ir.constants);
        self.constants = Vec::with_capacity(pool.len());
        for entry in &pool {
            let symbol = entry.identifier.qualified_name();
            let value = self
                .materialize_ir_constant_value(&entry.value, &symbol)
                .unwrap_or_else(|| self.context.i8_type().const_zero().into());
            self.constants.push(value);
        }
        self.ir.constants = pool;
        Ok(())
    }

    /// Look up a package-level constant by source-level `name` from
    /// [`ConstantTables`]. Compounds return the cached
    /// [`Self::constants`] slot; primitives materialize the inline
    /// operand on the fly. Used by the AST-level Stub-fallback path
    /// in [`crate::expr::compile_expr`]; the IR-lifted path emits
    /// [`expo_ir::IRInstruction::LoadConst`] directly. Returns `None`
    /// when no `current_package` is set (no qualified key to build).
    pub fn lookup_const_value(&self, name: &str) -> Option<BasicValueEnum<'ctx>> {
        let const_id = TypeIdentifier {
            package: self.current_package.clone()?,
            name: name.to_string(),
        };
        if let Some(id) = self.const_tables.compounds.get(&const_id).copied() {
            return self.constants.get(id.0 as usize).copied();
        }
        let operand = self.const_tables.primitives.get(&const_id)?;
        Some(self.operand_to_llvm_const(operand, name))
    }

    /// Pure LLVM emission for one [`IRConstantValue`] pool entry.
    /// `name_hint` labels the LLVM symbol for `String` and field
    /// globals; the entry already carries its resolved type identity
    /// and tag / field operands from [`populate_constants`].
    fn materialize_ir_constant_value(
        &self,
        value: &IRConstantValue,
        name_hint: &str,
    ) -> Option<BasicValueEnum<'ctx>> {
        match value {
            IRConstantValue::EnumVariant { enum_id, tag, .. } => {
                let enum_type = self.llvm_types.get_concrete(enum_id)?;
                let tag_val = self.context.i8_type().const_int(*tag as u64, false);
                let value = if enum_type.count_fields() > 1 {
                    let payload_ty = enum_type.get_field_type_at_index(1).unwrap();
                    enum_type
                        .const_named_struct(&[tag_val.into(), payload_ty.const_zero()])
                        .into()
                } else {
                    enum_type.const_named_struct(&[tag_val.into()]).into()
                };
                Some(value)
            }
            IRConstantValue::String(s) => {
                Some(self.create_string_global(s.as_bytes(), name_hint).into())
            }
            IRConstantValue::Struct { struct_id, fields } => {
                let struct_type = self.llvm_types.get_concrete(struct_id)?;
                let values: Vec<BasicValueEnum<'ctx>> = fields
                    .iter()
                    .map(|(field_name, operand)| self.operand_to_llvm_const(operand, field_name))
                    .collect();
                Some(struct_type.const_named_struct(&values).into())
            }
        }
    }

    /// Inline [`IROperand`] -> LLVM constant. Only the four
    /// `Const*` arms are reachable from the constant pool / primitives
    /// map; other arms are produced inside function bodies and never
    /// reach this path.
    fn operand_to_llvm_const(&self, operand: &IROperand, name_hint: &str) -> BasicValueEnum<'ctx> {
        match operand {
            IROperand::ConstBool(b) => self
                .context
                .bool_type()
                .const_int(u64::from(*b), false)
                .into(),
            IROperand::ConstFloat(v) => self.context.f64_type().const_float(*v).into(),
            IROperand::ConstInt(v) => self.context.i64_type().const_int(*v as u64, true).into(),
            IROperand::ConstStr(s) => self.create_string_global(s.as_bytes(), name_hint).into(),
            IROperand::Local(_) | IROperand::Unit => {
                unreachable!("non-const IROperand reached operand_to_llvm_const")
            }
        }
    }

    /// Pre-pass: ensures LLVM struct types exist for all parameter/return types
    /// referenced by a set of functions.
    fn ensure_function_types_exist(&mut self, functions: &[Function]) {
        for func in functions {
            for param in &func.params {
                if let Param::Regular { type_expr, .. } = param {
                    let ty = resolve_type_expr(&self.lower_ctx(), type_expr);
                    let _ = ensure_types_exist(self, &ty);
                }
            }
            if let Some(ret_te) = &func.return_type {
                let ret_ty = resolve_type_expr(&self.lower_ctx(), ret_te);
                let _ = ensure_types_exist(self, &ret_ty);
            }
        }
    }

    /// Declares a set of methods belonging to `type_name`. Mangles as
    /// `{prefix}_{fn_name}` where `prefix` comes from
    /// [`expo_ir::lower::naming::current_method_symbol_prefix`] — stdlib methods stay
    /// unqualified (e.g. `Int_hash`), user-package methods are qualified
    /// (e.g. `alpha.Config_new`). Shared by inline functions and impl blocks.
    fn declare_type_methods(
        &mut self,
        type_name: &str,
        functions: &[Function],
    ) -> Result<(), String> {
        let prefix = current_method_symbol_prefix(&self.lower_ctx(), type_name);
        self.fn_lower.self_type_name = Some(type_name.to_string());
        for func in functions {
            if let Some(rt) = &func.return_type {
                let return_type = resolve_type_expr(&self.lower_ctx(), rt);
                ensure_types_exist(self, &return_type)?;
            }
            for param in &func.params {
                if let Param::Regular { type_expr, .. } = param {
                    let pt = resolve_type_expr(&self.lower_ctx(), type_expr);
                    ensure_types_exist(self, &pt)?;
                }
            }
            let mangled = FunctionIdentifier::new(format!("{prefix}_{}", func.name));
            if self.functions.contains_key(&mangled) {
                continue;
            }
            let fn_value = self.declare_function(func, Some(&prefix), Some(type_name))?;
            if is_extern_c_decl(&func.annotations) {
                let attrs = extract_extern_attrs(&func.annotations, false);
                self.register_extern(mangled, fn_value, attrs);
            } else if is_intrinsic_decl(&func.annotations) {
                self.register_intrinsic(mangled, fn_value, type_name, &func.name);
            } else {
                let is_static = !matches!(func.params.first(), Some(Param::Self_ { .. }));
                let self_type = if is_static {
                    None
                } else {
                    Some(named_generic(
                        type_name,
                        Vec::new(),
                        self.type_ctx,
                        self.current_package.as_ref(),
                    ))
                };
                let mangled_type = MonomorphizedTypeIdentifier::new(prefix.clone());
                self.register_method(
                    mangled,
                    fn_value,
                    func.clone(),
                    type_name.to_string(),
                    mangled_type,
                    self_type,
                    is_static,
                );
            }
        }
        self.fn_lower.self_type_name = None;
        Ok(())
    }

    /// Defines (emits IR bodies for) a set of methods belonging to `type_name`.
    /// Shared by inline functions and impl blocks.
    fn define_type_methods(
        &mut self,
        type_name: &str,
        functions: &[Function],
    ) -> Result<(), String> {
        let prefix = current_method_symbol_prefix(&self.lower_ctx(), type_name);
        for func in functions {
            self.define_function(func, Some(&prefix), Some(type_name))?;
        }
        Ok(())
    }

    fn declare_functions(&mut self, file: &File) -> Result<(), String> {
        for item in &file.items {
            match item {
                Item::Impl(impl_block) => {
                    let fns: Vec<&Function> = impl_block
                        .members
                        .iter()
                        .filter_map(|m| {
                            if let ImplMember::Function(f) = m {
                                Some(f)
                            } else {
                                None
                            }
                        })
                        .collect();
                    for func in &fns {
                        for param in &func.params {
                            if let Param::Regular { type_expr, .. } = param {
                                let ty = resolve_type_expr(&self.lower_ctx(), type_expr);
                                let _ = ensure_types_exist(self, &ty);
                            }
                        }
                        if let Some(ret_te) = &func.return_type {
                            let ret_ty = resolve_type_expr(&self.lower_ctx(), ret_te);
                            let _ = ensure_types_exist(self, &ret_ty);
                        }
                    }
                }
                Item::Struct(s) => self.ensure_function_types_exist(&s.functions),
                Item::Enum(e) => self.ensure_function_types_exist(&e.functions),
                _ => {}
            }
        }

        for item in &file.items {
            match item {
                Item::Function(func) => {
                    if !func.type_params.is_empty() {
                        self.generic_fn_asts.insert(func.name.clone(), func.clone());
                        continue;
                    }
                    let mangled = FunctionIdentifier::new(&func.name);
                    if self.functions.contains_key(&mangled) {
                        continue;
                    }
                    let fn_value = self.declare_function(func, None, None)?;
                    if is_extern_c_decl(&func.annotations) {
                        let attrs = extract_extern_attrs(&func.annotations, false);
                        self.register_extern(mangled, fn_value, attrs);
                    } else if is_intrinsic_decl(&func.annotations) {
                        // Free intrinsics carry an empty base type;
                        // dispatch is keyed off the mangled name alone.
                        self.register_intrinsic(mangled, fn_value, "", &func.name);
                    } else if func.name == "main" {
                        // The LLVM `main` declared here is the synthetic
                        // C entry that wraps the user's body. The body
                        // itself ends up in `__expo_user_main` (see
                        // `define_function`), which also registers as
                        // `MainEntry`. Both halves of the entry pair
                        // share the kind so backends can tag them as
                        // transitional `fn main` synthesis.
                        self.register_main_entry(mangled, fn_value);
                    } else {
                        self.register_free(mangled, fn_value, func.clone());
                    }
                }
                Item::Struct(s) if !s.type_params.is_empty() => {}
                Item::Struct(s) => {
                    self.declare_type_methods(&s.name, &s.functions)?;
                }
                Item::Enum(e) if !e.type_params.is_empty() => {}
                Item::Enum(e) => {
                    self.declare_type_methods(&e.name, &e.functions)?;
                }
                Item::Impl(impl_block) => {
                    let target_name = type_name_from_expr(&impl_block.target);
                    if let Some(target_name) = target_name {
                        let impl_fns: Vec<Function> = impl_block
                            .members
                            .iter()
                            .filter_map(|m| {
                                if let ImplMember::Function(f) = m {
                                    Some(f.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        self.declare_type_methods(&target_name, &impl_fns)?;
                        if let Some(synth_fns) =
                            self.type_ctx.synthesized_default_fns.get(&target_name)
                        {
                            self.declare_type_methods(&target_name, synth_fns)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Emits the LLVM IR body for a single Expo function. Handles parameter
    /// binding (including `self`), implicit return of the last expression, and
    /// auto-inserted terminators for `main`.
    ///
    /// `mangling_prefix` is the (possibly package-qualified) prefix used to
    /// look up the declared LLVM function symbol (e.g. `alpha.Config` →
    /// `alpha.Config_new`). `type_bare_name` is the unqualified type name
    /// (e.g. `Config`) stored in `fn_state.self_type_name` so the body can
    /// resolve `Self` and call impl methods through the usual bare-name
    /// path. Top-level functions pass `None` for both.
    fn define_function(
        &mut self,
        func: &Function,
        mangling_prefix: Option<&str>,
        type_bare_name: Option<&str>,
    ) -> Result<(), String> {
        self.fn_lower.self_type_name = type_bare_name.map(|s| s.to_string());

        let mangled = match mangling_prefix {
            Some(prefix) => format!("{}_{}", prefix, func.name),
            None => func.name.clone(),
        };

        if is_intrinsic_decl(&func.annotations) {
            self.fn_lower.self_type_name = None;
            return crate::intrinsics::emit_primitive_intrinsic(self, &mangled);
        }

        if func.body.is_none() {
            self.fn_lower.self_type_name = None;
            return Ok(());
        }

        let fn_value = *self
            .functions
            .get(&FunctionIdentifier::new(&mangled))
            .ok_or_else(|| format!("undeclared function: {}", mangled))?;

        if fn_value.count_basic_blocks() > 0 {
            self.fn_lower.self_type_name = None;
            return Ok(());
        }

        let is_main = func.name == "main" && mangling_prefix.is_none();

        if is_main {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let user_main_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
            let user_main = self
                .module
                .add_function("__expo_user_main", user_main_ty, None);
            self.register_main_entry(FunctionIdentifier::new("__expo_user_main"), user_main);

            let file = self.debug.file();
            self.debug.push_function(
                user_main,
                "main",
                "__expo_user_main",
                file,
                func.span.start.line,
            );

            let um_entry = self.context.append_basic_block(user_main, "entry");
            self.builder.position_at_end(um_entry);
            self.debug.set_location(
                self.context,
                &self.builder,
                func.span.start.line,
                func.span.start.column,
            );
            self.fn_state.variables.clear();
            self.fn_lower.local_types.clear();
            compile_function_body(
                self,
                func.body.as_deref().unwrap_or(&[]),
                &Type::Unit,
                user_main,
                false,
            )?;

            self.debug.pop_scope(self.context, &self.builder);

            let file = self.debug.file();
            self.debug
                .push_function(fn_value, "main", "main", file, func.span.start.line);

            let main_entry = self.context.append_basic_block(fn_value, "entry");
            self.builder.position_at_end(main_entry);
            self.debug.set_location(
                self.context,
                &self.builder,
                func.span.start.line,
                func.span.start.column,
            );

            let spawn_fn = *self
                .functions
                .get(&FunctionIdentifier::new("expo_rt_spawn"))
                .ok_or("expo_rt_spawn not declared")?;
            let user_main_ptr = user_main.as_global_value().as_pointer_value();
            let null_ptr = ptr_ty.const_null();
            let zero_i64 = self.context.i64_type().const_int(0, false);
            self.call_void(
                spawn_fn,
                &[user_main_ptr.into(), null_ptr.into(), zero_i64.into()],
                "",
            );

            let main_done = *self
                .functions
                .get(&FunctionIdentifier::new("expo_rt_main_done"))
                .ok_or("expo_rt_main_done not declared")?;
            self.call_void(main_done, &[], "");

            let zero_i32 = self.context.i32_type().const_int(0, false);
            self.builder.build_return(Some(&zero_i32)).unwrap();

            self.debug.pop_scope(self.context, &self.builder);

            return Ok(());
        }

        let param_types: Vec<Type> = func
            .params
            .iter()
            .filter_map(|p| {
                if let Param::Regular { type_expr, .. } = p {
                    Some(resolve_type_expr(&self.lower_ctx(), type_expr))
                } else {
                    None
                }
            })
            .collect();

        let return_type = func
            .return_type
            .as_ref()
            .map(|t| resolve_type_expr(&self.lower_ctx(), t))
            .unwrap_or(Type::Unit);

        let self_type = type_bare_name.map(|n| (n, n));
        let result = compile_method_body(
            self,
            fn_value,
            func,
            self_type,
            &param_types,
            &return_type,
            HashMap::new(),
        );
        self.fn_lower.self_type_name = None;
        result
    }

    fn define_functions(&mut self, file: &File) -> Result<(), String> {
        let prev_site = self.closure_site_path.clone();
        self.closure_site_path = file.path.clone();
        let result = self.define_functions_inner(file);
        self.closure_site_path = prev_site;
        result
    }

    fn define_functions_inner(&mut self, file: &File) -> Result<(), String> {
        if let Some(path) = &file.path {
            self.debug.set_current_file(path);
        }

        for item in &file.items {
            match item {
                Item::Function(func) => {
                    if !func.type_params.is_empty() {
                        continue;
                    }
                    self.define_function(func, None, None)?;
                }
                Item::Struct(s) if !s.type_params.is_empty() => {}
                Item::Struct(s) => {
                    self.define_type_methods(&s.name, &s.functions)?;
                }
                Item::Enum(e) if !e.type_params.is_empty() => {}
                Item::Enum(e) => {
                    self.define_type_methods(&e.name, &e.functions)?;
                }
                Item::Impl(impl_block) => {
                    let target_name = type_name_from_expr(&impl_block.target);
                    if let Some(target_name) = target_name {
                        let impl_fns: Vec<Function> = impl_block
                            .members
                            .iter()
                            .filter_map(|m| {
                                if let ImplMember::Function(f) = m {
                                    Some(f.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        self.define_type_methods(&target_name, &impl_fns)?;
                        if let Some(synth_fns) =
                            self.type_ctx.synthesized_default_fns.get(&target_name)
                        {
                            self.define_type_methods(&target_name, synth_fns)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn resolve_param_types(
        &self,
        params: &[Param],
    ) -> Result<Vec<BasicMetadataTypeEnum<'ctx>>, String> {
        let mut types = Vec::new();
        for param in params {
            if let Param::Regular { type_expr, .. } = param {
                let ty = resolve_type_expr(&self.lower_ctx(), type_expr);
                if let Some(llvm_ty) = to_llvm_metadata_type(&ty, self.context, &self.llvm_types) {
                    types.push(llvm_ty);
                }
            }
        }
        Ok(types)
    }

    /// Emits a panic sequence: formats a message into a temporary buffer
    /// via `snprintf`, passes it to `expo_panic_backtrace` (which prints
    /// the message and a symbolicated stack trace), and marks the
    /// insertion point as unreachable.
    ///
    /// `fmt` is a printf-style format string (e.g. `"panic: %s\n"`).
    /// `args` are the values to interpolate into the format string.
    pub fn emit_panic(&self, fmt: &str, args: &[BasicValueEnum<'ctx>]) {
        let snprintf = *self
            .functions
            .get(&FunctionIdentifier::new("snprintf"))
            .expect("snprintf not declared");
        let panic_bt = *self
            .functions
            .get(&FunctionIdentifier::new("expo_panic_backtrace"))
            .expect("expo_panic_backtrace not declared");

        let i32_ty = self.context.i32_type();
        let buf_size = 1024u32;
        let buf = self.build_entry_alloca(self.context.i8_type().array_type(buf_size), "panic_buf");

        let fmt_ptr = self
            .builder
            .build_global_string_ptr(fmt, "panic_fmt")
            .unwrap();

        let mut snprintf_args: Vec<inkwell::values::BasicMetadataValueEnum> = vec![
            buf.into(),
            i32_ty.const_int(buf_size as u64, false).into(),
            fmt_ptr.as_pointer_value().into(),
        ];
        for arg in args {
            snprintf_args.push((*arg).into());
        }
        self.call_void(snprintf, &snprintf_args, "");

        self.call_void(panic_bt, &[buf.into()], "");
        self.builder.build_unreachable().unwrap();
    }

    /// Generates the C `main` function for a Process-based entry point.
    ///
    /// Resolves the `Process<C, M, R>` impl for `type_name`, builds the
    /// child-side spawn wrapper via [`crate::spawn::build_spawn_wrapper`]
    /// (with exit-code tracking), then emits a C `main` that serialises
    /// config, spawns the entry process, and waits for completion.
    fn emit_process_entry(&mut self, type_name: &str) -> Result<(), String> {
        let entry_id = resolve_name_current(&self.lower_ctx(), type_name)
            .cloned()
            .ok_or_else(|| format!("entry type `{type_name}` not found"))?;

        let process_args = self
            .type_ctx
            .protocol_impls
            .get(&entry_id)
            .and_then(|impls| {
                impls
                    .iter()
                    .find(|(proto, _)| proto == "Process")
                    .map(|(_, args)| args.clone())
            })
            .ok_or_else(|| format!("entry type `{type_name}` does not implement Process"))?;

        if process_args.len() < 3 {
            return Err(format!(
                "entry type `{type_name}` has incomplete Process impl (expected 3 type args)"
            ));
        }
        let config_type = &process_args[0];

        let struct_type = self
            .llvm_types
            .get_concrete(&entry_id)
            .ok_or_else(|| format!("entry type `{type_name}` has no LLVM struct layout"))?;

        let config_llvm =
            to_llvm_type(config_type, self.context, &self.llvm_types).ok_or_else(|| {
                format!(
                    "could not resolve LLVM type for config type `{}`",
                    config_type.display()
                )
            })?;

        let method_prefix = method_symbol_prefix(&entry_id.package, &entry_id.name);

        let start_fn_name = format!("{method_prefix}_start");
        let start_fn = self
            .module
            .get_function(&start_fn_name)
            .ok_or_else(|| format!("entry type `{type_name}` has no `start` function"))?;

        let run_fn_name = format!("{method_prefix}_run");
        let run_fn = self
            .module
            .get_function(&run_fn_name)
            .ok_or_else(|| format!("entry type `{type_name}` has no `run` function"))?;

        let code_fn = self
            .module
            .get_function("StopReason_code")
            .ok_or("StopReason_code (ExitStatus impl) not found")?;

        let stop_reason_llvm = self
            .llvm_types
            .get_concrete(&TypeIdentifier::std("StopReason"))
            .ok_or("StopReason LLVM type not found")?;

        let i32_ty = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());

        let exit_code_global = self.module.add_global(i32_ty, None, "__expo_exit_code");
        exit_code_global.set_initializer(&i32_ty.const_int(0, false));

        let exit_ctx = ExitCodeCtx {
            exit_code_global,
            code_fn,
            stop_reason_llvm,
            i32_ty,
        };
        let wrapper_name = format!("__entry_{type_name}");
        let wrapper_fn = spawn::build_spawn_wrapper(
            self,
            &wrapper_name,
            config_llvm,
            struct_type,
            start_fn,
            run_fn,
            Some(&exit_ctx),
        )?;

        // Detect whether C is List<String> for argv passing.
        let is_list_string = config_type.display() == "List<String>";

        let main_fn_type = if is_list_string {
            i32_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false)
        } else {
            i32_ty.fn_type(&[], false)
        };
        let main_fn = self.module.add_function("main", main_fn_type, None);

        let file = self.debug.file();
        self.debug.push_function(main_fn, "main", "main", file, 1);

        let main_entry = self.context.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(main_entry);
        self.debug.set_location(self.context, &self.builder, 1, 1);

        let config_val = if is_list_string {
            let argc_val = main_fn.get_nth_param(0).unwrap().into_int_value();
            let argv_val = main_fn.get_nth_param(1).unwrap().into_pointer_value();
            let void_ty = self.context.void_type();
            let build_argv_type =
                void_ty.fn_type(&[i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
            let build_argv_fn = self
                .module
                .get_function("expo_rt_build_argv")
                .unwrap_or_else(|| {
                    self.module
                        .add_function("expo_rt_build_argv", build_argv_type, None)
                });
            let list_alloca = self
                .builder
                .build_alloca(config_llvm.into_struct_type(), "argv_buf")
                .unwrap();
            self.call_void(
                build_argv_fn,
                &[argc_val.into(), argv_val.into(), list_alloca.into()],
                "",
            );
            self.builder
                .build_load(config_llvm.into_struct_type(), list_alloca, "argv_list")
                .unwrap()
        } else {
            config_llvm.const_zero()
        };

        let serialized = spawn::serialize_config(self, config_val)?;

        let spawn_fn = *self
            .functions
            .get(&FunctionIdentifier::new("expo_rt_spawn"))
            .ok_or("expo_rt_spawn not declared")?;
        let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();
        self.call_void(
            spawn_fn,
            &[
                wrapper_ptr.into(),
                serialized.ptr.into(),
                serialized.size.into(),
            ],
            "",
        );

        let main_done = *self
            .functions
            .get(&FunctionIdentifier::new("expo_rt_main_done"))
            .ok_or("expo_rt_main_done not declared")?;
        self.call_void(main_done, &[], "");

        let final_code = self
            .builder
            .build_load(i32_ty, exit_code_global.as_pointer_value(), "final_code")
            .unwrap();
        self.builder.build_return(Some(&final_code)).unwrap();

        self.debug.pop_scope(self.context, &self.builder);

        Ok(())
    }
}

/// Compiles a single Expo file to a native object file.
pub fn compile(
    file: &File,
    type_ctx: &TypeContext,
    output_path: &Path,
    release: bool,
    app_name: &str,
) -> Result<(), Vec<Diagnostic>> {
    compile_files(&[file], type_ctx, output_path, release, app_name, None)
}

/// Wraps an error string into the `Vec<Diagnostic>` shape that `run_codegen`
/// returns on failure. Centralizes the `Severity::Error` + no-hint pattern
/// used at every codegen call site.
fn codegen_error(message: String, span: Span) -> Vec<Diagnostic> {
    vec![Diagnostic {
        severity: Severity::Error,
        message,
        hint: None,
        span,
    }]
}

/// Runs codegen for all files: register types, declare, define. The
/// owning package per file is read off `file.package` (populated by
/// [`expo_parser::parse_file`] from `SourceFile.package`); every entry
/// must be a real, non-empty package name (the typecheck-side
/// `package_from_str` panics on `""`).
fn run_codegen<'ctx>(
    files: &[&File],
    type_ctx: &'ctx TypeContext,
    context: &'ctx Context,
    release: bool,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<Compiler<'ctx>, Vec<Diagnostic>> {
    let (filename, directory) = files
        .first()
        .and_then(|m| m.path.as_ref())
        .map(|p| {
            let f = p.file_name().and_then(|f| f.to_str()).unwrap_or("unknown");
            let d = p.parent().and_then(|d| d.to_str()).unwrap_or(".");
            (f.to_string(), d.to_string())
        })
        .unwrap_or_else(|| ("unknown".to_string(), ".".to_string()));

    let mut compiler = Compiler::new(context, type_ctx, &filename, &directory, release);

    let app_name_val = context.const_string(app_name.as_bytes(), true);
    let global = compiler
        .module
        .add_global(app_name_val.get_type(), None, "__expo_app_name");
    global.set_initializer(&app_name_val);
    global.set_constant(true);

    register_types(&mut compiler);
    crate::builtins::declare_builtins(&mut compiler);

    for file in files {
        if let Some(path) = &file.path {
            compiler.debug.register_file(path);
        }
    }

    compiler.const_tables = populate_constants(
        files,
        &mut compiler.ir,
        compiler.type_ctx,
        &compiler.layouts,
    );
    let constants_span = files.first().map(|m| m.span).unwrap_or_default();
    compiler
        .declare_constants()
        .map_err(|e| codegen_error(e, constants_span))?;

    for file in files {
        compiler
            .with_package(package_from_str(&file.package), |c| {
                c.declare_functions(file)
            })
            .map_err(|e| codegen_error(e, file.span))?;
    }

    // Impl-block parameter/return types may have monomorphized new generic
    // instances (and enqueued unions) during declaration. Finalize before
    // body compilation so union sizes are correct.
    finalize_pending_unions(&mut compiler);

    let entry_span = files.first().map(|m| m.span).unwrap_or_default();

    // Whole-program monomorphization closure: walks every function
    // body's AST and registers every reachable generic struct / enum
    // / function / (future: method) instantiation in `IRProgram` ahead
    // of body emission. Codegen still has on-demand monomorphization
    // shims as a safety net; they no-op for already-registered decls.
    //
    // `mem::take` shuffles `generic_fn_asts` out of the compiler so the
    // closure pass can read it while `lower_ctx_and_ir` holds a
    // mutable borrow on `compiler.ir`; the asts are restored
    // immediately after.
    {
        let generic_fn_asts = mem::take(&mut compiler.generic_fn_asts);
        let result = {
            let (lower_ctx, ir) = compiler.lower_ctx_and_ir();
            expo_ir::closure_program(ir, lower_ctx.type_ctx, lower_ctx.layouts, &generic_fn_asts)
        };
        compiler.generic_fn_asts = generic_fn_asts;
        result.map_err(|e| codegen_error(e, entry_span))?;
    }

    // Drain every IR decl the closure pass registered into the LLVM
    // caches. The closure pass is LLVM-free (it only mutates
    // `IRProgram`); this pass mirrors each registered struct / enum /
    // function into `c.llvm_types` / `c.functions` so subsequent
    // `compile_*` calls (and the IR-level `IRInstruction::Call` /
    // `StructConstruct` emit walkers) find the LLVM handles they need.
    // Each `emit_ir_*` is idempotent on its respective LLVM cache.
    drain_pending_ir_decls(&mut compiler).map_err(|e| codegen_error(e, entry_span))?;

    finalize_pending_unions(&mut compiler);

    // Elaboration boundary: structural decisions that need a fully-
    // declared IR view land here. No-op today; the seam exists so
    // future protocol-coercion / phi-incoming-coercion / numeric-
    // coercion passes have a single fixed integration point.
    expo_ir::elaborate_program(&mut compiler.ir).map_err(|e| codegen_error(e, entry_span))?;

    for file in files {
        compiler
            .with_package(package_from_str(&file.package), |c| {
                c.define_functions(file)
            })
            .map_err(|e| codegen_error(e, file.span))?;
    }

    finalize_pending_unions(&mut compiler);

    if let Some(type_name) = entry_type {
        compiler
            .with_package(package_from_str(app_name), |c| {
                c.emit_process_entry(type_name)
            })
            .map_err(|e| codegen_error(e, entry_span))?;
    }

    populate_ir_blocks(&mut compiler, files);

    // Verify the closure pass covered every generic instantiation the
    // codegen path had to backfill. Non-zero indicates a closure pass
    // gap; today this is logged advisory-only, but the eventual goal
    // (post-Slice 4 hardening) is `assert_eq!(0)`.
    if compiler.lazy_mono_count > 0 {
        eprintln!(
            "warning: closure pass missed {} generic instantiation(s); \
             codegen lazy path backfilled them",
            compiler.lazy_mono_count
        );
    }

    Ok(compiler)
}

/// Walk every decl the closure pass registered in `compiler.ir` and
/// emit its LLVM declaration via the corresponding `emit_ir_*` helper.
///
/// `emit_ir_struct` / `emit_ir_enum` / `emit_ir_function` are each
/// idempotent on their respective LLVM caches (`llvm_types`,
/// `c.functions`), so this pass is a no-op for decls codegen had
/// already emitted via the legacy lazy-monomorphization path.
///
/// Run between `closure_program` and `define_functions` so subsequent
/// `compile_*` calls + the IR-level `IRInstruction::Call` /
/// `StructConstruct` emit walkers find every monomorphized symbol.
fn drain_pending_ir_decls<'ctx>(compiler: &mut Compiler<'ctx>) -> Result<(), String> {
    let struct_ids: Vec<expo_ir::MonomorphizedTypeIdentifier> = compiler.ir.struct_order.clone();
    for id in struct_ids {
        let decl = compiler.ir.structs.get(&id).cloned();
        if let Some(decl) = decl {
            crate::generics::emit_ir_struct(compiler, &decl)?;
        }
    }
    let enum_ids: Vec<expo_ir::MonomorphizedTypeIdentifier> = compiler.ir.enum_order.clone();
    for id in enum_ids {
        let decl = compiler.ir.enums.get(&id).cloned();
        if let Some(decl) = decl {
            crate::generics::emit_ir_enum(compiler, &decl)?;
        }
    }
    // Function bodies are NOT pre-emitted here. `emit_ir_function` /
    // `emit_ir_impl_method` compile bodies which may reference other
    // unemitted symbols, so a naive `function_order` walk hits
    // dependency-ordering hazards. Instead, the IR-walker's
    // `emit_call` / `emit_method_call` lazy-heal by emitting the
    // callee on demand (see those functions in
    // `crate::control::instructions`). Struct / enum LLVM types are
    // declaration-level and order-independent, which is why they're
    // safe to drain here.
    Ok(())
}

/// Lower every `Free` / `Method` body in `compiler.ir` into its
/// `blocks` field via [`expo_ir::Lowerer::lower_function_body`]. Runs
/// after the LLVM-emitting `define_functions` pass so the IR carries
/// the same body in both representations until the codegen rewrite
/// makes the IR-blocks path the only one.
///
/// Lowering failures are tolerated: bodies that can't lower cleanly
/// are left empty. Backends that walk blocks
/// (e.g. [`crate::lower_files`]'s consumers) treat an empty `blocks`
/// field as "unsupported" and surface a structured error.
fn populate_ir_blocks<'ctx>(compiler: &mut Compiler<'ctx>, files: &[&File]) {
    let fn_packages = build_fn_package_map(files);
    let plans: Vec<LowerPlan> = compiler
        .ir
        .function_order
        .iter()
        .filter_map(|id| capture_lower_plan(&compiler.ir, id, compiler.type_ctx))
        .collect();
    for plan in plans {
        let snapshot = swap_fn_state_for_plan(&mut compiler.fn_lower, &plan);
        let saved_fn = compiler.fn_lower.enter_fn(plan.id.as_str().to_string());
        // Constants are resolved via `Lowerer.package`, which mirrors
        // `Compiler.current_package`. Source-declared free / impl
        // functions get their original package; generic instantiations
        // and other synthesized bodies fall through with `None` (no
        // bare-name const refs in those bodies today).
        let pkg = fn_packages.get(&plan.id).cloned();
        let result = match pkg {
            Some(pkg) => compiler.with_package(pkg, |c| {
                c.lowerer()
                    .lower_function_body(&plan.body, &plan.return_type)
            }),
            None => compiler
                .lowerer()
                .lower_function_body(&plan.body, &plan.return_type),
        };
        compiler.fn_lower.leave_fn(saved_fn);
        restore_fn_state(&mut compiler.fn_lower, snapshot);
        if let Ok(blocks) = result {
            store_ir_blocks(&mut compiler.ir, &plan.id, blocks);
        }
    }
}

/// Map every source-declared function's [`FunctionIdentifier`] to its
/// originating package. Mirrors the mangling
/// [`Compiler::declare_functions`] uses: free fns are bare-name, impl
/// methods are `{target}_{method}`. Used by [`populate_ir_blocks`] to
/// re-establish per-function package context (needed for bare-name
/// const lookups in [`expo_ir::Lowerer::lower_ident_or_stub`]) for the
/// IR-blocks lowering pass, which runs outside the per-file
/// `with_package` loop. Generic instantiations and synthesized bodies
/// (closure pass, intrinsics, etc.) aren't in any file's items and
/// fall through with no package — same as before this map existed.
fn build_fn_package_map(files: &[&File]) -> HashMap<FunctionIdentifier, Package> {
    let mut map: HashMap<FunctionIdentifier, Package> = HashMap::new();
    for file in files {
        let pkg = package_from_str(&file.package);
        for item in &file.items {
            match item {
                Item::Function(func) => {
                    map.entry(FunctionIdentifier::new(&func.name))
                        .or_insert_with(|| pkg.clone());
                }
                Item::Struct(s) => {
                    register_methods(&mut map, &s.name, &s.functions, &pkg);
                }
                Item::Enum(e) => {
                    register_methods(&mut map, &e.name, &e.functions, &pkg);
                }
                Item::Impl(impl_block) => {
                    if let Some(target) = type_name_from_expr(&impl_block.target) {
                        let fns: Vec<Function> = impl_block
                            .members
                            .iter()
                            .filter_map(|m| match m {
                                ImplMember::Function(f) => Some(f.clone()),
                                _ => None,
                            })
                            .collect();
                        register_methods(&mut map, &target, &fns, &pkg);
                    }
                }
                _ => {}
            }
        }
    }
    map
}

fn register_methods(
    map: &mut HashMap<FunctionIdentifier, Package>,
    target: &str,
    fns: &[Function],
    pkg: &Package,
) {
    for func in fns {
        let mangled = format!("{}_{}", target, func.name);
        map.entry(FunctionIdentifier::new(&mangled))
            .or_insert_with(|| pkg.clone());
    }
}

struct LowerPlan {
    id: expo_ir::FunctionIdentifier,
    body: Vec<expo_ast::ast::Statement>,
    return_type: expo_typecheck::types::Type,
    self_type_name: Option<String>,
    /// Parameter (name, type) pairs to seed `FnLowerState.local_types`
    /// before the lowerer walks the body. Includes the implicit
    /// `self` binding for instance methods.
    param_locals: Vec<(String, expo_typecheck::types::Type)>,
}

fn capture_lower_plan(
    program: &expo_ir::IRProgram,
    id: &expo_ir::FunctionIdentifier,
    type_ctx: &TypeContext,
) -> Option<LowerPlan> {
    let function = program.functions.get(id)?;
    let lookup_param_types = |func_ast: &Function| -> Vec<expo_typecheck::types::Type> {
        // `IRFunction.param_types` is empty on main (Free/Method
        // registration uses `Vec::new()`), so fall back to the
        // typecheck-published function signature for the param
        // types the lowerer needs to seed `local_types`.
        if !function.param_types.is_empty() {
            return function.param_types.clone();
        }
        type_ctx
            .functions
            .get(&func_ast.name)
            .map(|sig| sig.params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default()
    };
    let lookup_return_type = |func_ast: &Function| -> expo_typecheck::types::Type {
        // `IRFunction.return_type` is `Type::Unknown` on non-generic
        // user free fns (`register_free` hardcodes it), so fall back
        // to the typecheck-published signature. Without this the
        // lowerer's "fallthrough -> Return None vs Unreachable"
        // dispatch in `lower_function_body` would emit `Unreachable`
        // for any unannotated function whose body lacks a trailing
        // expression -- a runtime crash for the interpreter and an
        // LLVM verification failure for codegen.
        if !matches!(function.return_type, expo_typecheck::types::Type::Unknown) {
            return function.return_type.clone();
        }
        type_ctx
            .functions
            .get(&func_ast.name)
            .map(|sig| sig.return_type.clone())
            .unwrap_or(expo_typecheck::types::Type::Unknown)
    };
    match &function.kind {
        IRFunctionKind::Free {
            func_ast, blocks, ..
        } if blocks.is_empty() => {
            let param_types = lookup_param_types(func_ast);
            Some(LowerPlan {
                id: id.clone(),
                body: func_ast.body.clone().unwrap_or_default(),
                return_type: lookup_return_type(func_ast),
                self_type_name: None,
                param_locals: collect_param_locals(func_ast, &param_types, None),
            })
        }
        IRFunctionKind::Method {
            func_ast,
            base_type,
            self_type,
            blocks,
            ..
        } if blocks.is_empty() => {
            let param_types = lookup_param_types(func_ast);
            Some(LowerPlan {
                id: id.clone(),
                body: func_ast.body.clone().unwrap_or_default(),
                return_type: lookup_return_type(func_ast),
                self_type_name: Some(base_type.clone()),
                param_locals: collect_param_locals(func_ast, &param_types, self_type.as_ref()),
            })
        }
        _ => None,
    }
}

fn collect_param_locals(
    func_ast: &Function,
    param_types: &[expo_typecheck::types::Type],
    self_type: Option<&expo_typecheck::types::Type>,
) -> Vec<(String, expo_typecheck::types::Type)> {
    let mut locals = Vec::new();
    if let Some(self_ty) = self_type {
        locals.push(("self".to_string(), self_ty.clone()));
    }
    let mut idx = 0;
    for param in &func_ast.params {
        if let Param::Regular { name, .. } = param
            && let Some(ty) = param_types.get(idx)
        {
            locals.push((name.clone(), ty.clone()));
            idx += 1;
        }
    }
    locals
}

struct FnStateSnapshot {
    block_counter: u32,
    value_counter: u32,
    local_types: std::collections::HashMap<String, expo_typecheck::types::Type>,
    self_type_name: Option<String>,
}

fn swap_fn_state_for_plan(
    fn_lower: &mut expo_ir::FnLowerState,
    plan: &LowerPlan,
) -> FnStateSnapshot {
    let snapshot = FnStateSnapshot {
        block_counter: fn_lower.block_counter,
        value_counter: fn_lower.value_counter,
        local_types: std::mem::take(&mut fn_lower.local_types),
        self_type_name: fn_lower.self_type_name.take(),
    };
    fn_lower.block_counter = 0;
    fn_lower.value_counter = 0;
    fn_lower.self_type_name = plan.self_type_name.clone();
    for (name, ty) in &plan.param_locals {
        fn_lower.local_types.insert(name.clone(), ty.clone());
    }
    snapshot
}

fn restore_fn_state(fn_lower: &mut expo_ir::FnLowerState, snapshot: FnStateSnapshot) {
    fn_lower.block_counter = snapshot.block_counter;
    fn_lower.value_counter = snapshot.value_counter;
    fn_lower.local_types = snapshot.local_types;
    fn_lower.self_type_name = snapshot.self_type_name;
}

fn store_ir_blocks(
    program: &mut expo_ir::IRProgram,
    id: &expo_ir::FunctionIdentifier,
    new_blocks: Vec<expo_ir::IRBasicBlock>,
) {
    let Some(function) = program.functions.get_mut(id) else {
        return;
    };
    match &mut function.kind {
        IRFunctionKind::Free { blocks, .. } | IRFunctionKind::Method { blocks, .. } => {
            *blocks = new_blocks;
        }
        _ => {}
    }
}

/// Compiles multiple Expo files into a single native object file. Registers
/// types, declares all functions across files, then defines their bodies.
///
/// The owning package per file is read off `file.package` (populated by
/// [`expo_parser::parse_file`] from `SourceFile.package`). `"std"` is the
/// stdlib (unqualified method symbols like `Int_hash`); any other value is
/// a user package whose method symbols are prefixed (e.g. `alpha.Config_new`).
/// Empty strings are rejected by the typecheck-side `package_from_str`.
pub fn compile_files(
    files: &[&File],
    type_ctx: &TypeContext,
    output_path: &Path,
    release: bool,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<(), Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(files, type_ctx, &context, release, app_name, entry_type)?;

    compiler.apply_unwind_attrs();
    compiler.debug.finalize();

    compiler.module.verify().map_err(|e| {
        let span = files.first().map(|m| m.span).unwrap_or_default();
        vec![Diagnostic {
            severity: Severity::Error,
            message: format!("LLVM verification failed: {e}"),
            hint: None,
            span,
        }]
    })?;

    let span = files.first().map(|m| m.span).unwrap_or_default();
    compiler
        .emit_object_file(output_path, release)
        .map_err(|e| {
            vec![Diagnostic {
                severity: Severity::Error,
                message: e,
                hint: None,
                span,
            }]
        })
}

/// Compiles multiple Expo files and returns the LLVM IR as a string.
/// Skips verification so IR can be inspected even when it contains errors.
///
/// See [`compile_files`] for the per-file package semantics.
pub fn emit_llvm_ir(
    files: &[&File],
    type_ctx: &TypeContext,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<String, Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(files, type_ctx, &context, false, app_name, entry_type)?;
    compiler.apply_unwind_attrs();
    compiler.debug.finalize();
    Ok(compiler.module.print_to_string().to_string())
}

/// Lower Expo files into a sealed [`expo_ir::IRProgram`] without
/// emitting an LLVM artifact. Used by execution backends
/// (`expo-ir-eval`, the planned Cranelift JIT) that consume IR directly.
///
/// Today this still constructs an LLVM `Context` internally because
/// lowering shares the [`Compiler`] state machine; a future phase will
/// extract the lowering pipeline so this entry is genuinely LLVM-free.
/// The returned `IRProgram` is the same one a successful
/// [`compile_files`] would have built immediately before
/// `emit_object_file`.
///
/// See [`compile_files`] for the per-file package semantics.
pub fn lower_files(
    files: &[&File],
    type_ctx: &TypeContext,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<IRProgram, Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(files, type_ctx, &context, false, app_name, entry_type)?;
    Ok(compiler.ir)
}

/// Returns the natural ABI alignment (in bytes) of an LLVM type.
fn llvm_type_alignment(ty: inkwell::types::BasicTypeEnum) -> u32 {
    match ty {
        inkwell::types::BasicTypeEnum::IntType(it) => {
            let bytes = it.get_bit_width().div_ceil(8);
            bytes.next_power_of_two().min(8)
        }
        inkwell::types::BasicTypeEnum::FloatType(_) => 8,
        inkwell::types::BasicTypeEnum::PointerType(_) => 8,
        inkwell::types::BasicTypeEnum::StructType(st) => {
            if st.is_packed() {
                return 1;
            }
            st.get_field_types()
                .iter()
                .map(|f| llvm_type_alignment(*f))
                .max()
                .unwrap_or(1)
        }
        inkwell::types::BasicTypeEnum::ArrayType(at) => llvm_type_alignment(at.get_element_type()),
        _ => 8,
    }
}

/// Computes the ABI byte size of an LLVM type, including alignment padding
/// for struct fields. Used for enum payload sizing.
pub(crate) fn llvm_field_byte_size(ty: inkwell::types::BasicTypeEnum) -> u32 {
    match ty {
        inkwell::types::BasicTypeEnum::IntType(it) => it.get_bit_width().div_ceil(8),
        inkwell::types::BasicTypeEnum::FloatType(_) => 8,
        inkwell::types::BasicTypeEnum::PointerType(_) => 8,
        inkwell::types::BasicTypeEnum::StructType(st) => {
            let fields = st.get_field_types();
            if st.is_packed() {
                return fields.iter().map(|f| llvm_field_byte_size(*f)).sum();
            }
            let mut offset: u32 = 0;
            let mut max_align: u32 = 1;
            for f in &fields {
                let align = llvm_type_alignment(*f);
                max_align = max_align.max(align);
                offset = (offset + align - 1) & !(align - 1);
                offset += llvm_field_byte_size(*f);
            }
            (offset + max_align - 1) & !(max_align - 1)
        }
        inkwell::types::BasicTypeEnum::ArrayType(at) => {
            llvm_field_byte_size(at.get_element_type()) * at.len()
        }
        _ => 8,
    }
}

/// Re-exports for the legacy codegen call sites that still use the
/// `*_decl` names; the canonical helpers live in [`expo_ast::ast`].
pub(crate) use expo_ast::ast::is_extern_c as is_extern_c_decl;
pub(crate) use expo_ast::ast::is_intrinsic as is_intrinsic_decl;

/// Builds the [`ExternAttrs`] payload for a user-source `@extern "C"`
/// declaration from its annotations.
///
/// Annotation conventions:
///
/// - `@extern "C"` selects the ABI (the only ABI today).
/// - `@link "lib"` records the linker library; emits `-llib`.
/// - `@link "lib:symbol"` records both the linker library and an
///   override of the LLVM symbol name (so the Expo-source name can
///   differ from the C symbol).
///
/// `is_variadic` is taken from the LLVM `FunctionType` because user
/// `@extern "C"` declarations have no variadic syntax in Expo source
/// today (the typecheck pass rejects it); pass `false` from those
/// call sites.
pub(crate) fn extract_extern_attrs(
    annotations: &[expo_ast::ast::Annotation],
    is_variadic: bool,
) -> ExternAttrs {
    let mut link_lib = None;
    let mut link_name = None;
    for ann in annotations {
        if ann.name != "link" {
            continue;
        }
        let Some(AnnotationValue::String(payload)) = &ann.value else {
            continue;
        };
        match payload.split_once(':') {
            Some((lib, sym)) => {
                link_lib = Some(lib.to_string());
                link_name = Some(sym.to_string());
            }
            None => link_lib = Some(payload.clone()),
        }
    }
    ExternAttrs {
        abi: ExternAbi::C,
        is_variadic,
        link_lib,
        link_name,
    }
}
