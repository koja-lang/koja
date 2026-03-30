//! Compilation driver: holds all LLVM state, registers types, declares and
//! defines functions, and orchestrates emission of native object files.

use std::collections::{BTreeMap, HashMap};
use std::mem;
use std::path::Path;

use crate::drop::Ownership;

/// Result of attempting to emit an intrinsic method for a built-in type.
/// `NotIntrinsic` signals the caller to fall through to body compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitResult {
    Emitted,
    NotIntrinsic,
}
use expo_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, FieldInit, Function, ImplMember, Item, Literal, Module,
    Param, Severity, StringPart, TypeExpr,
};
use expo_typecheck::context::{TypeContext, VariantData};
use expo_typecheck::types::{
    Type, build_substitution, process_envelope_type, resolve_type_expr_with_params, substitute,
    substitute_preserving,
};
use inkwell::OptimizationLevel;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module as LlvmModule;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

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

/// Holds all LLVM state needed to compile an Expo module: the LLVM context,
/// module, builder, declared functions, variable bindings, and type mappings.
pub struct Compiler<'ctx> {
    pub context: &'ctx Context,
    pub module: LlvmModule<'ctx>,
    pub builder: Builder<'ctx>,
    pub constants: HashMap<String, BasicValueEnum<'ctx>>,
    pub functions: HashMap<String, FunctionValue<'ctx>>,
    pub variables: BTreeMap<String, (PointerValue<'ctx>, Type, Ownership)>,
    pub struct_types: HashMap<String, StructType<'ctx>>,
    pub enum_variant_payloads: HashMap<String, Vec<(String, Option<StructType<'ctx>>)>>,
    pub enum_name_tables: HashMap<String, PointerValue<'ctx>>,
    pub loop_exit_stack: Vec<BasicBlock<'ctx>>,
    pub type_ctx: &'ctx TypeContext,
    pub closure_counter: usize,
    pub generic_fn_asts: HashMap<String, Function>,
    pub mono_struct_info: HashMap<String, Vec<(String, Type)>>,
    pub mono_enum_variants: HashMap<String, Vec<(String, VariantData)>>,
    /// Active type substitution during monomorphized body compilation.
    /// Maps type parameter names (e.g. "T", "U") to concrete types.
    pub type_subst: HashMap<String, Type>,
    /// The message type M for the current process function, used by `receive`
    /// codegen to determine the LLVM type to load from the mailbox pointer.
    pub process_msg_type: Option<Type>,
    /// Cache of generated thunk wrappers for bare function references.
    /// Maps original function name to the thunk `FunctionValue`.
    pub fn_ref_thunks: HashMap<String, FunctionValue<'ctx>>,
    /// Return type of the function currently being compiled. Used by
    /// generic enum construction to resolve unit variant type args when
    /// they can't be inferred from arguments (e.g. `Option.None` inside
    /// a method with its own type parameters like `map<U>`).
    pub return_type_hint: Option<Type>,
    /// Self-recursive tail call optimization state.
    pub tco: TailCallCtx<'ctx>,
}

impl<'ctx> Compiler<'ctx> {
    /// Creates a new compiler instance with an empty LLVM module.
    pub fn new(context: &'ctx Context, type_ctx: &'ctx TypeContext) -> Self {
        let module = context.create_module("expo_module");
        let builder = context.create_builder();
        Self {
            context,
            module,
            builder,
            constants: HashMap::new(),
            functions: HashMap::new(),
            variables: BTreeMap::new(),
            struct_types: HashMap::new(),
            enum_variant_payloads: HashMap::new(),
            enum_name_tables: HashMap::new(),
            loop_exit_stack: Vec::new(),
            type_ctx,
            closure_counter: 0,
            generic_fn_asts: HashMap::new(),
            mono_struct_info: HashMap::new(),
            mono_enum_variants: HashMap::new(),
            type_subst: HashMap::new(),
            process_msg_type: None,
            fn_ref_thunks: HashMap::new(),
            return_type_hint: None,
            tco: TailCallCtx::new(),
        }
    }

    /// Maps an LLVM struct type back to the Expo mangled name (e.g. `Task_$Int$`).
    pub fn mangled_name_for_struct_type(&self, st: StructType<'ctx>) -> Option<String> {
        self.struct_types
            .iter()
            .find(|(_, registered)| **registered == st)
            .map(|(name, _)| name.clone())
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
        let entry = self.context.append_basic_block(thunk_fn, "entry");

        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

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
    pub fn emit_object_file(&self, path: &Path) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize native target: {e}"))?;

        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("failed to get target: {}", e.to_string()))?;

        let machine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::Default,
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
        if let Some(fields) = self.mono_struct_info.get(struct_name) {
            return fields
                .iter()
                .position(|(name, _)| name == field_name)
                .map(|i| i as u32);
        }
        self.type_ctx.types.get(struct_name).and_then(|info| {
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
        if let Some(fields) = self.mono_struct_info.get(struct_name) {
            return fields
                .iter()
                .find(|(name, _)| name == field_name)
                .map(|(_, ty)| ty.clone());
        }
        self.type_ctx.types.get(struct_name).and_then(|info| {
            info.fields().and_then(|fields| {
                fields
                    .iter()
                    .find(|(name, _)| name == field_name)
                    .map(|(_, ty)| ty.clone())
            })
        })
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

    /// Resolves an optional return type annotation to an Expo type, defaulting
    /// to `Unit` when absent.
    pub fn resolve_return_type(&self, return_type: &Option<TypeExpr>) -> Type {
        match return_type {
            Some(te) => self.resolve_type_expr(te),
            None => Type::Unit,
        }
    }

    /// Resolves a type expression AST node into an Expo type, using the
    /// currently registered struct and enum names for lookup.
    pub fn resolve_type_expr(&self, type_expr: &TypeExpr) -> Type {
        let struct_names: Vec<&str> = self
            .type_ctx
            .types
            .iter()
            .filter(|(_, ti)| ti.is_struct())
            .map(|(name, _)| name.as_str())
            .collect();
        let enum_names: Vec<&str> = self
            .type_ctx
            .types
            .iter()
            .filter(|(_, ti)| ti.is_enum())
            .map(|(name, _)| name.as_str())
            .collect();
        let type_params: Vec<&str> = self.type_subst.keys().map(|s| s.as_str()).collect();
        let ty = resolve_type_expr_with_params(
            type_expr,
            &struct_names,
            &enum_names,
            &type_params,
            &self.type_ctx.type_aliases,
        );
        substitute_preserving(&ty, &self.type_subst)
    }

    fn declare_builtins(&mut self) {
        let void = self.context.void_type();
        let i32 = self.context.i32_type();
        let i64 = self.context.i64_type();
        let ptr = self.context.ptr_type(inkwell::AddressSpace::default());

        let mut decl = |name: &str, ty: inkwell::types::FunctionType<'ctx>| {
            let f = self.module.add_function(name, ty, None);
            self.functions.insert(name.to_string(), f);
        };

        // C stdlib
        decl("printf", i32.fn_type(&[ptr.into()], true));
        decl(
            "snprintf",
            i32.fn_type(&[ptr.into(), i32.into(), ptr.into()], true),
        );
        decl("fprintf", i32.fn_type(&[ptr.into(), ptr.into()], true));
        decl("abort", void.fn_type(&[], false));
        decl("fdopen", ptr.fn_type(&[i32.into(), ptr.into()], false));
        decl("malloc", ptr.fn_type(&[i64.into()], false));
        decl("realloc", ptr.fn_type(&[ptr.into(), i64.into()], false));
        decl("free", void.fn_type(&[ptr.into()], false));
        decl("strcmp", i32.fn_type(&[ptr.into(), ptr.into()], false));
        decl("strlen", i64.fn_type(&[ptr.into()], false));
        decl(
            "memset",
            ptr.fn_type(&[ptr.into(), i32.into(), i64.into()], false),
        );
        decl(
            "memcpy",
            ptr.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
        );
        decl(
            "memcmp",
            i32.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
        );

        // Process runtime
        decl(
            "expo_rt_spawn",
            i64.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
        );
        decl(
            "expo_rt_send",
            void.fn_type(&[i64.into(), ptr.into(), i64.into()], false),
        );
        decl("expo_rt_receive", ptr.fn_type(&[], false));
        decl("expo_rt_receive_timeout", ptr.fn_type(&[i64.into()], false));
        decl("expo_rt_self", i64.fn_type(&[], false));
        decl("expo_rt_main_done", void.fn_type(&[], false));

        // String intrinsics
        decl(
            "expo_utf8_validate",
            i64.fn_type(&[ptr.into(), i64.into()], false),
        );
        decl("expo_string_length", i64.fn_type(&[ptr.into()], false));
        decl(
            "expo_string_get",
            ptr.fn_type(&[ptr.into(), i64.into()], false),
        );
        decl(
            "expo_string_slice",
            ptr.fn_type(&[ptr.into(), i64.into(), i64.into()], false),
        );
        decl(
            "expo_int_parse",
            i64.fn_type(&[ptr.into(), ptr.into()], false),
        );
        decl(
            "expo_float_parse",
            i64.fn_type(&[ptr.into(), ptr.into()], false),
        );

        // File I/O
        decl(
            "expo_fd_read",
            ptr.fn_type(&[i64.into(), i64.into()], false),
        );
        decl(
            "expo_fd_write",
            i64.fn_type(&[i64.into(), ptr.into()], false),
        );
        decl("expo_fd_close", i64.fn_type(&[i64.into()], false));
        decl("expo_last_error", ptr.fn_type(&[], false));
        decl(
            "expo_file_open",
            i64.fn_type(&[ptr.into(), i64.into()], false),
        );
        decl("expo_file_read_all", ptr.fn_type(&[ptr.into()], false));
    }

    fn declare_function(
        &self,
        func: &Function,
        self_type_name: Option<&str>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let mut return_type = self.resolve_return_type(&func.return_type);
        if let Some(name) = self_type_name
            && return_type == Type::Unknown
            && matches!(&func.return_type, Some(TypeExpr::Self_ { .. }))
        {
            if self.type_ctx.is_struct(name) {
                return_type = Type::Struct(name.to_string());
            } else if self.type_ctx.is_enum(name) {
                return_type = Type::Enum(name.to_string());
            }
        }
        let mut param_types = Vec::new();

        if let Some(name) = self_type_name
            && func
                .params
                .first()
                .is_some_and(|p| matches!(p, Param::Self_ { .. }))
        {
            if let Some(st) = self.struct_types.get(name) {
                param_types.push((*st).into());
            } else {
                let prim_ty = crate::types::primitive_name_to_type(name);
                if let Some(llvm_ty) = to_llvm_type(&prim_ty, self.context, &self.struct_types) {
                    param_types.push(llvm_ty.into());
                }
            }
        }

        param_types.extend(self.resolve_param_types(&func.params)?);

        let mangled = match self_type_name {
            Some(tn) => format!("{}_{}", tn, func.name),
            None => func.name.clone(),
        };

        let fn_type = if func.name == "main" && self_type_name.is_none() {
            self.context.i32_type().fn_type(&param_types, false)
        } else {
            match to_llvm_type(&return_type, self.context, &self.struct_types) {
                Some(ret_ty) => ret_ty.fn_type(&param_types, false),
                None => self.context.void_type().fn_type(&param_types, false),
            }
        };

        Ok(self.module.add_function(&mangled, fn_type, None))
    }

    fn declare_constants(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
            if let Item::Constant(c) = item {
                let val: BasicValueEnum = match &c.value {
                    Expr::Literal {
                        value: Literal::Int(s),
                        ..
                    } => {
                        let v = crate::util::parse_int_literal(s)?;
                        self.context.i64_type().const_int(v as u64, true).into()
                    }
                    Expr::Literal {
                        value: Literal::Float(s),
                        ..
                    } => {
                        let v: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
                        self.context.f64_type().const_float(v).into()
                    }
                    Expr::Literal {
                        value: Literal::Bool(b),
                        ..
                    } => self
                        .context
                        .bool_type()
                        .const_int(if *b { 1 } else { 0 }, false)
                        .into(),
                    Expr::String { parts, .. } => {
                        let mut combined = String::new();
                        for part in parts {
                            if let StringPart::Literal { value, .. } = part {
                                combined.push_str(value);
                            }
                        }
                        self.create_string_global(combined.as_bytes(), &c.name)
                            .into()
                    }
                    Expr::EnumConstruction {
                        type_path,
                        variant,
                        data: EnumConstructionData::Unit,
                        ..
                    } => {
                        let enum_name = type_path.join(".");
                        let Some(&enum_type) = self.struct_types.get(&enum_name) else {
                            continue;
                        };
                        let Some(tag) = self.get_variant_tag(&enum_name, variant) else {
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
                    Expr::StructConstruction {
                        type_path, fields, ..
                    } => {
                        let struct_name = type_path.join(".");
                        let Some(&struct_type) = self.struct_types.get(&struct_name) else {
                            continue;
                        };
                        let Some(info) = self.type_ctx.types.get(&struct_name) else {
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
            let val: BasicValueEnum = match &fi.value {
                Expr::Literal {
                    value: Literal::Int(s),
                    ..
                } => {
                    let v = crate::util::parse_int_literal(s).ok()?;
                    self.context.i64_type().const_int(v as u64, true).into()
                }
                Expr::Literal {
                    value: Literal::Float(s),
                    ..
                } => {
                    let v: f64 = s.parse().ok()?;
                    self.context.f64_type().const_float(v).into()
                }
                Expr::Literal {
                    value: Literal::Bool(b),
                    ..
                } => self
                    .context
                    .bool_type()
                    .const_int(if *b { 1 } else { 0 }, false)
                    .into(),
                Expr::String { parts, .. } => {
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

    fn declare_functions(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
            if let Item::Impl(impl_block) = item {
                for member in &impl_block.members {
                    if let ImplMember::Function(func) = member {
                        for param in &func.params {
                            if let Param::Regular { type_expr, .. } = param {
                                let ty = self.resolve_type_expr(type_expr);
                                let _ = self.ensure_types_exist(&ty);
                            }
                        }
                        if let Some(ret_te) = &func.return_type {
                            let ret_ty = self.resolve_type_expr(ret_te);
                            let _ = self.ensure_types_exist(&ret_ty);
                        }
                    }
                }
            }
        }

        for item in &module.items {
            match item {
                Item::Function(func) => {
                    if !func.type_params.is_empty() {
                        self.generic_fn_asts.insert(func.name.clone(), func.clone());
                        continue;
                    }
                    let fn_value = self.declare_function(func, None)?;
                    self.functions.insert(func.name.clone(), fn_value);
                }
                Item::Impl(impl_block) => {
                    let target_name = self.type_name_from_expr(&impl_block.target);
                    if let Some(target_name) = target_name {
                        for member in &impl_block.members {
                            if let ImplMember::Function(func) = member {
                                let mangled = format!("{}_{}", target_name, func.name);
                                let fn_value = self.declare_function(func, Some(&target_name))?;
                                self.functions.insert(mangled, fn_value);
                            }
                        }
                        if let Some(synth_fns) =
                            self.type_ctx.synthesized_default_fns.get(&target_name)
                        {
                            for func in synth_fns {
                                let mangled = format!("{}_{}", target_name, func.name);
                                let fn_value = self.declare_function(func, Some(&target_name))?;
                                self.functions.insert(mangled, fn_value);
                            }
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
        let mangled = match self_type_name {
            Some(tn) => format!("{}_{}", tn, func.name),
            None => func.name.clone(),
        };

        if crate::hashtable::is_primitive_intrinsic(&mangled) {
            return crate::hashtable::emit_primitive_intrinsic(self, &mangled);
        }

        let fn_value = *self
            .functions
            .get(&mangled)
            .ok_or_else(|| format!("undeclared function: {}", mangled))?;

        let is_main = func.name == "main" && self_type_name.is_none();

        if is_main {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let user_main_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
            let user_main = self
                .module
                .add_function("__expo_user_main", user_main_ty, None);
            self.functions
                .insert("__expo_user_main".to_string(), user_main);
            let um_entry = self.context.append_basic_block(user_main, "entry");
            self.builder.position_at_end(um_entry);
            self.variables.clear();
            self.compile_function_body(&func.body, &Type::Unit, user_main, false)?;

            let main_entry = self.context.append_basic_block(fn_value, "entry");
            self.builder.position_at_end(main_entry);

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

        let mut return_type = self.resolve_return_type(&func.return_type);
        if let Some(target) = self_type_name
            && return_type == Type::Unknown
            && matches!(&func.return_type, Some(TypeExpr::Self_ { .. }))
        {
            if self.type_ctx.is_struct(target) {
                return_type = Type::Struct(target.to_string());
            } else if self.type_ctx.is_enum(target) {
                return_type = Type::Enum(target.to_string());
            }
        }

        let self_type = self_type_name.map(|n| (n, n));
        self.compile_method_body(
            fn_value,
            func,
            self_type,
            &param_types,
            &return_type,
            HashMap::new(),
        )
    }

    fn define_functions(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
            match item {
                Item::Function(func) => {
                    if !func.type_params.is_empty() {
                        continue;
                    }
                    self.define_function(func, None)?;
                }
                Item::Impl(impl_block) => {
                    let target_name = self.type_name_from_expr(&impl_block.target);
                    if let Some(target_name) = target_name {
                        for member in &impl_block.members {
                            if let ImplMember::Function(func) = member {
                                self.define_function(func, Some(&target_name))?;
                            }
                        }
                        if let Some(synth_fns) =
                            self.type_ctx.synthesized_default_fns.get(&target_name)
                        {
                            for func in synth_fns {
                                self.define_function(func, Some(&target_name))?;
                            }
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
                if let Some(llvm_ty) = to_llvm_metadata_type(&ty, self.context, &self.struct_types)
                {
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

    /// Emits a panic sequence: writes a formatted message to stderr via
    /// `fdopen(2,"w")` + `fprintf`, then calls `abort` and marks the
    /// insertion point as unreachable.
    ///
    /// `fmt` is a printf-style format string (e.g. `"panic: %s\n"`).
    /// `args` are the values to interpolate into the format string.
    pub fn emit_panic(&self, fmt: &str, args: &[BasicValueEnum<'ctx>]) {
        let fdopen = *self.functions.get("fdopen").expect("fdopen not declared");
        let fprintf = *self.functions.get("fprintf").expect("fprintf not declared");
        let abort = *self.functions.get("abort").expect("abort not declared");

        let fd_val = self.context.i32_type().const_int(2, false);
        let mode = self
            .builder
            .build_global_string_ptr("w", "panic_mode")
            .unwrap();
        let stderr = self
            .call(
                fdopen,
                &[fd_val.into(), mode.as_pointer_value().into()],
                "panic_stderr",
            )
            .expect("fdopen returned no value");

        let fmt_ptr = self
            .builder
            .build_global_string_ptr(fmt, "panic_fmt")
            .unwrap();

        let mut fprintf_args: Vec<inkwell::values::BasicMetadataValueEnum> =
            vec![stderr.into(), fmt_ptr.as_pointer_value().into()];
        for arg in args {
            fprintf_args.push((*arg).into());
        }
        self.call_void(fprintf, &fprintf_args, "panic_fprintf");

        self.call_void(abort, &[], "panic_abort");
        self.builder.build_unreachable().unwrap();
    }
}

/// Compiles a single Expo module to a native object file.
pub fn compile(
    module: &Module,
    type_ctx: &TypeContext,
    output_path: &Path,
) -> Result<(), Vec<Diagnostic>> {
    compile_modules(&[module], type_ctx, output_path)
}

/// Runs codegen for all modules: register types, declare, define.
fn run_codegen<'ctx>(
    modules: &[&Module],
    type_ctx: &'ctx TypeContext,
    context: &'ctx Context,
) -> Result<Compiler<'ctx>, Vec<Diagnostic>> {
    let mut compiler = Compiler::new(context, type_ctx);

    compiler.register_types();
    compiler.declare_builtins();

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

    Ok(compiler)
}

/// Compiles multiple Expo modules into a single native object file. Registers
/// types, declares all functions across modules, then defines their bodies.
pub fn compile_modules(
    modules: &[&Module],
    type_ctx: &TypeContext,
    output_path: &Path,
) -> Result<(), Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(modules, type_ctx, &context)?;

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
    compiler.emit_object_file(output_path).map_err(|e| {
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
) -> Result<String, Vec<Diagnostic>> {
    let context = Context::create();
    let compiler = run_codegen(modules, type_ctx, &context)?;
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
        let ti = c.type_ctx.types.get(&base)?;
        let subst = build_substitution(&ti.type_params, &type_args);
        let m = substitute(proto_args.get(1)?, &subst);
        let r = substitute(proto_args.get(2)?, &subst);
        return Some(process_envelope_type(&m, &r));
    }
    None
}
