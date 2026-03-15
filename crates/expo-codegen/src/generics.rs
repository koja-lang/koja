//! Monomorphization engine: specializes generic functions, structs, and enums
//! for concrete type arguments, and manages the mangled-name encoding used to
//! distinguish each instantiation.

use expo_ast::ast::{Param, Statement};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{GenericKind, Type};
use inkwell::types::BasicType;
use inkwell::values::FunctionValue;

use crate::compiler::{Compiler, type_byte_size};
use crate::expr::compile_expr;
use crate::stmt::compile_statement;
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
        is_main: bool,
    ) -> Result<(), String> {
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
            if is_main {
                let zero = self.context.i32_type().const_int(0, false);
                self.builder.build_return(Some(&zero)).unwrap();
            } else {
                self.builder.build_return(None).unwrap();
            }
        }

        Ok(())
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

        let mangled = expo_typecheck::types::mangle_name(name, type_args);
        if self.functions.contains_key(&mangled) {
            return Ok(());
        }

        let sig = self
            .type_ctx
            .functions
            .get(name)
            .ok_or_else(|| format!("no signature for generic function `{name}`"))?;

        let subst = expo_typecheck::types::build_substitution(&sig.type_params, type_args);

        let return_type = expo_typecheck::types::substitute(&sig.return_type, &subst);

        let param_types: Vec<Type> = sig
            .params
            .iter()
            .map(|p| expo_typecheck::types::substitute(&p.ty, &subst))
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
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_block = self.builder.get_insert_block();

        self.builder.position_at_end(entry);

        for (i, param) in func_ast.params.iter().enumerate() {
            if let Param::Regular { name: pname, .. } = param {
                let ty = &param_types[i];
                if let Some(llvm_ty) = to_llvm_type(ty, self.context, &self.struct_types) {
                    let alloca = self.builder.build_alloca(llvm_ty, pname).unwrap();
                    let param_val = fn_value.get_nth_param(i as u32).unwrap();
                    self.builder.build_store(alloca, param_val).unwrap();
                    self.variables.insert(pname.clone(), (alloca, ty.clone()));
                }
            }
        }

        self.compile_function_body(&func_ast.body, &return_type, fn_value, false)?;

        self.variables = saved_vars;
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        Ok(())
    }

    /// Generates a monomorphized (specialized) version of a generic struct for
    /// the given concrete type arguments. Creates the LLVM struct type with
    /// concrete field types and registers it under the mangled name.
    pub fn monomorphize_struct(&mut self, name: &str, type_args: &[Type]) -> Result<(), String> {
        let mangled = expo_typecheck::types::mangle_name(name, type_args);
        if self.struct_types.contains_key(&mangled) {
            return Ok(());
        }

        let info = self
            .type_ctx
            .structs
            .get(name)
            .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;

        let subst = expo_typecheck::types::build_substitution(&info.type_params, type_args);

        let concrete_fields: Vec<(String, Type)> = info
            .fields
            .iter()
            .map(|(fname, fty)| {
                (
                    fname.clone(),
                    expo_typecheck::types::substitute(fty, &subst),
                )
            })
            .collect();

        let st = self.context.opaque_struct_type(&mangled);
        let field_llvm_types: Vec<_> = concrete_fields
            .iter()
            .filter_map(|(_, ty)| to_llvm_type(ty, self.context, &self.struct_types))
            .collect();
        st.set_body(&field_llvm_types, false);
        self.struct_types.insert(mangled.clone(), st);
        self.mono_struct_info.insert(mangled, concrete_fields);

        Ok(())
    }

    /// Generates a monomorphized (specialized) version of a generic enum for
    /// the given concrete type arguments. Creates the LLVM tagged union type
    /// with concrete variant payloads and registers it under the mangled name.
    pub fn monomorphize_enum(&mut self, name: &str, type_args: &[Type]) -> Result<(), String> {
        let mangled = expo_typecheck::types::mangle_name(name, type_args);
        if self.struct_types.contains_key(&mangled) {
            return Ok(());
        }

        let info = self
            .type_ctx
            .enums
            .get(name)
            .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;

        let subst = expo_typecheck::types::build_substitution(&info.type_params, type_args);

        let concrete_variants: Vec<_> = info
            .variants
            .iter()
            .map(|vi| {
                let data = match &vi.data {
                    VariantData::Unit => VariantData::Unit,
                    VariantData::Tuple(types) => VariantData::Tuple(
                        types
                            .iter()
                            .map(|t| expo_typecheck::types::substitute(t, &subst))
                            .collect(),
                    ),
                    VariantData::Struct(fields) => VariantData::Struct(
                        fields
                            .iter()
                            .map(|(n, t)| (n.clone(), expo_typecheck::types::substitute(t, &subst)))
                            .collect(),
                    ),
                };
                (vi.name.clone(), data)
            })
            .collect();

        let mut variant_payloads = Vec::new();
        let mut max_payload_size: u32 = 0;

        for (vname, vdata) in &concrete_variants {
            match vdata {
                VariantData::Unit => {
                    variant_payloads.push((vname.clone(), None));
                }
                VariantData::Tuple(types) => {
                    let field_llvm: Vec<_> = types
                        .iter()
                        .filter_map(|ty| to_llvm_type(ty, self.context, &self.struct_types))
                        .collect();
                    let payload = self.context.struct_type(&field_llvm, true);
                    let size: u32 = types.iter().map(type_byte_size).sum();
                    max_payload_size = max_payload_size.max(size);
                    variant_payloads.push((vname.clone(), Some(payload)));
                }
                VariantData::Struct(fields) => {
                    let field_llvm: Vec<_> = fields
                        .iter()
                        .filter_map(|(_, ty)| to_llvm_type(ty, self.context, &self.struct_types))
                        .collect();
                    let payload = self.context.struct_type(&field_llvm, true);
                    let size: u32 = fields.iter().map(|(_, ty)| type_byte_size(ty)).sum();
                    max_payload_size = max_payload_size.max(size);
                    variant_payloads.push((vname.clone(), Some(payload)));
                }
            }
        }

        let enum_type = self.context.opaque_struct_type(&mangled);
        let i8_type = self.context.i8_type();
        if max_payload_size > 0 {
            let payload_array = i8_type.array_type(max_payload_size);
            enum_type.set_body(&[i8_type.into(), payload_array.into()], false);
        } else {
            enum_type.set_body(&[i8_type.into()], false);
        }
        self.struct_types.insert(mangled.clone(), enum_type);
        self.enum_variant_payloads
            .insert(mangled.clone(), variant_payloads);

        let ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let name_ptrs: Vec<_> = concrete_variants
            .iter()
            .map(|(vname, _)| {
                let bytes = self.context.const_string(vname.as_bytes(), true);
                let g = self.module.add_global(
                    bytes.get_type(),
                    None,
                    &format!("{mangled}_{vname}_name"),
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
            &format!("{mangled}_variant_names"),
        );
        table_global.set_initializer(&table_init);
        table_global.set_constant(true);
        self.enum_name_tables
            .insert(mangled.clone(), table_global.as_pointer_value());

        self.mono_enum_variants.insert(mangled, concrete_variants);

        Ok(())
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
                let mangled = expo_typecheck::types::mangle_name(base, type_args);
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
            Type::Tuple(elems) => {
                for e in elems {
                    self.ensure_types_exist(e)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
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
            parts.push(std::mem::take(&mut current));
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
    use expo_typecheck::types::Primitive;
    if s == "unit" {
        return Type::Unit;
    }
    if let Some(p) = Primitive::from_name(s) {
        return Type::Primitive(p);
    }
    Type::Struct(s.to_string())
}
