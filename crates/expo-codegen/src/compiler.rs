//! Compilation driver: holds all LLVM state, registers types, declares and
//! defines functions, and orchestrates emission of native object files.

use std::collections::HashMap;
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
    Diagnostic, Expr, Function, ImplMember, Item, Literal, Module, Param, Severity, TypeExpr,
};
use expo_typecheck::context::{TypeContext, VariantData};
use expo_typecheck::types::{Type, mangle_type};
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

/// Holds all LLVM state needed to compile an Expo module: the LLVM context,
/// module, builder, declared functions, variable bindings, and type mappings.
pub struct Compiler<'ctx> {
    pub context: &'ctx Context,
    pub module: LlvmModule<'ctx>,
    pub builder: Builder<'ctx>,
    pub constants: HashMap<String, BasicValueEnum<'ctx>>,
    pub functions: HashMap<String, FunctionValue<'ctx>>,
    pub variables: HashMap<String, (PointerValue<'ctx>, Type, Ownership)>,
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
    /// Maps process function names to their message type, populated from
    /// `TypeContext::process_fn_msg_types` so `receive` codegen can determine
    /// the LLVM type to load from the mailbox pointer.
    pub process_msg_type: Option<Type>,
    /// Cache of generated thunk wrappers for bare function references.
    /// Maps original function name to the thunk `FunctionValue`.
    pub fn_ref_thunks: HashMap<String, FunctionValue<'ctx>>,
    /// Return type of the function currently being compiled. Used by
    /// generic enum construction to resolve unit variant type args when
    /// they can't be inferred from arguments (e.g. `Option.None` inside
    /// a method with its own type parameters like `map<U>`).
    pub return_type_hint: Option<Type>,
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
            variables: HashMap::new(),
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
        }
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
            thunk_params.push(target_ty.get_param_types()[i as usize].into());
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

        let call_val = self
            .builder
            .build_call(target_fn, &forward_args, "fwd")
            .unwrap();

        match call_val.try_as_basic_value().left() {
            Some(ret) => self.builder.build_return(Some(&ret)).unwrap(),
            None => self.builder.build_return(None).unwrap(),
        };

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        self.fn_ref_thunks.insert(fn_name.to_string(), thunk_fn);
        Ok(thunk_fn)
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
        self.type_ctx.structs.get(struct_name).and_then(|info| {
            info.fields
                .iter()
                .position(|(name, _)| name == field_name)
                .map(|i| i as u32)
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
        self.type_ctx.structs.get(struct_name).and_then(|info| {
            info.fields
                .iter()
                .find(|(name, _)| name == field_name)
                .map(|(_, ty)| ty.clone())
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
        let struct_names: Vec<&str> = self.type_ctx.structs.keys().map(|s| s.as_str()).collect();
        let enum_names: Vec<&str> = self.type_ctx.enums.keys().map(|s| s.as_str()).collect();
        let type_params: Vec<&str> = self.type_subst.keys().map(|s| s.as_str()).collect();
        expo_typecheck::types::resolve_type_expr_with_params(
            type_expr,
            &struct_names,
            &enum_names,
            &type_params,
            &self.type_ctx.type_aliases,
        )
    }

    fn declare_builtins(&mut self) {
        let i32_type = self.context.i32_type();
        let i8_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());

        let printf_type = i32_type.fn_type(&[i8_ptr_type.into()], true);
        let printf = self.module.add_function("printf", printf_type, None);
        self.functions.insert("printf".to_string(), printf);

        let snprintf_type = i32_type.fn_type(
            &[i8_ptr_type.into(), i32_type.into(), i8_ptr_type.into()],
            true,
        );
        let snprintf = self.module.add_function("snprintf", snprintf_type, None);
        self.functions.insert("snprintf".to_string(), snprintf);

        let fprintf_type = i32_type.fn_type(&[i8_ptr_type.into(), i8_ptr_type.into()], true);
        let fprintf = self.module.add_function("fprintf", fprintf_type, None);
        self.functions.insert("fprintf".to_string(), fprintf);

        let abort_type = self.context.void_type().fn_type(&[], false);
        let abort = self.module.add_function("abort", abort_type, None);
        self.functions.insert("abort".to_string(), abort);

        let fdopen_type = i8_ptr_type.fn_type(&[i32_type.into(), i8_ptr_type.into()], false);
        let fdopen = self.module.add_function("fdopen", fdopen_type, None);
        self.functions.insert("fdopen".to_string(), fdopen);

        let i64_type = self.context.i64_type();

        let malloc_type = i8_ptr_type.fn_type(&[i64_type.into()], false);
        let malloc = self.module.add_function("malloc", malloc_type, None);
        self.functions.insert("malloc".to_string(), malloc);

        let realloc_type = i8_ptr_type.fn_type(&[i8_ptr_type.into(), i64_type.into()], false);
        let realloc = self.module.add_function("realloc", realloc_type, None);
        self.functions.insert("realloc".to_string(), realloc);

        let free_type = self
            .context
            .void_type()
            .fn_type(&[i8_ptr_type.into()], false);
        let free = self.module.add_function("free", free_type, None);
        self.functions.insert("free".to_string(), free);

        let strcmp_type = i32_type.fn_type(&[i8_ptr_type.into(), i8_ptr_type.into()], false);
        let strcmp = self.module.add_function("strcmp", strcmp_type, None);
        self.functions.insert("strcmp".to_string(), strcmp);

        let strlen_type = i64_type.fn_type(&[i8_ptr_type.into()], false);
        let strlen = self.module.add_function("strlen", strlen_type, None);
        self.functions.insert("strlen".to_string(), strlen);

        let memset_type = i8_ptr_type.fn_type(
            &[i8_ptr_type.into(), i32_type.into(), i64_type.into()],
            false,
        );
        let memset = self.module.add_function("memset", memset_type, None);
        self.functions.insert("memset".to_string(), memset);

        let memcpy_type = i8_ptr_type.fn_type(
            &[i8_ptr_type.into(), i8_ptr_type.into(), i64_type.into()],
            false,
        );
        let memcpy = self.module.add_function("memcpy", memcpy_type, None);
        self.functions.insert("memcpy".to_string(), memcpy);

        let spawn_type = i64_type.fn_type(&[i8_ptr_type.into()], false);
        let spawn = self.module.add_function("expo_rt_spawn", spawn_type, None);
        self.functions.insert("expo_rt_spawn".to_string(), spawn);

        let send_type = self.context.void_type().fn_type(
            &[i64_type.into(), i8_ptr_type.into(), i64_type.into()],
            false,
        );
        let send = self.module.add_function("expo_rt_send", send_type, None);
        self.functions.insert("expo_rt_send".to_string(), send);

        let receive_type = i8_ptr_type.fn_type(&[], false);
        let receive = self
            .module
            .add_function("expo_rt_receive", receive_type, None);
        self.functions
            .insert("expo_rt_receive".to_string(), receive);

        let main_done_type = self.context.void_type().fn_type(&[], false);
        let main_done = self
            .module
            .add_function("expo_rt_main_done", main_done_type, None);
        self.functions
            .insert("expo_rt_main_done".to_string(), main_done);
    }

    fn declare_function(
        &self,
        func: &Function,
        self_type_name: Option<&str>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let return_type = self.resolve_return_type(&func.return_type);
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
                            if let expo_ast::ast::StringPart::Literal { value, .. } = part {
                                combined.push_str(value);
                            }
                        }
                        let bytes = combined.as_bytes();
                        let str_type = self.context.i8_type().array_type((bytes.len() + 1) as u32);
                        let global = self.module.add_global(str_type, None, &c.name);
                        global.set_initializer(&self.context.const_string(bytes, true));
                        global.set_constant(true);
                        global.as_pointer_value().into()
                    }
                    _ => continue,
                };
                self.constants.insert(c.name.clone(), val);
            }
        }
        Ok(())
    }

    fn declare_functions(&mut self, module: &Module) -> Result<(), String> {
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

        let entry = self.context.append_basic_block(fn_value, "entry");
        self.builder.position_at_end(entry);

        self.variables.clear();

        let mut llvm_param_idx: u32 = 0;

        if let Some(type_name) = self_type_name
            && func
                .params
                .first()
                .is_some_and(|p| matches!(p, Param::Self_ { .. }))
        {
            let self_ty = Type::Struct(type_name.to_string());
            if let Some(llvm_ty) = to_llvm_type(&self_ty, self.context, &self.struct_types) {
                let alloca = self.builder.build_alloca(llvm_ty, "self").unwrap();
                let param_val = fn_value.get_nth_param(llvm_param_idx).unwrap();
                self.builder.build_store(alloca, param_val).unwrap();
                self.variables
                    .insert("self".to_string(), (alloca, self_ty, Ownership::Unowned));
                llvm_param_idx += 1;
            }
        }

        for param in func.params.iter() {
            if let Param::Regular {
                name, type_expr, ..
            } = param
            {
                let param_ty = self.resolve_type_expr(type_expr);
                if let Some(llvm_ty) = to_llvm_type(&param_ty, self.context, &self.struct_types) {
                    let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
                    let param_val = fn_value.get_nth_param(llvm_param_idx).unwrap();
                    self.builder.build_store(alloca, param_val).unwrap();
                    self.variables
                        .insert(name.clone(), (alloca, param_ty, Ownership::Unowned));
                    llvm_param_idx += 1;
                }
            }
        }

        let saved_process_msg = self.process_msg_type.take();
        if self_type_name.is_none() {
            self.process_msg_type = self.type_ctx.process_fn_msg_types.get(&func.name).cloned();
        }

        let return_type = self.resolve_return_type(&func.return_type);
        let is_main = func.name == "main" && self_type_name.is_none();
        let result = self.compile_function_body(&func.body, &return_type, fn_value, is_main);

        self.process_msg_type = saved_process_msg;
        result
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
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Translates Expo type-checked structs and enums into LLVM struct types.
    /// Uses a two-pass approach (opaque types first, then bodies) so
    /// cross-referencing types resolve correctly.
    fn register_types(&mut self) {
        // Pass 1: create opaque types so cross-references resolve
        for (name, info) in &self.type_ctx.structs {
            if !info.type_params.is_empty() {
                continue;
            }
            let st = self.context.opaque_struct_type(name);
            self.struct_types.insert(name.clone(), st);
        }
        for (name, info) in &self.type_ctx.enums {
            if !info.type_params.is_empty() {
                continue;
            }
            let et = self.context.opaque_struct_type(name);
            self.struct_types.insert(name.clone(), et);
        }

        // Pass 2: set struct bodies (skip generic templates)
        for (name, info) in &self.type_ctx.structs {
            if !info.type_params.is_empty() {
                continue;
            }
            let struct_type = *self.struct_types.get(name).unwrap();
            let field_types: Vec<_> = info
                .fields
                .iter()
                .filter_map(|(_, ty)| to_llvm_type(ty, self.context, &self.struct_types))
                .collect();
            struct_type.set_body(&field_types, false);
        }

        // Pass 3: set enum bodies (skip generic templates)
        for (name, info) in &self.type_ctx.enums {
            if !info.type_params.is_empty() {
                continue;
            }
            let mut variant_payloads = Vec::new();
            let mut max_payload_size: u32 = 0;

            for variant in &info.variants {
                match &variant.data {
                    VariantData::Unit => {
                        variant_payloads.push((variant.name.clone(), None));
                    }
                    VariantData::Tuple(types) => {
                        let field_llvm: Vec<_> = types
                            .iter()
                            .filter_map(|ty| to_llvm_type(ty, self.context, &self.struct_types))
                            .collect();
                        let payload = self.context.struct_type(&field_llvm, true);
                        let size: u32 = types.iter().map(type_byte_size).sum();
                        max_payload_size = max_payload_size.max(size);
                        variant_payloads.push((variant.name.clone(), Some(payload)));
                    }
                    VariantData::Struct(fields) => {
                        let field_llvm: Vec<_> = fields
                            .iter()
                            .filter_map(|(_, ty)| {
                                to_llvm_type(ty, self.context, &self.struct_types)
                            })
                            .collect();
                        let payload = self.context.struct_type(&field_llvm, true);
                        let size: u32 = fields.iter().map(|(_, ty)| type_byte_size(ty)).sum();
                        max_payload_size = max_payload_size.max(size);
                        variant_payloads.push((variant.name.clone(), Some(payload)));
                    }
                }
            }

            let enum_type = *self.struct_types.get(name).unwrap();
            let i8_type = self.context.i8_type();
            if max_payload_size > 0 {
                let payload_array = i8_type.array_type(max_payload_size);
                enum_type.set_body(&[i8_type.into(), payload_array.into()], false);
            } else {
                enum_type.set_body(&[i8_type.into()], false);
            }

            self.enum_variant_payloads
                .insert(name.clone(), variant_payloads);

            let ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
            let name_ptrs: Vec<_> = info
                .variants
                .iter()
                .map(|v| {
                    let bytes = self.context.const_string(v.name.as_bytes(), true);
                    let g = self.module.add_global(
                        bytes.get_type(),
                        None,
                        &format!("{name}_{}_name", v.name),
                    );
                    g.set_initializer(&bytes);
                    g.set_constant(true);
                    g.as_pointer_value()
                })
                .collect();
            let table_init = ptr_type.const_array(&name_ptrs);
            let table_global = self.module.add_global(
                table_init.get_type(),
                None,
                &format!("{name}_variant_names"),
            );
            table_global.set_initializer(&table_init);
            table_global.set_constant(true);
            self.enum_name_tables
                .insert(name.clone(), table_global.as_pointer_value());
        }

        // Pass 4: register union types (tagged-union layout reusing enum infrastructure)
        let mut union_types: Vec<Type> = Vec::new();
        for ty in self.type_ctx.type_aliases.values() {
            collect_union_types(ty, &mut union_types);
        }
        for sig in self.type_ctx.functions.values() {
            collect_union_types(&sig.return_type, &mut union_types);
            for p in &sig.params {
                collect_union_types(&p.ty, &mut union_types);
            }
        }
        for info in self.type_ctx.structs.values() {
            for (_, ty) in &info.fields {
                collect_union_types(ty, &mut union_types);
            }
        }

        let i8_type = self.context.i8_type();
        for union_ty in &union_types {
            let Type::Union(members) = union_ty else {
                continue;
            };
            let mangled = mangle_type(union_ty);
            if self.struct_types.contains_key(&mangled) {
                continue;
            }

            let opaque = self.context.opaque_struct_type(&mangled);
            self.struct_types.insert(mangled.clone(), opaque);

            let mut variant_payloads = Vec::new();
            let mut max_payload_size: u32 = 0;

            for member in members {
                let member_name = mangle_type(member);
                if let Some(llvm_ty) = to_llvm_type(member, self.context, &self.struct_types) {
                    let payload = self.context.struct_type(&[llvm_ty], true);
                    let size = type_byte_size(member);
                    max_payload_size = max_payload_size.max(size);
                    variant_payloads.push((member_name, Some(payload)));
                } else {
                    variant_payloads.push((member_name, None));
                }
            }

            if max_payload_size > 0 {
                let payload_array = i8_type.array_type(max_payload_size);
                opaque.set_body(&[i8_type.into(), payload_array.into()], false);
            } else {
                opaque.set_body(&[i8_type.into()], false);
            }

            self.enum_variant_payloads.insert(mangled, variant_payloads);
        }
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
            .builder
            .build_call(
                fdopen,
                &[fd_val.into(), mode.as_pointer_value().into()],
                "panic_stderr",
            )
            .unwrap()
            .try_as_basic_value()
            .left()
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
        self.builder
            .build_call(fprintf, &fprintf_args, "panic_fprintf")
            .unwrap();

        self.builder.build_call(abort, &[], "panic_abort").unwrap();
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

/// Compiles multiple Expo modules into a single native object file. Registers
/// types, declares all functions across modules, then defines their bodies.
pub fn compile_modules(
    modules: &[&Module],
    type_ctx: &TypeContext,
    output_path: &Path,
) -> Result<(), Vec<Diagnostic>> {
    let context = Context::create();
    let mut compiler = Compiler::new(&context, type_ctx);

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

pub(crate) fn type_byte_size(ty: &Type) -> u32 {
    use expo_typecheck::types::Primitive;
    match ty {
        Type::Primitive(p) => match p {
            Primitive::Bool | Primitive::I8 | Primitive::U8 => 1,
            Primitive::I16 | Primitive::U16 => 2,
            Primitive::I32 | Primitive::U32 | Primitive::F32 => 4,
            Primitive::I64 | Primitive::U64 | Primitive::F64 | Primitive::String => 8,
        },
        Type::Function { .. } => 16,
        _ => 8,
    }
}

/// Recursively collects all `Type::Union` variants reachable from `ty`.
fn collect_union_types(ty: &Type, out: &mut Vec<Type>) {
    match ty {
        Type::Union(members) => {
            out.push(ty.clone());
            for m in members {
                collect_union_types(m, out);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for p in params {
                collect_union_types(p, out);
            }
            collect_union_types(return_type, out);
        }
        Type::GenericInstance { type_args, .. } => {
            for ta in type_args {
                collect_union_types(ta, out);
            }
        }
        Type::Tuple(elems) => {
            for e in elems {
                collect_union_types(e, out);
            }
        }
        _ => {}
    }
}
