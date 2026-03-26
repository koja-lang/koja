//! Monomorphization engine: specializes generic functions, structs, and enums
//! for concrete type arguments, and manages the mangled-name encoding used to
//! distinguish each instantiation.

use std::collections::HashMap;
use std::mem;

use expo_ast::ast::{Function, ImplMember, Param, Statement, TypeExpr};
use expo_typecheck::context::{FunctionKind, VariantData};
use expo_typecheck::types::{
    GenericKind, Primitive, Type, build_substitution, mangle_name, mangle_type, substitute,
};
use inkwell::types::BasicType;
use inkwell::values::{FunctionValue, PointerValue};

use crate::drop::Ownership;

use crate::compiler::{Compiler, resolve_process_envelope_type};
use crate::expr::compile_expr;
use crate::stmt::{apply_coercion, compile_statement};
use crate::types::to_llvm_type;

impl<'ctx> Compiler<'ctx> {
    /// Compiles a function body: iterates statements, handles implicit return
    /// of the last expression, and inserts a terminator if missing. When
    /// `is_main` is true, a missing terminator returns `0` instead of void.
    pub(crate) fn compile_function_body(
        &mut self,
        body: &[Statement],
        return_type: &Type,
        fn_value: FunctionValue<'ctx>,
        _is_main: bool,
    ) -> Result<(), String> {
        let saved_hint = mem::replace(
            &mut self.return_type_hint,
            if *return_type != Type::Unit {
                Some(return_type.clone())
            } else {
                None
            },
        );

        let body_len = body.len();

        for (i, stmt) in body.iter().enumerate() {
            let is_last = i == body_len - 1;

            if self.current_block_terminated() {
                break;
            }

            if is_last
                && *return_type != Type::Unit
                && let Statement::Expr(expr) = stmt
            {
                self.tco.mark_tail();
                let val = compile_expr(self, expr, fn_value)?.map(|tv| tv.value);
                self.tco.clear_tail();
                if !self.current_block_terminated() {
                    if let Some(v) = val {
                        let v = apply_coercion(self, v, expr)?;
                        self.builder.build_return(Some(&v)).unwrap();
                    } else {
                        self.builder.build_unreachable().unwrap();
                    }
                }
                continue;
            }

            compile_statement(self, stmt, fn_value)?;
        }

        if !self.current_block_terminated() {
            if *return_type == Type::Unit {
                crate::drop::drop_live_variables(self, None);
                self.builder.build_return(None).unwrap();
            } else {
                self.builder.build_unreachable().unwrap();
            }
        }

        self.return_type_hint = saved_hint;
        Ok(())
    }

    /// Shared compilation kernel for method bodies: saves/restores compiler
    /// state, binds `self` and regular parameters, sets up process message
    /// types, then compiles the function body. Used by both `define_function`
    /// (non-generic methods) and `monomorphize_impl_method` (generic methods).
    ///
    /// `self_type` is `Some((mangled, base))` for instance/static methods.
    /// `mangled` is the LLVM-registered type name (e.g. `List_$Token$`).
    /// `base` is the unmangled name for `is_enum`/`is_struct` lookups.
    /// For non-generic methods both are identical.
    pub(crate) fn compile_method_body(
        &mut self,
        fn_value: FunctionValue<'ctx>,
        func: &Function,
        self_type: Option<(&str, &str)>,
        param_types: &[Type],
        return_type: &Type,
        subst: HashMap<String, Type>,
    ) -> Result<(), String> {
        let entry = self.context.append_basic_block(fn_value, "entry");
        let saved_vars = mem::take(&mut self.variables);
        let saved_block = self.builder.get_insert_block();
        let saved_subst = mem::replace(&mut self.type_subst, subst);

        self.builder.position_at_end(entry);

        let mut param_idx = 0u32;
        let mut param_allocas: Vec<PointerValue<'ctx>> = Vec::new();

        if func
            .params
            .first()
            .is_some_and(|p| matches!(p, Param::Self_ { .. }))
            && let Some((mangled, base)) = self_type
        {
            let self_ty = if let Some(p) = Primitive::from_name(base) {
                Type::Primitive(p)
            } else if self.type_ctx.is_enum(base) {
                Type::Enum(mangled.to_string())
            } else {
                Type::Struct(mangled.to_string())
            };
            if let Some(llvm_ty) = to_llvm_type(&self_ty, self.context, &self.struct_types) {
                let alloca = self.builder.build_alloca(llvm_ty, "self").unwrap();
                let param_val = fn_value.get_nth_param(param_idx).unwrap();
                self.builder.build_store(alloca, param_val).unwrap();
                self.variables
                    .insert("self".to_string(), (alloca, self_ty, Ownership::Unowned));
                param_allocas.push(alloca);
                param_idx += 1;
            }
        }

        let mut type_idx = 0usize;
        for param in func.params.iter() {
            if let Param::Regular { name: pname, .. } = param
                && type_idx < param_types.len()
            {
                let ty = &param_types[type_idx];
                type_idx += 1;
                if let Some(llvm_ty) = to_llvm_type(ty, self.context, &self.struct_types) {
                    let alloca = self.builder.build_alloca(llvm_ty, pname).unwrap();
                    let param_val = fn_value.get_nth_param(param_idx).unwrap();
                    self.builder.build_store(alloca, param_val).unwrap();
                    self.variables
                        .insert(pname.clone(), (alloca, ty.clone(), Ownership::Unowned));
                    param_allocas.push(alloca);
                    param_idx += 1;
                }
            }
        }

        let loop_header = self.context.append_basic_block(fn_value, "tco_loop");
        self.builder
            .build_unconditional_branch(loop_header)
            .unwrap();
        self.builder.position_at_end(loop_header);

        let saved_process_msg = self.process_msg_type.take();
        if let Some((mangled, _)) = self_type {
            self.process_msg_type = resolve_process_envelope_type(self, mangled);
            if let Some(env_type) = self.process_msg_type.clone() {
                let _ = self.ensure_types_exist(&env_type);
            }
        }

        let saved_fn = self
            .tco
            .enter_fn(fn_value.get_name().to_str().unwrap_or("").to_string());
        let saved_loop = self.tco.set_loop(loop_header, param_allocas);

        let result = self.compile_function_body(&func.body, return_type, fn_value, false);

        self.tco.leave_fn(saved_fn);
        self.tco.restore_loop(saved_loop);
        self.process_msg_type = saved_process_msg;
        self.variables = saved_vars;
        self.type_subst = saved_subst;
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        result
    }

