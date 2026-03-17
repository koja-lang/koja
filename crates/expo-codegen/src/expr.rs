//! Expression compilation: translates Expo expressions (literals, variables,
//! binary/unary ops, calls, closures, string interpolation, etc.) into LLVM IR.

use expo_ast::ast::{ClosureParam, Expr, Literal, Statement, StringPart};

use expo_typecheck::types::Type;
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::calls::compile_call;
use crate::compiler::Compiler;
use crate::control::{
    compile_cond, compile_for, compile_if, compile_loop, compile_match, compile_ternary,
    compile_while,
};
use crate::enums::compile_enum_construction;
use crate::ops::{compile_binary, compile_unary};
use crate::structs::{compile_field_access, compile_method_call, compile_struct_construction};
use crate::types::to_llvm_type;

/// Top-level expression dispatch. Matches each AST expression variant and
/// delegates to the appropriate specialized compiler function.
pub fn compile_expr<'ctx>(
    c: &mut Compiler<'ctx>,
    expr: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match expr {
        Expr::Literal { value, .. } => compile_literal(c, value),

        Expr::Ident { name, .. } => {
            if let Some((ptr, ty)) = c.variables.get(name) {
                let llvm_ty = to_llvm_type(ty, c.context, &c.struct_types)
                    .ok_or_else(|| format!("cannot load variable of unsupported type: {name}"))?;
                let val = c.builder.build_load(llvm_ty, *ptr, name).unwrap();
                Ok(Some(val))
            } else if let Some(val) = c.constants.get(name) {
                Ok(Some(*val))
            } else {
                Err(format!("undefined variable: {name}"))
            }
        }

        Expr::Group { expr, .. } => compile_expr(c, expr, function),

        Expr::Binary {
            op, left, right, ..
        } => compile_binary(c, op, left, right, function),

        Expr::Unary { op, operand, .. } => compile_unary(c, op, operand, function),

        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                compile_call(c, name, args, function)
            } else {
                Err("only named function calls are supported".to_string())
            }
        }

        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => compile_if(c, condition, then_body, else_body, function),

        Expr::StructConstruction {
            type_path, fields, ..
        } => compile_struct_construction(c, type_path, fields, function),

        Expr::FieldAccess {
            receiver, field, ..
        } => compile_field_access(c, receiver, field, function),

        Expr::MethodCall {
            receiver,
            method,
            args,
            ..
        } => compile_method_call(c, receiver, method, args, function),

        Expr::String { parts, .. } => compile_string(c, parts, function),

        Expr::Loop { body, .. } => compile_loop(c, body, function),

        Expr::While {
            condition, body, ..
        } => compile_while(c, condition, body, function),

        Expr::Self_ { .. } => {
            if let Some((ptr, ty)) = c.variables.get("self") {
                let llvm_ty = to_llvm_type(ty, c.context, &c.struct_types)
                    .ok_or("cannot load self of unsupported type")?;
                let val = c.builder.build_load(llvm_ty, *ptr, "self").unwrap();
                Ok(Some(val))
            } else {
                Err("self used outside of impl method".to_string())
            }
        }

        Expr::Cond {
            arms, else_body, ..
        } => compile_cond(c, arms, else_body, function),

        Expr::EnumConstruction {
            type_path,
            variant,
            data,
            ..
        } => compile_enum_construction(c, type_path, variant, data, function),

        Expr::Match { subject, arms, .. } => compile_match(c, subject, arms, function),

        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => compile_ternary(c, condition, then_expr, else_expr, function),

        Expr::Closure {
            params,
            return_type,
            body,
            span,
        } => compile_closure(c, params, return_type, body, function, *span),

        Expr::For {
            pattern,
            iterable,
            body,
            ..
        } => compile_for(c, pattern, iterable, body, function),

        _ => Err(format!(
            "not yet supported in compilation: {:?}",
            std::mem::discriminant(expr)
        )),
    }
}

fn compile_literal<'ctx>(
    c: &Compiler<'ctx>,
    lit: &Literal,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match lit {
        Literal::Int(s) => {
            let val = crate::util::parse_int_literal(s)?;
            Ok(Some(
                c.context.i32_type().const_int(val as u64, true).into(),
            ))
        }
        Literal::Float(s) => {
            let val: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
            Ok(Some(c.context.f64_type().const_float(val).into()))
        }
        Literal::Bool(b) => Ok(Some(
            c.context
                .bool_type()
                .const_int(if *b { 1 } else { 0 }, false)
                .into(),
        )),
        Literal::Unit => Ok(None),
    }
}

