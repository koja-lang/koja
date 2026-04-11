//! Compilation driver: holds all LLVM state, registers types, declares and
//! defines functions, and orchestrates emission of native object files.

use std::collections::{BTreeMap, HashMap};
use std::mem;
use std::path::Path;

use crate::debug::synthesize_all_formats;
use crate::drop::Ownership;
use crate::generics::{compile_function_body, compile_method_body, ensure_types_exist};
use crate::registration::register_types;
use crate::util::parse_int_literal;

/// Result of attempting to emit an intrinsic method for a built-in type.
/// `NotIntrinsic` signals the caller to fall through to body compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitResult {
    Emitted,
    NotIntrinsic,
}
use expo_ast::ast::{
    AnnotationValue, Diagnostic, EnumConstructionData, ExprKind, FieldInit, Function, ImplMember,
    Item, Literal, Module, Param, Severity, StringPart, TypeExpr,
};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::{TypeContext, VariantData};
use expo_typecheck::types::{
    Type, build_substitution, named, process_envelope_type, resolve_type_expr_with_params,
    substitute, substitute_preserving,
};
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

/// Tracks state needed to detect and rewrite self-recursive tail calls
/// as loops. Isolated as a struct so it can move independently when
/// `Compiler` is broken into smaller pieces.
pub struct TailCallCtx<'ctx> {
    /// Mangled name of the function currently being compiled.
    current_fn: Option<String>,
    /// Whether the current expression is in tail position.
    tail_position: bool,
    /// Loop header block for the current function. When a self-recursive
    /// tail call is detected, codegen stores new arguments into the
    /// parameter allocas and branches here instead of emitting a call.
    pub loop_header: Option<BasicBlock<'ctx>>,
    /// Parameter allocas in call order (self first, then regular params).
    pub param_allocas: Vec<PointerValue<'ctx>>,
}

impl<'ctx> TailCallCtx<'ctx> {
    pub fn new() -> Self {
        Self {
            current_fn: None,
            tail_position: false,
            loop_header: None,
            param_allocas: Vec::new(),
        }
    }

    /// Set the current function name at method-body entry. Returns the
    /// previous value so the caller can restore it on exit.
    pub fn enter_fn(&mut self, name: String) -> Option<String> {
        self.current_fn.replace(name)
    }

    /// Restore the previous function name when leaving a method body.
    pub fn leave_fn(&mut self, saved: Option<String>) {
        self.current_fn = saved;
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

    /// Restore the previous loop header and parameter allocas.
    pub fn restore_loop(&mut self, saved: (Option<BasicBlock<'ctx>>, Vec<PointerValue<'ctx>>)) {
        self.loop_header = saved.0;
        self.param_allocas = saved.1;
    }

    /// Mark the current compile position as tail position.
    pub fn mark_tail(&mut self) {
        self.tail_position = true;
    }

    /// Clear the tail-position flag.
    pub fn clear_tail(&mut self) {
        self.tail_position = false;
    }

    /// Save and clear the tail-position flag. The flag is cleared so that
    /// subexpressions (receiver, arguments) don't inherit it. The returned
    /// value must be passed to `restore_tail` and `is_self_tail_call`.
    pub fn save_tail(&mut self) -> bool {
        mem::replace(&mut self.tail_position, false)
    }

    /// Restore the tail-position flag after subexpression compilation.
    /// This ensures sibling code paths (other match arms, if/else branches)
    /// still see the flag.
    pub fn restore_tail(&mut self, was_tail: bool) {
        if was_tail {
            self.tail_position = true;
        }
    }

    /// Check whether `callee` is a self-recursive call that should be
    /// rewritten as a loop jump. `was_tail` should come from `save_tail`.
    pub fn is_self_tail_call(&self, callee: &str, was_tail: bool) -> bool {
        was_tail && self.current_fn.as_deref() == Some(callee)
    }
}

/// LLVM struct types, enum payloads, name tables, and monomorphisation info.
/// Populated during type registration / monomorphisation and read during body
/// compilation. Mirrors the read-only `TypeContext` pattern from COMPILER.md.
pub struct TypeRegistry<'ctx> {
    /// Collision-safe map for non-generic types, keyed by package-qualified
    /// `TypeIdentifier`. Used for concrete structs and enums.
    pub concrete: HashMap<TypeIdentifier, StructType<'ctx>>,

