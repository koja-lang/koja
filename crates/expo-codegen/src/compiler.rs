use std::collections::HashMap;
use std::path::Path;

use expo_ast::ast::{Diagnostic, Function, ImplMember, Item, Module, Param, Severity, TypeExpr};
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::Type;
use inkwell::OptimizationLevel;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module as LlvmModule;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, StructType};
use inkwell::values::{FunctionValue, PointerValue};

use crate::expr::compile_expr;
use crate::stmt::compile_statement;
use crate::types::{to_llvm_metadata_type, to_llvm_type};

pub struct Compiler<'ctx> {
    pub context: &'ctx Context,
    pub module: LlvmModule<'ctx>,
    pub builder: Builder<'ctx>,
    pub functions: HashMap<String, FunctionValue<'ctx>>,
    pub variables: HashMap<String, (PointerValue<'ctx>, Type)>,
    pub struct_types: HashMap<String, StructType<'ctx>>,
    pub loop_exit_stack: Vec<BasicBlock<'ctx>>,
    pub type_ctx: &'ctx TypeContext,
}

impl<'ctx> Compiler<'ctx> {
    pub fn new(context: &'ctx Context, type_ctx: &'ctx TypeContext) -> Self {
        let module = context.create_module("expo_module");
        let builder = context.create_builder();
        Self {
            context,
            module,
            builder,
            functions: HashMap::new(),
            variables: HashMap::new(),
            struct_types: HashMap::new(),
            loop_exit_stack: Vec::new(),
            type_ctx,
        }
    }

    pub fn compile_module(&mut self, module: &Module) -> Result<(), String> {
        self.register_struct_types();
        self.declare_builtins();
        self.declare_functions(module)?;
        self.define_functions(module)?;
        self.module
            .verify()
            .map_err(|e| format!("LLVM verification failed: {}", e.to_string()))
    }

    fn register_struct_types(&mut self) {
        for (name, info) in &self.type_ctx.structs {
            let struct_type = self.context.opaque_struct_type(name);
            let field_types: Vec<_> = info
                .fields
                .iter()
                .filter_map(|(_, ty)| to_llvm_type(ty, self.context, &self.struct_types))
                .collect();
            struct_type.set_body(&field_types, false);
            self.struct_types.insert(name.clone(), struct_type);
        }
    }

    fn declare_builtins(&mut self) {
        let i32_type = self.context.i32_type();
        let i8_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());

        let printf_type = i32_type.fn_type(&[i8_ptr_type.into()], true);
        let printf = self.module.add_function("printf", printf_type, None);
        self.functions.insert("printf".to_string(), printf);
    }

    fn declare_functions(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
            match item {
                Item::Function(func) => {
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
            && let Some(st) = self.struct_types.get(name)
        {
            param_types.push((*st).into());
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

    fn define_functions(&mut self, module: &Module) -> Result<(), String> {
        for item in &module.items {
            match item {
                Item::Function(func) => {
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

    fn type_name_from_expr(&self, te: &TypeExpr) -> Option<String> {
        if let TypeExpr::Named { path, .. } = te
            && path.len() == 1
        {
            return Some(path[0].clone());
        }
        None
    }

    fn define_function(
        &mut self,
        func: &Function,
        self_type_name: Option<&str>,
    ) -> Result<(), String> {
        let mangled = match self_type_name {
            Some(tn) => format!("{}_{}", tn, func.name),
            None => func.name.clone(),
        };

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
                self.variables.insert("self".to_string(), (alloca, self_ty));
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
                    self.variables.insert(name.clone(), (alloca, param_ty));
                    llvm_param_idx += 1;
                }
            }
        }

        let return_type = self.resolve_return_type(&func.return_type);
        let body_len = func.body.len();

        for (i, stmt) in func.body.iter().enumerate() {
            let is_last = i == body_len - 1;

            if self.current_block_terminated() {
                break;
            }

            if is_last
                && return_type != Type::Unit
                && let expo_ast::ast::Statement::Expr(expr) = stmt
            {
                let val = compile_expr(self, expr, fn_value)?;
                if !self.current_block_terminated() {
                    if let Some(v) = val {
                        self.builder.build_return(Some(&v)).unwrap();
                    } else {
                        self.builder.build_return(None).unwrap();
                    }
                }
                continue;
            }

            compile_statement(self, stmt, fn_value)?;
        }

        if !self.current_block_terminated() {
            if func.name == "main" && self_type_name.is_none() {
                let zero = self.context.i32_type().const_int(0, false);
                self.builder.build_return(Some(&zero)).unwrap();
            } else {
                self.builder.build_return(None).unwrap();
            }
        }

        Ok(())
    }

    pub fn current_block_terminated(&self) -> bool {
        self.builder
            .get_insert_block()
            .map(|bb| bb.get_terminator().is_some())
            .unwrap_or(true)
    }

    pub fn resolve_return_type(&self, return_type: &Option<TypeExpr>) -> Type {
        match return_type {
            Some(te) => self.resolve_type_expr(te),
            None => Type::Unit,
        }
    }

    pub fn resolve_type_expr(&self, type_expr: &TypeExpr) -> Type {
        let struct_names: Vec<&str> = self.type_ctx.structs.keys().map(|s| s.as_str()).collect();
        let enum_names: Vec<&str> = self.type_ctx.enums.keys().map(|s| s.as_str()).collect();
        expo_typecheck::types::resolve_type_expr(type_expr, &struct_names, &enum_names)
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

    pub fn get_field_index(&self, struct_name: &str, field_name: &str) -> Option<u32> {
        self.type_ctx.structs.get(struct_name).and_then(|info| {
            info.fields
                .iter()
                .position(|(name, _)| name == field_name)
                .map(|i| i as u32)
        })
    }

    pub fn get_field_type(&self, struct_name: &str, field_name: &str) -> Option<Type> {
        self.type_ctx.structs.get(struct_name).and_then(|info| {
            info.fields
                .iter()
                .find(|(name, _)| name == field_name)
                .map(|(_, ty)| ty.clone())
        })
    }
}

pub fn compile(
    module: &Module,
    type_ctx: &TypeContext,
    output_path: &Path,
) -> Result<(), Vec<Diagnostic>> {
    let context = Context::create();
    let mut compiler = Compiler::new(&context, type_ctx);
    compiler.compile_module(module).map_err(|e| {
        vec![Diagnostic {
            severity: Severity::Error,
            message: e,
            hint: None,
            span: module.span,
        }]
    })?;
    compiler.emit_object_file(output_path).map_err(|e| {
        vec![Diagnostic {
            severity: Severity::Error,
            message: e,
            hint: None,
            span: module.span,
        }]
    })
}