fn compile_string<'ctx>(
    c: &mut Compiler<'ctx>,
    parts: &[StringPart],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let has_interpolation = parts
        .iter()
        .any(|p| matches!(p, StringPart::Interpolation { .. }));

    if !has_interpolation {
        let mut combined = String::new();
        for part in parts {
            if let StringPart::Literal { value, .. } = part {
                combined.push_str(value);
            }
        }
        let global = c.builder.build_global_string_ptr(&combined, "str").unwrap();
        return Ok(Some(global.as_pointer_value().into()));
    }

    let snprintf = *c.functions.get("snprintf").ok_or("snprintf not declared")?;

    let mut fmt_string = String::new();
    let mut interp_values: Vec<BasicValueEnum<'ctx>> = Vec::new();

    for part in parts {
        match part {
            StringPart::Literal { value, .. } => {
                for ch in value.chars() {
                    if ch == '%' {
                        fmt_string.push_str("%%");
                    } else {
                        fmt_string.push(ch);
                    }
                }
            }
            StringPart::Interpolation { expr, .. } => {
                let val = compile_expr(c, expr, function)?
                    .ok_or("interpolated expression produced no value")?;

                if val.is_int_value() && val.into_int_value().get_type().get_bit_width() == 1 {
                    let str_ptr = crate::util::bool_to_string_ptr(c, val.into_int_value());
                    fmt_string.push_str("%s");
                    interp_values.push(str_ptr.into());
                } else if let Ok(spec) = crate::util::printf_format_spec(&val) {
                    fmt_string.push_str(spec);
                    interp_values.push(val);
                } else if val.is_struct_value() {
                    let str_ptr = enum_value_to_string(c, val, function)?;
                    fmt_string.push_str("%s");
                    interp_values.push(str_ptr.into());
                } else {
                    return Err("cannot interpolate value of unsupported type".to_string());
                }
            }
        }
    }

    let fmt_global = c
        .builder
        .build_global_string_ptr(&fmt_string, "interp_fmt")
        .unwrap();
    let fmt_ptr = fmt_global.as_pointer_value();

    let i32_type = c.context.i32_type();
    let i8_ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());
    let null_ptr = i8_ptr_type.const_null();
    let zero = i32_type.const_int(0, false);

    let mut size_args: Vec<BasicValueEnum> = vec![null_ptr.into(), zero.into(), fmt_ptr.into()];
    size_args.extend_from_slice(&interp_values);
    let size_args_meta: Vec<_> = size_args.iter().map(|v| (*v).into()).collect();

    let size_call = c
        .builder
        .build_call(snprintf, &size_args_meta, "interp_len")
        .unwrap();
    let needed = size_call
        .try_as_basic_value()
        .left()
        .ok_or("snprintf did not return a value")?
        .into_int_value();

    let one = i32_type.const_int(1, false);
    let buf_size = c.builder.build_int_add(needed, one, "buf_size").unwrap();

    let i8_type = c.context.i8_type();
    let buf = c
        .builder
        .build_array_alloca(i8_type, buf_size, "interp_buf")
        .unwrap();

    let mut write_args: Vec<BasicValueEnum> = vec![buf.into(), buf_size.into(), fmt_ptr.into()];
    write_args.extend_from_slice(&interp_values);
    let write_args_meta: Vec<_> = write_args.iter().map(|v| (*v).into()).collect();

    c.builder
        .build_call(snprintf, &write_args_meta, "interp_write")
        .unwrap();

    Ok(Some(buf.into()))
}