    /// Generates a monomorphized (specialized) version of a generic function for
    /// the given concrete type arguments. Declares the LLVM function, compiles its
    /// body with type variables substituted, and registers it under the mangled name.
    pub fn monomorphize_function(&mut self, name: &str, type_args: &[Type]) -> Result<(), String> {
        let func_ast = self
            .generic_fn_asts
            .get(name)
            .ok_or_else(|| format!("no generic function `{name}` to monomorphize"))?
            .clone();

        let mangled = mangle_name(name, type_args);
        if self.functions.contains_key(&mangled) {
            return Ok(());
        }

        let sig = self
            .type_ctx
            .functions
            .get(name)
            .ok_or_else(|| format!("no signature for generic function `{name}`"))?;

        let subst = build_substitution(&sig.type_params, type_args);

        let return_type = substitute(&sig.return_type, &subst);

        let param_types: Vec<Type> = sig
            .params
            .iter()
            .map(|p| substitute(&p.ty, &subst))
            .collect();

        self.ensure_types_exist(&return_type)?;
        for pt in &param_types {
            self.ensure_types_exist(pt)?;
        }

        let llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = param_types
            .iter()
            .filter_map(|ty| to_llvm_type(ty, self.context, &self.struct_types))
            .map(|t| t.into())
            .collect();

        let fn_type = match to_llvm_type(&return_type, self.context, &self.struct_types) {
            Some(ret) => ret.fn_type(&llvm_param_types, false),
            None => self.context.void_type().fn_type(&llvm_param_types, false),
        };

        let fn_value = self.module.add_function(&mangled, fn_type, None);
        self.functions.insert(mangled.clone(), fn_value);

        let entry = self.context.append_basic_block(fn_value, "entry");
        let saved_vars = mem::take(&mut self.variables);
        let saved_block = self.builder.get_insert_block();
        let saved_subst = mem::replace(&mut self.type_subst, subst.clone());

        self.builder.position_at_end(entry);

        for (i, param) in func_ast.params.iter().enumerate() {
            if let Param::Regular { name: pname, .. } = param {
                let ty = &param_types[i];
                if let Some(llvm_ty) = to_llvm_type(ty, self.context, &self.struct_types) {
                    let alloca = self.builder.build_alloca(llvm_ty, pname).unwrap();
                    let param_val = fn_value.get_nth_param(i as u32).unwrap();
                    self.builder.build_store(alloca, param_val).unwrap();
                    self.variables
                        .insert(pname.clone(), (alloca, ty.clone(), Ownership::Unowned));
                }
            }
        }

        self.compile_function_body(&func_ast.body, &return_type, fn_value, false)?;

        self.variables = saved_vars;
        self.type_subst = saved_subst;
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        Ok(())
    }