    /// Map for monomorphized generic types and unions, keyed by mangled name
    /// strings (e.g. `"List_$Int32$"`, `"Union_$Int.String$"`).
    pub monomorphized: HashMap<String, StructType<'ctx>>,

    pub enum_variant_payloads: HashMap<String, Vec<(String, Option<StructType<'ctx>>)>>,
    pub enum_name_tables: HashMap<String, PointerValue<'ctx>>,
    pub mono_struct_info: HashMap<String, Vec<(String, Type)>>,
    pub mono_enum_variants: HashMap<String, Vec<(String, VariantData)>>,
}

impl<'ctx> TypeRegistry<'ctx> {
    pub fn new() -> Self {
        Self {
            concrete: HashMap::new(),
            monomorphized: HashMap::new(),
            enum_variant_payloads: HashMap::new(),
            enum_name_tables: HashMap::new(),
            mono_struct_info: HashMap::new(),
            mono_enum_variants: HashMap::new(),
        }
    }

    /// Register a non-generic struct or enum type by its package-qualified
    /// identifier.
    pub fn register_concrete(&mut self, id: &TypeIdentifier, ty: StructType<'ctx>) {
        self.concrete.insert(id.clone(), ty);
    }

    /// Register a monomorphized generic or union type by its mangled name.
    pub fn register_monomorphized(&mut self, mangled: String, ty: StructType<'ctx>) {
        self.monomorphized.insert(mangled, ty);
    }

    /// Look up a non-generic type by its package-qualified identifier.
    pub fn get_concrete(&self, id: &TypeIdentifier) -> Option<StructType<'ctx>> {
        self.concrete.get(id).copied()
    }

    /// Look up a monomorphized generic or union type by its mangled name.
    pub fn get_monomorphized(&self, mangled: &str) -> Option<StructType<'ctx>> {
        self.monomorphized.get(mangled).copied()
    }

    /// Look up a type by bare name in the concrete map regardless of
    /// package. Used for intrinsic/stdlib lookups where the caller only
    /// knows the type name (e.g. `"Fd"`, `"Socket"`, `"StopReason"`).
    pub fn get_stdlib(&self, name: &str) -> Option<StructType<'ctx>> {
        self.concrete
            .iter()
            .find(|(id, _)| id.name == name)
            .map(|(_, ty)| *ty)
    }

    /// Check whether a monomorphized type is registered.
    pub fn contains_monomorphized(&self, mangled: &str) -> bool {
        self.monomorphized.contains_key(mangled)
    }

    /// Returns the LLVM struct type for an enum variant's payload, if it has one.
    pub fn get_variant_payload_type(
        &self,
        enum_name: &str,
        variant_name: &str,
    ) -> Option<StructType<'ctx>> {
        self.enum_variant_payloads.get(enum_name).and_then(|vs| {
            vs.iter()
                .find(|(name, _)| name == variant_name)
                .and_then(|(_, pt)| *pt)
        })
    }

    /// Returns the tag index (0-based) for an enum variant.
    pub fn get_variant_tag(&self, enum_name: &str, variant_name: &str) -> Option<u8> {
        self.enum_variant_payloads.get(enum_name).and_then(|vs| {
            vs.iter()
                .position(|(name, _)| name == variant_name)
                .map(|i| i as u8)
        })
    }
}