/// Converts an enum value to a string pointer for interpolation. Calls
/// `to_string` if the enum defines one, otherwise looks up the variant
/// name from the enum's global name table.
fn enum_value_to_string<'ctx>(
    c: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    _function: FunctionValue<'ctx>,
) -> Result<inkwell::values::PointerValue<'ctx>, String> {
    let sv = val.into_struct_value();
    let st = sv.get_type();
    let enum_name = st
        .get_name()
        .and_then(|n| n.to_str().ok())
        .ok_or("cannot determine enum type for interpolation")?;

    if !c.type_ctx.enums.contains_key(enum_name) {
        return Err(format!(
            "cannot interpolate struct value `{enum_name}` (not an enum)"
        ));
    }

    if c.type_ctx
        .enums
        .get(enum_name)
        .and_then(|ei| ei.methods.get("to_string"))
        .is_some()
    {
        let mangled = format!("{enum_name}_to_string");
        if let Some(to_string_fn) = c.functions.get(&mangled) {
            let result = c
                .builder
                .build_call(*to_string_fn, &[val.into()], "to_str_ret")
                .unwrap();
            return result
                .try_as_basic_value()
                .left()
                .map(|v| v.into_pointer_value())
                .ok_or("to_string did not return a value".to_string());
        }
    }

    let enum_type = *c
        .struct_types
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum type: {enum_name}"))?;
    let table_ptr = *c
        .enum_name_tables
        .get(enum_name)
        .ok_or_else(|| format!("no name table for enum: {enum_name}"))?;

    let alloca = c.builder.build_alloca(enum_type, "interp_enum").unwrap();
    c.builder.build_store(alloca, val).unwrap();
    let tag_ptr = c
        .builder
        .build_struct_gep(enum_type, alloca, 0, "interp_tag_ptr")
        .unwrap();
    let tag = c
        .builder
        .build_load(c.context.i8_type(), tag_ptr, "interp_tag")
        .unwrap()
        .into_int_value();

    let tag_i32 = c
        .builder
        .build_int_z_extend(tag, c.context.i32_type(), "tag_ext")
        .unwrap();

    let ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());
    let variant_count = c
        .type_ctx
        .enums
        .get(enum_name)
        .map(|ei| ei.variants.len() as u32)
        .unwrap_or(0);
    let table_type = ptr_type.array_type(variant_count);
    let zero = c.context.i32_type().const_int(0, false);
    let name_ptr_ptr = unsafe {
        c.builder
            .build_in_bounds_gep(table_type, table_ptr, &[zero, tag_i32], "name_ptr_ptr")
            .unwrap()
    };
    let name_ptr = c
        .builder
        .build_load(ptr_type, name_ptr_ptr, "variant_name")
        .unwrap()
        .into_pointer_value();

    Ok(name_ptr)
}

fn resolve_closure_params<'ctx>(
    c: &Compiler<'ctx>,
    params: &[ClosureParam],
) -> Vec<expo_typecheck::types::Type> {
    params
        .iter()
        .map(|p| match p {
            ClosureParam::Name {
                type_expr: Some(te),
                ..
            } => c.resolve_type_expr(te),
            _ => expo_typecheck::types::Type::Primitive(expo_typecheck::types::Primitive::I32),
        })
        .collect()
}