    /// Generates a monomorphized (specialized) version of a generic struct for
    /// the given concrete type arguments. Creates the LLVM struct type with
    /// concrete field types and registers it under the mangled name.
    pub fn monomorphize_struct(&mut self, name: &str, type_args: &[Type]) -> Result<(), String> {
        let mangled = mangle_name(name, type_args);
        if self.struct_types.contains_key(&mangled) {
            return Ok(());
        }

        if name == "List" {
            return crate::list::monomorphize_list_struct(self, &mangled);
        }
        if name == "Map" || name == "Set" {
            return crate::hashtable::monomorphize_hashtable_struct(self, &mangled);
        }
        if name == "Ref" {
            return crate::process::monomorphize_ref_struct(self, &mangled);
        }
        if name == "ReplyTo" {
            return crate::process::monomorphize_reply_to_struct(self, &mangled);
        }

        let info = self
            .type_ctx
            .types
            .get(name)
            .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;
        let fields = info
            .fields()
            .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;

        let subst = build_substitution(&info.type_params, type_args);

        let concrete_fields: Vec<(String, Type)> = fields
            .iter()
            .map(|(fname, fty)| (fname.clone(), substitute(fty, &subst)))
            .collect();

        let st = self.context.opaque_struct_type(&mangled);
        self.struct_types.insert(mangled.clone(), st);

        let mut deferred_indirect = Vec::new();
        for (_, fty) in &concrete_fields {
            if let Type::Indirect(inner) = fty {
                deferred_indirect.push(inner.as_ref().clone());
            } else {
                self.ensure_types_exist(fty)?;
            }
        }

        // `to_llvm_type` returns `None` for `Unit` and other ZSTs, but we must keep one
        // LLVM field per logical field so GEP indices match `mono_struct_info` (e.g.
        // `Pair<Unit, T>.second` is index 1, not 0 when `first` is Unit).
        let field_llvm_types: Vec<_> = concrete_fields
            .iter()
            .map(|(_, ty)| {
                to_llvm_type(ty, self.context, &self.struct_types).unwrap_or_else(|| {
                    // Placeholder for ZST / missing LLVM mapping; keeps field indices aligned.
                    self.context.i8_type().into()
                })
            })
            .collect();
        st.set_body(&field_llvm_types, false);

        for ty in &deferred_indirect {
            self.ensure_types_exist(ty)?;
        }

        self.mono_struct_info.insert(mangled, concrete_fields);

        Ok(())
    }

    /// Generates a monomorphized (specialized) version of a generic enum for
    /// the given concrete type arguments. Creates the LLVM tagged union type
    /// with concrete variant payloads and registers it under the mangled name.
    pub fn monomorphize_enum(&mut self, name: &str, type_args: &[Type]) -> Result<(), String> {
        let mangled = mangle_name(name, type_args);
        if self.struct_types.contains_key(&mangled) {
            return Ok(());
        }

        let info = self
            .type_ctx
            .types
            .get(name)
            .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;
        let variants = info
            .variants()
            .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;

        let subst = build_substitution(&info.type_params, type_args);

        let concrete_variants: Vec<_> = variants
            .iter()
            .map(|vi| {
                let data = match &vi.data {
                    VariantData::Unit => VariantData::Unit,
                    VariantData::Tuple(types) => {
                        VariantData::Tuple(types.iter().map(|t| substitute(t, &subst)).collect())
                    }
                    VariantData::Struct(fields) => VariantData::Struct(
                        fields
                            .iter()
                            .map(|(n, t)| (n.clone(), substitute(t, &subst)))
                            .collect(),
                    ),
                };
                (vi.name.clone(), data)
            })
            .collect();

        let enum_type = self.context.opaque_struct_type(&mangled);
        self.struct_types.insert(mangled.clone(), enum_type);

        for (_, vdata) in &concrete_variants {
            match vdata {
                VariantData::Unit => {}
                VariantData::Tuple(types) => {
                    for ty in types {
                        self.ensure_types_exist(ty)?;
                    }
                }
                VariantData::Struct(fields) => {
                    for (_, ty) in fields {
                        self.ensure_types_exist(ty)?;
                    }
                }
            }
        }

        self.build_enum_layout(&mangled, enum_type, &concrete_variants);

        self.mono_enum_variants.insert(mangled, concrete_variants);

        Ok(())
    }