/// Per-function ephemeral state that is set/reset at each `define_function`
/// call. Extends the pattern established by `TailCallCtx`.
pub struct FnState<'ctx> {
    pub variables: BTreeMap<String, (PointerValue<'ctx>, Type, Ownership)>,
    pub loop_exit_stack: Vec<BasicBlock<'ctx>>,
    pub process_msg_type: Option<Type>,
    pub return_type_hint: Option<Type>,
    pub type_subst: HashMap<String, Type>,
    pub tco: TailCallCtx<'ctx>,
    pub closure_counter: usize,
    /// When inside an `impl` block, the concrete type name (e.g. "Counter").
    /// Used by `resolve_type_expr` to substitute `Self` automatically.
    pub self_type_name: Option<String>,
}

impl<'ctx> FnState<'ctx> {
    pub fn new() -> Self {
        Self {
            variables: BTreeMap::new(),
            loop_exit_stack: Vec::new(),
            process_msg_type: None,
            return_type_hint: None,
            type_subst: HashMap::new(),
            tco: TailCallCtx::new(),
            closure_counter: 0,
            self_type_name: None,
        }
    }
}

/// Holds all LLVM state needed to compile an Expo module: the LLVM context,
/// module, builder, declared functions, variable bindings, and type mappings.
pub struct Compiler<'ctx> {
    pub context: &'ctx Context,
    pub module: LlvmModule<'ctx>,
    pub builder: Builder<'ctx>,
    pub constants: HashMap<String, BasicValueEnum<'ctx>>,
    pub functions: HashMap<String, FunctionValue<'ctx>>,
    pub type_ctx: &'ctx TypeContext,
    pub generic_fn_asts: HashMap<String, Function>,
    /// Cache of generated thunk wrappers for bare function references.
    /// Maps original function name to the thunk `FunctionValue`.
    pub fn_ref_thunks: HashMap<String, FunctionValue<'ctx>>,
    /// Type registry: LLVM struct types, enum payloads, and monomorphisation data.
    pub types: TypeRegistry<'ctx>,
    /// Per-function ephemeral state (variables, loops, TCO, etc.).
    pub fn_state: FnState<'ctx>,
    /// DWARF debug info state (always present; emitted in all builds).
    pub debug: DebugContext<'ctx>,
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
        Self {
            context,
            module,
            builder,
            constants: HashMap::new(),
            functions: HashMap::new(),
            type_ctx,
            generic_fn_asts: HashMap::new(),
            fn_ref_thunks: HashMap::new(),
            types: TypeRegistry::new(),
            fn_state: FnState::new(),
            debug,
        }
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
        if let Some(thunk) = self.fn_ref_thunks.get(fn_name) {
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

        self.fn_ref_thunks.insert(fn_name.to_string(), thunk_fn);
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

        let machine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                opt_level,
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or("failed to create target machine")?;

        machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|e| format!("failed to write object file: {}", e.to_string()))
    }

    /// Returns the LLVM struct field index for the given struct and field name.
    pub fn get_field_index(&self, struct_name: &str, field_name: &str) -> Option<u32> {
        if let Some(fields) = self.types.mono_struct_info.get(struct_name) {
            return fields
                .iter()
                .position(|(name, _)| name == field_name)
                .map(|i| i as u32);
        }
        self.type_ctx.find_type(struct_name).and_then(|info| {
            info.fields().and_then(|fields| {
                fields
                    .iter()
                    .position(|(name, _)| name == field_name)
                    .map(|i| i as u32)
            })
        })
    }

    /// Returns the Expo type of a struct field.
    pub fn get_field_type(&self, struct_name: &str, field_name: &str) -> Option<Type> {
        if let Some(fields) = self.types.mono_struct_info.get(struct_name) {
            return fields
                .iter()
                .find(|(name, _)| name == field_name)
                .map(|(_, ty)| ty.clone());
        }
        self.type_ctx.find_type(struct_name).and_then(|info| {
            info.fields().and_then(|fields| {
                fields
                    .iter()
                    .find(|(name, _)| name == field_name)
                    .map(|(_, ty)| ty.clone())
            })
        })
    }

    /// Resolves a type expression AST node into an Expo type, using the
    /// currently registered struct and enum names for lookup. When inside an
    /// `impl` block (`fn_state.self_type_name` is set), `Self` is automatically
    /// substituted with the concrete target type.
    pub fn resolve_type_expr(&self, type_expr: &TypeExpr) -> Type {
        let struct_names: Vec<&str> = self
            .type_ctx
            .types
            .iter()
            .filter(|(_, ti)| ti.is_struct())
            .map(|(name, _)| name.name.as_str())
            .collect();
        let enum_names: Vec<&str> = self
            .type_ctx
            .types
            .iter()
            .filter(|(_, ti)| ti.is_enum())
            .map(|(name, _)| name.name.as_str())
            .collect();
        let mut type_params: Vec<&str> = self
            .fn_state
            .type_subst
            .keys()
            .map(|s| s.as_str())
            .collect();
        if self.fn_state.self_type_name.is_some() && !type_params.contains(&"Self") {
            type_params.push("Self");
        }
        let mut ty = resolve_type_expr_with_params(
            type_expr,
            &struct_names,
            &enum_names,
            &type_params,
            &self.type_ctx.type_aliases,
        );
        self.type_ctx.resolve_type(&mut ty);
        if let Some(ref name) = self.fn_state.self_type_name {
            let self_ty = if self.type_ctx.is_struct(name) || self.type_ctx.is_enum(name) {
                named(name)
            } else {
                return substitute_preserving(&ty, &self.fn_state.type_subst);
            };
            let mut subst = self.fn_state.type_subst.clone();
            subst.insert("Self".to_string(), self_ty);
            substitute_preserving(&ty, &subst)
        } else {
            substitute_preserving(&ty, &self.fn_state.type_subst)
        }
    }

    fn declare_function(
        &self,
        func: &Function,
        self_type_name: Option<&str>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let return_type = func
            .return_type
            .as_ref()
            .map(|t| self.resolve_type_expr(t))
            .unwrap_or(Type::Unit);
        let mut param_types = Vec::new();

        if let Some(name) = self_type_name
            && func
                .params
                .first()
                .is_some_and(|p| matches!(p, Param::Self_ { .. }))
        {
            if let Some(st) = self.types.get_stdlib(name) {
                param_types.push(st.into());
            } else {
                let prim_ty = crate::types::primitive_name_to_type(name);
                if let Some(llvm_ty) = to_llvm_type(&prim_ty, self.context, &self.types) {
                    param_types.push(llvm_ty.into());
                }
            }
        }

        param_types.extend(self.resolve_param_types(&func.params)?);

        let is_extern_c = func.annotations.iter().any(|a| {
            a.name == "extern" && matches!(&a.value, Some(AnnotationValue::String(s)) if s == "C")
        });

        let mangled = if is_extern_c {
            extract_link_symbol(&func.annotations).unwrap_or_else(|| func.name.clone())
        } else {
            match self_type_name {
                Some(tn) => format!("{}_{}", tn, func.name),
                None => func.name.clone(),
            }
        };

        let fn_type = if func.name == "main" && self_type_name.is_none() {
            self.context.i32_type().fn_type(&param_types, false)
        } else {
            match to_llvm_type(&return_type, self.context, &self.types) {
                Some(ret_ty) => ret_ty.fn_type(&param_types, false),
                None => self.context.void_type().fn_type(&param_types, false),
            }
        };

        if is_extern_c && let Some(existing) = self.module.get_function(&mangled) {
            return Ok(existing);
        }

        Ok(self.module.add_function(&mangled, fn_type, None))
    }

    fn declare_constants(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
            if let Item::Constant(c) = item {
                let val: BasicValueEnum = match &c.value.kind {
                    ExprKind::Literal {
                        value: Literal::Int(s),
                        ..
                    } => {
                        let v = parse_int_literal(s)?;
                        self.context.i64_type().const_int(v as u64, true).into()
                    }
                    ExprKind::Literal {
                        value: Literal::Float(s),
                        ..
                    } => {
                        let v: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
                        self.context.f64_type().const_float(v).into()
                    }
                    ExprKind::Literal {
                        value: Literal::Bool(b),
                        ..
                    } => self
                        .context
                        .bool_type()
                        .const_int(if *b { 1 } else { 0 }, false)
                        .into(),
                    ExprKind::String { parts, .. } => {
                        let mut combined = String::new();
                        for part in parts {
                            if let StringPart::Literal { value, .. } = part {
                                combined.push_str(value);
                            }
                        }
                        self.create_string_global(combined.as_bytes(), &c.name)
                            .into()
                    }
                    ExprKind::EnumConstruction {
                        type_path,
                        variant,
                        data: EnumConstructionData::Unit,
                        ..
                    } => {
                        let enum_name = type_path.join(".");
                        let Some(enum_type) = self.types.get_stdlib(&enum_name) else {
                            continue;
                        };
                        let Some(tag) = self.types.get_variant_tag(&enum_name, variant) else {
                            continue;
                        };
                        let tag_val = self.context.i8_type().const_int(tag as u64, false);
                        let field_count = enum_type.count_fields();
                        if field_count > 1 {
                            let payload_ty = enum_type.get_field_type_at_index(1).unwrap();
                            let zero_payload = payload_ty.const_zero();
                            enum_type
                                .const_named_struct(&[tag_val.into(), zero_payload])
                                .into()
                        } else {
                            enum_type.const_named_struct(&[tag_val.into()]).into()
                        }
                    }
                    ExprKind::StructConstruction {
                        type_path, fields, ..
                    } => {
                        let struct_name = type_path.join(".");
                        let Some(struct_type) = self.types.get_stdlib(&struct_name) else {
                            continue;
                        };
                        let Some(info) = self.type_ctx.find_type(&struct_name) else {
                            continue;
                        };
                        let Some(struct_fields) = info.fields() else {
                            continue;
                        };
                        match self.build_const_struct(struct_type, struct_fields, fields) {
                            Some(val) => val,
                            None => continue,
                        }
                    }
                    _ => continue,
                };
                self.constants.insert(c.name.clone(), val);
            }
        }
        Ok(())
    }

    fn build_const_struct(
        &self,
        struct_type: StructType<'ctx>,
        struct_fields: &[(String, Type)],
        field_inits: &[FieldInit],
    ) -> Option<BasicValueEnum<'ctx>> {
        let mut values: Vec<BasicValueEnum<'ctx>> =
            vec![self.context.i8_type().const_zero().into(); struct_fields.len()];
        for fi in field_inits {
            let idx = struct_fields.iter().position(|(n, _)| *n == fi.name)?;
            let val: BasicValueEnum = match &fi.value.kind {
                ExprKind::Literal {
                    value: Literal::Int(s),
                    ..
                } => {
                    let v = parse_int_literal(s).ok()?;
                    self.context.i64_type().const_int(v as u64, true).into()
                }
                ExprKind::Literal {
                    value: Literal::Float(s),
                    ..
                } => {
                    let v: f64 = s.parse().ok()?;
                    self.context.f64_type().const_float(v).into()
                }
                ExprKind::Literal {
                    value: Literal::Bool(b),
                    ..
                } => self
                    .context
                    .bool_type()
                    .const_int(if *b { 1 } else { 0 }, false)
                    .into(),
                ExprKind::String { parts, .. } => {
                    let mut combined = String::new();
                    for part in parts {
                        if let StringPart::Literal { value, .. } = part {
                            combined.push_str(value);
                        }
                    }
                    self.create_string_global(combined.as_bytes(), &fi.name)
                        .into()
                }
                _ => return None,
            };
            values[idx] = val;
        }
        Some(struct_type.const_named_struct(&values).into())
    }

    /// Pre-pass: ensures LLVM struct types exist for all parameter/return types
    /// referenced by a set of functions.
    fn ensure_function_types_exist(&mut self, functions: &[Function]) {
        for func in functions {
            for param in &func.params {
                if let Param::Regular { type_expr, .. } = param {
                    let ty = self.resolve_type_expr(type_expr);
                    let _ = ensure_types_exist(self, &ty);
                }
            }
            if let Some(ret_te) = &func.return_type {
                let ret_ty = self.resolve_type_expr(ret_te);
                let _ = ensure_types_exist(self, &ret_ty);
            }
        }
    }

    /// Declares a set of methods belonging to `type_name`, mangling as
    /// `{TypeName}_{fn_name}`. Shared by inline functions and impl blocks.
    fn declare_type_methods(
        &mut self,
        type_name: &str,
        functions: &[Function],
    ) -> Result<(), String> {
        self.fn_state.self_type_name = Some(type_name.to_string());
        for func in functions {
            if let Some(rt) = &func.return_type {
                let return_type = self.resolve_type_expr(rt);
                ensure_types_exist(self, &return_type)?;
            }
            for param in &func.params {
                if let Param::Regular { type_expr, .. } = param {
                    let pt = self.resolve_type_expr(type_expr);
                    ensure_types_exist(self, &pt)?;
                }
            }
            let mangled = format!("{type_name}_{}", func.name);
            if self.functions.contains_key(&mangled) {
                continue;
            }
            let fn_value = self.declare_function(func, Some(type_name))?;
            self.functions.insert(mangled, fn_value);
        }
        self.fn_state.self_type_name = None;
        Ok(())
    }

    /// Defines (emits IR bodies for) a set of methods belonging to `type_name`.
    /// Shared by inline functions and impl blocks.
    fn define_type_methods(
        &mut self,
        type_name: &str,
        functions: &[Function],
    ) -> Result<(), String> {
        for func in functions {
            self.define_function(func, Some(type_name))?;
        }
        Ok(())
    }

    fn declare_functions(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
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
                                let ty = self.resolve_type_expr(type_expr);
                                let _ = ensure_types_exist(self, &ty);
                            }
                        }
                        if let Some(ret_te) = &func.return_type {
                            let ret_ty = self.resolve_type_expr(ret_te);
                            let _ = ensure_types_exist(self, &ret_ty);
                        }
                    }
                }
                Item::Struct(s) => self.ensure_function_types_exist(&s.functions),
                Item::Enum(e) => self.ensure_function_types_exist(&e.functions),
                _ => {}
            }
        }

        for item in &module.items {
            match item {
                Item::Function(func) => {
                    if !func.type_params.is_empty() {
                        self.generic_fn_asts.insert(func.name.clone(), func.clone());
                        continue;
                    }
                    if self.functions.contains_key(&func.name) {
                        continue;
                    }
                    let fn_value = self.declare_function(func, None)?;
                    self.functions.insert(func.name.clone(), fn_value);
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
                    let target_name = self.type_name_from_expr(&impl_block.target);
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
    fn define_function(
        &mut self,
        func: &Function,
        self_type_name: Option<&str>,
    ) -> Result<(), String> {
        if func.body.is_none() {
            return Ok(());
        }
        self.fn_state.self_type_name = self_type_name.map(|s| s.to_string());

        let mangled = match self_type_name {
            Some(tn) => format!("{}_{}", tn, func.name),
            None => func.name.clone(),
        };

        if crate::intrinsics::is_primitive_intrinsic(&mangled) {
            self.fn_state.self_type_name = None;
            return crate::intrinsics::emit_primitive_intrinsic(self, &mangled);
        }

        let fn_value = *self
            .functions
            .get(&mangled)
            .ok_or_else(|| format!("undeclared function: {}", mangled))?;

        if fn_value.count_basic_blocks() > 0 {
            self.fn_state.self_type_name = None;
            return Ok(());
        }

        let is_main = func.name == "main" && self_type_name.is_none();

        if is_main {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let user_main_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
            let user_main = self
                .module
                .add_function("__expo_user_main", user_main_ty, None);
            self.functions
                .insert("__expo_user_main".to_string(), user_main);

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
                .get("expo_rt_spawn")
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
                .get("expo_rt_main_done")
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
                    Some(self.resolve_type_expr(type_expr))
                } else {
                    None
                }
            })
            .collect();

        let return_type = func
            .return_type
            .as_ref()
            .map(|t| self.resolve_type_expr(t))
            .unwrap_or(Type::Unit);

        let self_type = self_type_name.map(|n| (n, n));
        let result = compile_method_body(
            self,
            fn_value,
            func,
            self_type,
            &param_types,
            &return_type,
            HashMap::new(),
        );
        self.fn_state.self_type_name = None;
        result
    }

    fn define_functions(&mut self, module: &Module) -> Result<(), String> {
        if let Some(path) = &module.path {
            self.debug.set_current_file(path);
        }

        for item in &module.items {
            match item {
                Item::Function(func) => {
                    if !func.type_params.is_empty() {
                        continue;
                    }
                    self.define_function(func, None)?;
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
                    let target_name = self.type_name_from_expr(&impl_block.target);
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
                let ty = self.resolve_type_expr(type_expr);
                if let Some(llvm_ty) = to_llvm_metadata_type(&ty, self.context, &self.types) {
                    types.push(llvm_ty);
                }
            }
        }
        Ok(types)
    }

    fn type_name_from_expr(&self, te: &TypeExpr) -> Option<String> {
        if let TypeExpr::Named { path, .. } = te
            && path.len() == 1
        {
            return Some(path[0].clone());
        }
        None
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
            .get("snprintf")
            .expect("snprintf not declared");
        let panic_bt = *self
            .functions
            .get("expo_panic_backtrace")
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
        use crate::spawn::{self, ExitCodeCtx};

        let process_args = self
            .type_ctx
            .protocol_impls
            .get(type_name)
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
            .types
            .get_stdlib(type_name)
            .ok_or_else(|| format!("entry type `{type_name}` has no LLVM struct layout"))?;

        let config_llvm =
            to_llvm_type(config_type, self.context, &self.types).ok_or_else(|| {
                format!(
                    "could not resolve LLVM type for config type `{}`",
                    config_type.display()
                )
            })?;

        let start_fn_name = format!("{type_name}_start");
        let start_fn = self
            .module
            .get_function(&start_fn_name)
            .ok_or_else(|| format!("entry type `{type_name}` has no `start` function"))?;

        let run_fn_name = format!("{type_name}_run");
        let run_fn = self
            .module
            .get_function(&run_fn_name)
            .ok_or_else(|| format!("entry type `{type_name}` has no `run` function"))?;

        let code_fn = self
            .module
            .get_function("StopReason_code")
            .ok_or("StopReason_code (ExitStatus impl) not found")?;

        let stop_reason_llvm = self
            .types
            .get_stdlib("StopReason")
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
            .get("expo_rt_spawn")
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
            .get("expo_rt_main_done")
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

/// Compiles a single Expo module to a native object file.
pub fn compile(
    module: &Module,
    type_ctx: &TypeContext,
    output_path: &Path,
    release: bool,
    app_name: &str,
) -> Result<(), Vec<Diagnostic>> {
    compile_modules(&[module], type_ctx, output_path, release, app_name, None)
}

/// Runs codegen for all modules: register types, declare, define.
fn run_codegen<'ctx>(
    modules: &[&Module],
    type_ctx: &'ctx TypeContext,
    context: &'ctx Context,
    release: bool,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<Compiler<'ctx>, Vec<Diagnostic>> {
    let (filename, directory) = modules
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
    crate::builtins::declare_builtins(compiler.context, &compiler.module, &mut compiler.functions);

    for module in modules {
        if let Some(path) = &module.path {
            compiler.debug.register_file(path);
        }
    }

    for module in modules {
        compiler.declare_constants(module).map_err(|e| {
            vec![Diagnostic {
                severity: Severity::Error,
                message: e,
                hint: None,
                span: module.span,
            }]
        })?;
    }

    for module in modules {
        compiler.declare_functions(module).map_err(|e| {
            vec![Diagnostic {
                severity: Severity::Error,
                message: e,
                hint: None,
                span: module.span,
            }]
        })?;
    }

    synthesize_all_formats(&mut compiler).map_err(|e| {
        let span = modules.first().map(|m| m.span).unwrap_or_default();
        vec![Diagnostic {
            severity: Severity::Error,
            message: e,
            hint: None,
            span,
        }]
    })?;

    for module in modules {
        compiler.define_functions(module).map_err(|e| {
            vec![Diagnostic {
                severity: Severity::Error,
                message: e,
                hint: None,
                span: module.span,
            }]
        })?;
    }

    if let Some(type_name) = entry_type {
        let span = modules.first().map(|m| m.span).unwrap_or_default();
        compiler.emit_process_entry(type_name).map_err(|e| {
            vec![Diagnostic {
                severity: Severity::Error,
                message: e,
                hint: None,
                span,
            }]
        })?;
    }

    Ok(compiler)
}

/// Compiles multiple Expo modules into a single native object file. Registers
/// types, declares all functions across modules, then defines their bodies.
pub fn compile_modules(
    modules: &[&Module],
    type_ctx: &TypeContext,
    output_path: &Path,
    release: bool,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<(), Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(modules, type_ctx, &context, release, app_name, entry_type)?;

    compiler.apply_unwind_attrs();
    compiler.debug.finalize();

    compiler.module.verify().map_err(|e| {
        let span = modules.first().map(|m| m.span).unwrap_or_default();
        vec![Diagnostic {
            severity: Severity::Error,
            message: format!("LLVM verification failed: {e}"),
            hint: None,
            span,
        }]
    })?;

    let span = modules.first().map(|m| m.span).unwrap_or_default();
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

/// Compiles multiple Expo modules and returns the LLVM IR as a string.
/// Skips verification so IR can be inspected even when it contains errors.
pub fn emit_llvm_ir(
    modules: &[&Module],
    type_ctx: &TypeContext,
    app_name: &str,
    entry_type: Option<&str>,
) -> Result<String, Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(modules, type_ctx, &context, false, app_name, entry_type)?;
    compiler.apply_unwind_attrs();
    compiler.debug.finalize();
    Ok(compiler.module.print_to_string().to_string())
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

/// Extracts the C symbol name from a `@link "lib:symbol"` annotation.
/// Returns `Some("symbol")` if the colon convention is used, `None` otherwise.
fn extract_link_symbol(annotations: &[expo_ast::ast::Annotation]) -> Option<String> {
    annotations.iter().find_map(|a| {
        if a.name == "link"
            && let Some(AnnotationValue::String(s)) = &a.value
        {
            return s.split_once(':').map(|(_, sym)| sym.to_string());
        }
        None
    })
}

/// Resolves the mailbox message type `Pair<M, Option<ReplyTo<R>>>` for `receive`
/// when compiling a `Process` impl method. Uses an exact `protocol_impls` key
/// (e.g. `Task`) or, for monomorphized impls, the base type name plus substitution
/// from the mangled self type (e.g. `Task_$Int$`).
pub(crate) fn resolve_process_envelope_type<'ctx>(
    c: &Compiler<'ctx>,
    target: &str,
) -> Option<Type> {
    if let Some(impls) = c.type_ctx.protocol_impls.get(target)
        && let Some((_, args)) = impls.iter().find(|(proto, _)| proto == "Process")
    {
        let m = args.get(1)?;
        let r = args.get(2)?;
        return Some(process_envelope_type(m, r));
    }
    if let Some((base, type_args)) = crate::generics::try_parse_mangled_name(target, c) {
        let impls = c.type_ctx.protocol_impls.get(&base)?;
        let (_, proto_args) = impls.iter().find(|(proto, _)| proto == "Process")?;
        let ti = c.type_ctx.find_type(&base)?;
        let subst = build_substitution(&ti.type_params, &type_args);
        let m = substitute(proto_args.get(1)?, &subst);
        let r = substitute(proto_args.get(2)?, &subst);
        return Some(process_envelope_type(&m, &r));
    }
    None
}