/// Compiles a block closure (`fn (params) -> type ... end`) into an anonymous
/// LLVM function and returns a fat pointer `{ fn_ptr, env_ptr }`. Every closure
/// function receives an implicit `env_ptr: ptr` as its first parameter.
fn compile_closure<'ctx>(
    c: &mut Compiler<'ctx>,
    params: &[ClosureParam],
    return_type: &Option<expo_ast::ast::TypeExpr>,
    body: &[Statement],
    _parent_fn: FunctionValue<'ctx>,
    span: expo_ast::span::Span,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let param_types = resolve_closure_params(c, params);

    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

    let mut llvm_meta_params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptr_ty.into()]; // env_ptr is always the first param
    for ty in &param_types {
        if let Some(lt) = to_llvm_type(ty, c.context, &c.struct_types) {
            llvm_meta_params.push(lt.into());
        }
    }

    let ret_type = match return_type {
        Some(te) => c.resolve_type_expr(te),
        None => expo_typecheck::types::Type::Unit,
    };
    let fn_type = match to_llvm_type(&ret_type, c.context, &c.struct_types) {
        Some(ret_llvm) => ret_llvm.fn_type(&llvm_meta_params, false),
        None => c.context.void_type().fn_type(&llvm_meta_params, false),
    };

    let closure_name = format!("__closure_{}", c.closure_counter);
    c.closure_counter += 1;

    let captures = c.type_ctx.closure_captures.get(&span).cloned();

    // Read captured values from parent scope before saving variables
    let captured_values: Vec<(String, inkwell::values::BasicValueEnum<'ctx>, Type)> =
        if let Some(ref caps) = captures {
            caps.iter()
                .filter_map(|cap| {
                    let (ptr, ty) = c.variables.get(&cap.name)?;
                    let llvm_ty = to_llvm_type(ty, c.context, &c.struct_types)?;
                    let val = c.builder.build_load(llvm_ty, *ptr, &cap.name).unwrap();
                    Some((cap.name.clone(), val, ty.clone()))
                })
                .collect()
        } else {
            Vec::new()
        };

    let closure_fn = c.module.add_function(&closure_name, fn_type, None);
    let entry = c.context.append_basic_block(closure_fn, "entry");

    let saved_vars = std::mem::take(&mut c.variables);
    let saved_block = c.builder.get_insert_block();
    let saved_subst = {
        let mut extra = std::collections::HashMap::<String, Type>::new();
        if let Type::GenericInstance {
            base, type_args, ..
        } = &ret_type
        {
            let type_params = c
                .type_ctx
                .enums
                .get(base.as_str())
                .map(|ei| &ei.type_params)
                .or_else(|| {
                    c.type_ctx
                        .structs
                        .get(base.as_str())
                        .map(|si| &si.type_params)
                });
            if let Some(tps) = type_params {
                for (tp, ta) in tps.iter().zip(type_args.iter()) {
                    extra.insert(tp.clone(), ta.clone());
                }
            }
        }
        if extra.is_empty() {
            None
        } else {
            let mut merged = c.type_subst.clone();
            merged.extend(extra);
            Some(std::mem::replace(&mut c.type_subst, merged))
        }
    };

    c.builder.position_at_end(entry);

    // Bind user params (offset by 1 for the env_ptr param)
    for (i, param) in params.iter().enumerate() {
        if let ClosureParam::Name { name, .. } = param {
            let ty = &param_types[i];
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.struct_types) {
                let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
                let param_val = closure_fn.get_nth_param((i + 1) as u32).unwrap();
                c.builder.build_store(alloca, param_val).unwrap();
                c.variables.insert(name.clone(), (alloca, ty.clone()));
            }
        }
    }

    // Load captured variables from the env struct into local allocas
    if !captured_values.is_empty() {
        let env_ptr = closure_fn.get_nth_param(0).unwrap().into_pointer_value();
        let env_field_types: Vec<inkwell::types::BasicTypeEnum> = captured_values
            .iter()
            .filter_map(|(_, _, ty)| to_llvm_type(ty, c.context, &c.struct_types))
            .collect();
        let env_struct_ty = c.context.struct_type(&env_field_types, false);

        for (i, (name, _, ty)) in captured_values.iter().enumerate() {
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.struct_types) {
                let field_ptr = c
                    .builder
                    .build_struct_gep(env_struct_ty, env_ptr, i as u32, &format!("cap_{name}"))
                    .unwrap();
                let val = c
                    .builder
                    .build_load(llvm_ty, field_ptr, &format!("load_{name}"))
                    .unwrap();
                let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
                c.builder.build_store(alloca, val).unwrap();
                c.variables.insert(name.clone(), (alloca, ty.clone()));
            }
        }
    }

    let last_val = crate::control::compile_body_as_value(c, body, closure_fn)?;
    if !c.current_block_terminated() {
        match last_val {
            Some(v) => c.builder.build_return(Some(&v)).unwrap(),
            None => c.builder.build_return(None).unwrap(),
        };
    }

    c.variables = saved_vars;
    if let Some(old) = saved_subst {
        c.type_subst = old;
    }
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }

    // Build the env_ptr: malloc + store captures, or null for non-capturing
    let env_ptr_val = if !captured_values.is_empty() {
        let env_field_types: Vec<inkwell::types::BasicTypeEnum> = captured_values
            .iter()
            .filter_map(|(_, _, ty)| to_llvm_type(ty, c.context, &c.struct_types))
            .collect();
        let env_struct_ty = c.context.struct_type(&env_field_types, false);
        let env_size = env_struct_ty.size_of().unwrap();

        let malloc = *c
            .functions
            .get("malloc")
            .expect("malloc not declared in builtins");
        let raw_ptr = c
            .builder
            .build_call(malloc, &[env_size.into()], "env_alloc")
            .unwrap()
            .try_as_basic_value()
            .left()
            .unwrap()
            .into_pointer_value();

        for (i, (name, val, _)) in captured_values.iter().enumerate() {
            let field_ptr = c
                .builder
                .build_struct_gep(env_struct_ty, raw_ptr, i as u32, &format!("env_{name}"))
                .unwrap();
            c.builder.build_store(field_ptr, *val).unwrap();
        }

        raw_ptr
    } else {
        ptr_ty.const_null()
    };

    // Build the fat pointer struct { fn_ptr, env_ptr }
    let closure_struct_ty = c
        .context
        .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
    let fn_ptr = closure_fn.as_global_value().as_pointer_value();
    let mut fat_ptr = closure_struct_ty.get_undef();
    fat_ptr = c
        .builder
        .build_insert_value(fat_ptr, fn_ptr, 0, "insert_fn")
        .unwrap()
        .into_struct_value();
    fat_ptr = c
        .builder
        .build_insert_value(fat_ptr, env_ptr_val, 1, "insert_env")
        .unwrap()
        .into_struct_value();

    Ok(Some(fat_ptr.into()))
}