    /// Generates a monomorphized version of a method from a generic impl block.
    /// Finds the method AST in `generic_impl_asts`, substitutes the type
    /// parameters with concrete type args, and compiles the body.
    ///
    /// When `method_type_args` is non-empty, method-level type parameters
    /// (e.g. `U` in `map<U>`) are also substituted into the mangled name
    /// and type substitution map.
    pub fn monomorphize_impl_method(
        &mut self,
        base_type: &str,
        method_name: &str,
        type_args: &[Type],
        method_type_args: &[Type],
    ) -> Result<(), String> {
        let mangled_type = mangle_name(base_type, type_args);
        let mangled_fn = if method_type_args.is_empty() {
            format!("{}_{}", mangled_type, method_name)
        } else {
            let mangled_method = mangle_name(method_name, method_type_args);
            format!("{}_{}", mangled_type, mangled_method)
        };
        if self.functions.contains_key(&mangled_fn) {
            return Ok(());
        }

        if method_type_args.is_empty() {
            match base_type {
                "List" => {
                    if let crate::compiler::EmitResult::Emitted = crate::list::emit_list_method(
                        self,
                        &mangled_type,
                        &mangled_fn,
                        method_name,
                        type_args,
                    )? {
                        return Ok(());
                    }
                }
                "Map" => {
                    if let crate::compiler::EmitResult::Emitted = crate::map::emit_map_method(
                        self,
                        &mangled_type,
                        &mangled_fn,
                        method_name,
                        type_args,
                    )? {
                        return Ok(());
                    }
                }
                "Set" => {
                    if let crate::compiler::EmitResult::Emitted = crate::set::emit_set_method(
                        self,
                        &mangled_type,
                        &mangled_fn,
                        method_name,
                        type_args,
                    )? {
                        return Ok(());
                    }
                }
                "Ref" => {
                    if let crate::compiler::EmitResult::Emitted = crate::process::emit_ref_method(
                        self,
                        &mangled_type,
                        &mangled_fn,
                        method_name,
                        type_args,
                    )? {
                        return Ok(());
                    }
                }
                "ReplyTo" => {
                    if let crate::compiler::EmitResult::Emitted =
                        crate::process::emit_reply_to_method(
                            self,
                            &mangled_type,
                            &mangled_fn,
                            method_name,
                            type_args,
                        )?
                    {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }

        let impl_blocks = self
            .type_ctx
            .generic_impl_asts
            .get(base_type)
            .ok_or_else(|| format!("no generic impl for `{base_type}`"))?
            .clone();

        let mut method_ast = None;
        let mut impl_type_params = Vec::new();
        for block in &impl_blocks {
            if let TypeExpr::Generic { args, .. } = &block.target {
                let tp_names: Vec<String> = args
                    .iter()
                    .filter_map(|a| {
                        if let TypeExpr::Named { path, .. } = a
                            && path.len() == 1
                        {
                            return Some(path[0].clone());
                        }
                        None
                    })
                    .collect();
                for member in &block.members {
                    if let ImplMember::Function(f) = member
                        && f.name == method_name
                    {
                        method_ast = Some(f.clone());
                        impl_type_params = tp_names;
                        break;
                    }
                }
                if method_ast.is_some() {
                    break;
                }
            }
        }

        let func_ast = method_ast
            .ok_or_else(|| format!("method `{method_name}` not found in impl for `{base_type}`"))?;

        let mut subst = build_substitution(&impl_type_params, type_args);
        for (tp, ta) in func_ast.type_params.iter().zip(method_type_args.iter()) {
            subst.insert(tp.clone(), ta.clone());
        }

        let info = self
            .type_ctx
            .types
            .get(base_type)
            .map(|ti| (&ti.functions, &ti.type_params));

        let (return_type, param_types, is_static) = if let Some((methods, _)) = info {
            if let Some(sig) = methods.get(method_name) {
                let ret = substitute(&sig.return_type, &subst);
                let pts: Vec<Type> = sig
                    .params
                    .iter()
                    .map(|p| substitute(&p.ty, &subst))
                    .collect();
                let is_static = sig.kind == FunctionKind::Static;
                (ret, pts, is_static)
            } else {
                return Err(format!(
                    "no signature for method `{method_name}` on `{base_type}`"
                ));
            }
        } else {
            return Err(format!("no type info for `{base_type}`"));
        };

        self.ensure_types_exist(&return_type)?;
        for pt in &param_types {
            self.ensure_types_exist(pt)?;
        }

        let mut llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum> = Vec::new();

        if !is_static {
            let self_llvm_type = *self
                .struct_types
                .get(&mangled_type)
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;
            llvm_param_types.push(self_llvm_type.into());
        }

        for ty in &param_types {
            let lt = to_llvm_type(ty, self.context, &self.struct_types).ok_or_else(|| {
                format!("no LLVM type for method parameter type `{ty:?}` in `{mangled_fn}`")
            })?;
            llvm_param_types.push(lt.into());
        }

        let fn_type = match to_llvm_type(&return_type, self.context, &self.struct_types) {
            Some(ret) => ret.fn_type(&llvm_param_types, false),
            None => self.context.void_type().fn_type(&llvm_param_types, false),
        };

        let fn_value = self.module.add_function(&mangled_fn, fn_type, None);
        self.functions.insert(mangled_fn.clone(), fn_value);

        let self_type = if is_static {
            None
        } else {
            Some((mangled_type.as_str(), base_type))
        };
        self.compile_method_body(
            fn_value,
            &func_ast,
            self_type,
            &param_types,
            &return_type,
            subst,
        )
    }

    /// Ensures that all concrete types referenced by `ty` have been registered.
    /// For mangled generic names, triggers monomorphization if needed.
    pub(crate) fn ensure_types_exist(&mut self, ty: &Type) -> Result<(), String> {
        match ty {
            Type::Struct(name) => {
                if !self.struct_types.contains_key(name)
                    && let Some((base, type_args)) = parse_mangled_name(name, self)
                {
                    self.monomorphize_struct(&base, &type_args)?;
                }
            }
            Type::Enum(name) => {
                if !self.struct_types.contains_key(name)
                    && let Some((base, type_args)) = parse_mangled_name(name, self)
                {
                    self.monomorphize_enum(&base, &type_args)?;
                }
            }
            Type::GenericInstance {
                base,
                type_args,
                kind,
            } => {
                for arg in type_args {
                    self.ensure_types_exist(arg)?;
                }
                let mangled = mangle_name(base, type_args);
                if !self.struct_types.contains_key(&mangled) {
                    match kind {
                        GenericKind::Struct => self.monomorphize_struct(base, type_args)?,
                        GenericKind::Enum => self.monomorphize_enum(base, type_args)?,
                    }
                }
            }
            Type::Function {
                params,
                return_type,
            } => {
                for p in params {
                    self.ensure_types_exist(p)?;
                }
                self.ensure_types_exist(return_type)?;
            }
            Type::Indirect(inner) => {
                self.ensure_types_exist(inner)?;
            }
            Type::Union(members) => {
                for m in members {
                    self.ensure_types_exist(m)?;
                }
                let mangled = mangle_type(ty);
                if !self.struct_types.contains_key(&mangled) {
                    let opaque = self.context.opaque_struct_type(&mangled);
                    self.struct_types.insert(mangled.clone(), opaque);
                    self.build_union_layout(&mangled, opaque, members);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Public entry point for parsing a mangled name from call sites outside this
/// module (e.g. method call dispatch in `structs.rs`).
pub fn try_parse_mangled_name(mangled: &str, c: &Compiler) -> Option<(String, Vec<Type>)> {
    parse_mangled_name(mangled, c)
}

/// Attempts to recover the base name and concrete type args from a mangled
/// name like `Pair_$i32.string$`. Returns `None` if the name doesn't match
/// a known generic struct or enum template.
fn parse_mangled_name(mangled: &str, c: &Compiler) -> Option<(String, Vec<Type>)> {
    let sep_pos = mangled.find("_$")?;
    let base = &mangled[..sep_pos];
    if !c.type_ctx.generic_struct_asts.contains_key(base)
        && !c.type_ctx.generic_enum_asts.contains_key(base)
    {
        return None;
    }
    if !mangled.ends_with('$') {
        return None;
    }
    let inner = &mangled[sep_pos + 2..mangled.len() - 1];
    let parts = split_mangled_args(inner);
    let type_args: Vec<Type> = parts.iter().map(|s| parse_mangled_type(s)).collect();
    Some((base.to_string(), type_args))
}

/// Splits a mangled args string on `.` at depth 0, respecting nested `_$...$`.
fn split_mangled_args(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'$' {
            depth += 1;
            current.push('_');
            current.push('$');
            i += 2;
        } else if bytes[i] == b'$' {
            depth -= 1;
            current.push('$');
            i += 1;
        } else if bytes[i] == b'.' && depth == 0 {
            parts.push(mem::take(&mut current));
            i += 1;
        } else {
            current.push(bytes[i] as char);
            i += 1;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn parse_mangled_type(s: &str) -> Type {
    if s == "unit" {
        return Type::Unit;
    }
    if let Some(p) = Primitive::from_name(s) {
        return Type::Primitive(p);
    }
    Type::Struct(s.to_string())
}
