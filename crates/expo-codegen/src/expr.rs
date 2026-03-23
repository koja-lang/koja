//! Expression compilation: translates Expo expressions (literals, variables,
//! binary/unary ops, calls, closures, string interpolation, etc.) into LLVM IR.

use expo_ast::ast::{ClosureParam, Expr, Literal, Statement, StringPart};

use expo_typecheck::types::{Primitive, Type, build_substitution, mangle_name, substitute};
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::binary::construction::compile_binary_literal;
use crate::calls::compile_call;
use crate::compiler::Compiler;
use crate::control::{
    compile_cond, compile_for, compile_if, compile_loop, compile_match, compile_ternary,
    compile_unless, compile_while,
};
use crate::drop::Ownership;
use crate::enums::compile_enum_construction;
use crate::ops::{compile_binary, compile_unary};
use crate::stmt::{apply_coercion, coerce_numeric, infer_type_from_llvm};
use crate::structs::{compile_field_access, compile_method_call, compile_struct_construction};
use crate::types::to_llvm_type;

/// Compiles an expression and coerces the result to the expected type.
/// Use when the target type is known (e.g. function arguments, struct fields).
pub fn compile_expr_coerced<'ctx>(
    c: &mut Compiler<'ctx>,
    expr: &Expr,
    expected: &Type,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let val = compile_expr(c, expr, function)?;
    match val {
        Some(v) => {
            let v = coerce_numeric(c, v, expected);
            let v = apply_coercion(c, v, expr)?;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

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
            if let Some((ptr, ty, _)) = c.variables.get(name) {
                let llvm_ty = to_llvm_type(ty, c.context, &c.struct_types)
                    .ok_or_else(|| format!("cannot load variable of unsupported type: {name}"))?;
                let val = c.builder.build_load(llvm_ty, *ptr, name).unwrap();
                Ok(Some(val))
            } else if let Some(val) = c.constants.get(name) {
                Ok(Some(*val))
            } else if c.module.get_function(name).is_some() {
                let thunk = c.get_or_create_thunk(name)?;
                let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
                let closure_struct_ty = c
                    .context
                    .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                let thunk_ptr = thunk.as_global_value().as_pointer_value();
                let null_env = ptr_ty.const_null();
                let mut fat_ptr = closure_struct_ty.get_undef();
                fat_ptr = c
                    .builder
                    .build_insert_value(fat_ptr, thunk_ptr, 0, "insert_fn")
                    .unwrap()
                    .into_struct_value();
                fat_ptr = c
                    .builder
                    .build_insert_value(fat_ptr, null_env, 1, "insert_env")
                    .unwrap()
                    .into_struct_value();
                Ok(Some(fat_ptr.into()))
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
            if let Some((ptr, ty, _)) = c.variables.get("self") {
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

        Expr::Unless {
            condition, body, ..
        } => compile_unless(c, condition, body, function),

        Expr::List { elements, .. } => compile_list_literal(c, elements, function),

        Expr::Map { entries, .. } => compile_map_literal(c, entries, function),

        Expr::BinaryLiteral { segments, .. } => compile_binary_literal(c, segments, function),

        Expr::Spawn { expr, .. } => compile_spawn(c, expr, function),

        Expr::Receive {
            arms,
            after_timeout,
            after_body,
            ..
        } => compile_receive(c, arms, after_timeout.as_deref(), after_body, function),

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
                c.context.i64_type().const_int(val as u64, true).into(),
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
        Literal::String(_) => unreachable!("string literals use Expr::String, not Expr::Literal"),
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
        let payload_ptr = c.create_string_global(combined.as_bytes(), "str");
        return Ok(Some(payload_ptr.into()));
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

    let malloc_fn = *c.functions.get("malloc").ok_or("malloc not declared")?;
    let i64_type = c.context.i64_type();
    let i8_type = c.context.i8_type();
    let needed_i64 = c
        .builder
        .build_int_z_extend(needed, i64_type, "needed_i64")
        .unwrap();
    let alloc_size = c
        .builder
        .build_int_add(needed_i64, i64_type.const_int(9, false), "interp_alloc_sz")
        .unwrap();
    let base_ptr = c
        .builder
        .build_call(malloc_fn, &[alloc_size.into()], "interp_base")
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("malloc did not return a value")?
        .into_pointer_value();

    let bit_length = c
        .builder
        .build_int_mul(needed_i64, i64_type.const_int(8, false), "bit_length")
        .unwrap();
    c.builder.build_store(base_ptr, bit_length).unwrap();

    let payload = unsafe {
        c.builder
            .build_in_bounds_gep(
                i8_type,
                base_ptr,
                &[i64_type.const_int(8, false)],
                "interp_payload",
            )
            .unwrap()
    };

    let mut write_args: Vec<BasicValueEnum> = vec![payload.into(), buf_size.into(), fmt_ptr.into()];
    write_args.extend_from_slice(&interp_values);
    let write_args_meta: Vec<_> = write_args.iter().map(|v| (*v).into()).collect();

    c.builder
        .build_call(snprintf, &write_args_meta, "interp_write")
        .unwrap();

    Ok(Some(payload.into()))
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
                    let (ptr, ty, _) = c.variables.get(&cap.name)?;
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
                c.variables
                    .insert(name.clone(), (alloca, ty.clone(), Ownership::Unowned));
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
                c.variables
                    .insert(name.clone(), (alloca, ty.clone(), Ownership::Unowned));
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

fn compile_list_literal<'ctx>(
    c: &mut Compiler<'ctx>,
    elements: &[Expr],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let compiled: Vec<BasicValueEnum<'ctx>> = elements
        .iter()
        .map(|e| {
            compile_expr(c, e, function)
                .and_then(|v| v.ok_or("list element produced no value".into()))
        })
        .collect::<Result<_, _>>()?;

    let elem_type = if let Some(subst) = c.type_subst.get("T") {
        subst.clone()
    } else if let Some(first) = compiled.first() {
        infer_type_from_llvm(c, first)
    } else {
        Type::Primitive(expo_typecheck::types::Primitive::I32)
    };
    let type_args = vec![elem_type.clone()];
    let mangled_type = mangle_name("List", &type_args);

    if !c.struct_types.contains_key(&mangled_type) {
        c.monomorphize_struct("List", &type_args)?;
    }

    let new_fn_name = format!("{mangled_type}_new");
    if !c.functions.contains_key(&new_fn_name) {
        c.monomorphize_impl_method("List", "new", &type_args)?;
    }
    let push_fn_name = format!("{mangled_type}_push");
    if !c.functions.contains_key(&push_fn_name) {
        c.monomorphize_impl_method("List", "push", &type_args)?;
    }

    let new_fn = *c.functions.get(&new_fn_name).ok_or("List.new not found")?;
    let push_fn = *c
        .functions
        .get(&push_fn_name)
        .ok_or("List.push not found")?;

    let mut list_val = c
        .builder
        .build_call(new_fn, &[], "list_new")
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("List.new returned void")?;

    for elem in &compiled {
        let coerced = coerce_numeric(c, *elem, &elem_type);
        list_val = c
            .builder
            .build_call(push_fn, &[list_val.into(), coerced.into()], "list_push")
            .unwrap()
            .try_as_basic_value()
            .left()
            .ok_or("List.push returned void")?;
    }

    Ok(Some(list_val))
}

fn compile_map_literal<'ctx>(
    c: &mut Compiler<'ctx>,
    entries: &[(Expr, Expr)],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let (key_type, val_type) =
        if let (Some(k_subst), Some(v_subst)) = (c.type_subst.get("K"), c.type_subst.get("V")) {
            (k_subst.clone(), v_subst.clone())
        } else if let Some((first_k, first_v)) = entries.first() {
            let k_val = compile_expr(c, first_k, function)?.ok_or("map key produced no value")?;
            let v_val = compile_expr(c, first_v, function)?.ok_or("map value produced no value")?;
            (
                infer_type_from_llvm(c, &k_val),
                infer_type_from_llvm(c, &v_val),
            )
        } else {
            return Err("empty map literal requires a type annotation".to_string());
        };

    let type_args = vec![key_type.clone(), val_type.clone()];
    let mangled_type = mangle_name("Map", &type_args);

    if !c.struct_types.contains_key(&mangled_type) {
        c.monomorphize_struct("Map", &type_args)?;
    }

    let new_fn_name = format!("{mangled_type}_new");
    if !c.functions.contains_key(&new_fn_name) {
        c.monomorphize_impl_method("Map", "new", &type_args)?;
    }
    let put_fn_name = format!("{mangled_type}_put");
    if !c.functions.contains_key(&put_fn_name) {
        c.monomorphize_impl_method("Map", "put", &type_args)?;
    }

    let new_fn = *c.functions.get(&new_fn_name).ok_or("Map.new not found")?;
    let put_fn = *c.functions.get(&put_fn_name).ok_or("Map.put not found")?;

    let mut map_val = c
        .builder
        .build_call(new_fn, &[], "map_new")
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("Map.new returned void")?;

    for (key_expr, val_expr) in entries {
        let key = compile_expr(c, key_expr, function)?.ok_or("map key produced no value")?;
        let val = compile_expr(c, val_expr, function)?.ok_or("map value produced no value")?;
        let key = coerce_numeric(c, key, &key_type);
        let val = coerce_numeric(c, val, &val_type);
        map_val = c
            .builder
            .build_call(put_fn, &[map_val.into(), key.into(), val.into()], "map_put")
            .unwrap()
            .try_as_basic_value()
            .left()
            .ok_or("Map.put returned void")?;
    }

    Ok(Some(map_val))
}

fn compile_spawn<'ctx>(
    c: &mut Compiler<'ctx>,
    expr: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let type_name = match expr {
        Expr::MethodCall { receiver, .. }
        | Expr::Call {
            callee: receiver, ..
        } => match receiver.as_ref() {
            Expr::Ident { name, .. } => name.clone(),
            Expr::FieldAccess { receiver: r, .. } => {
                if let Expr::Ident { name, .. } = r.as_ref() {
                    name.clone()
                } else {
                    return Err("spawn requires T.new(config) form".to_string());
                }
            }
            _ => return Err("spawn requires T.new(config) form".to_string()),
        },
        _ => return Err("spawn requires T.new(config) form".to_string()),
    };

    let init_value = compile_expr(c, expr, function)?
        .ok_or_else(|| format!("{type_name}.new() did not produce a value"))?;

    let struct_type = init_value.get_type().into_struct_type();
    let state_alloca = c.builder.build_alloca(struct_type, "spawn_state").unwrap();
    c.builder.build_store(state_alloca, init_value).unwrap();

    let state_ptr = c
        .builder
        .build_bit_cast(
            state_alloca,
            c.context.ptr_type(inkwell::AddressSpace::default()),
            "state_ptr",
        )
        .unwrap();

    let state_size = struct_type.size_of().unwrap();
    let state_size_i64 = c
        .builder
        .build_int_cast(state_size, c.context.i64_type(), "state_size")
        .unwrap();

    let mangled_state = c.mangled_name_for_struct_type(struct_type).ok_or_else(|| {
        format!("could not resolve mangled struct name for spawn state (receiver `{type_name}`)")
    })?;
    if let Some((base, type_args)) = crate::generics::try_parse_mangled_name(&mangled_state, c) {
        c.monomorphize_impl_method(&base, "run", &type_args)?;
    }
    let run_fn_name = format!("{mangled_state}_run");
    let run_fn = c
        .module
        .get_function(&run_fn_name)
        .ok_or_else(|| format!("undefined run function: {run_fn_name}"))?;

    let wrapper_name = format!("__spawn_{mangled_state}");
    let wrapper = if let Some(existing) = c.module.get_function(&wrapper_name) {
        existing
    } else {
        let i8_ptr = c.context.ptr_type(inkwell::AddressSpace::default());
        let wrapper_type = c.context.void_type().fn_type(&[i8_ptr.into()], false);
        let wrapper_fn = c.module.add_function(&wrapper_name, wrapper_type, None);

        let entry = c.context.append_basic_block(wrapper_fn, "entry");

        let saved_block = c.builder.get_insert_block();
        c.builder.position_at_end(entry);

        let raw_ptr = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
        let typed_ptr = c
            .builder
            .build_bit_cast(
                raw_ptr,
                c.context.ptr_type(inkwell::AddressSpace::default()),
                "typed_ptr",
            )
            .unwrap()
            .into_pointer_value();
        let loaded = c
            .builder
            .build_load(struct_type, typed_ptr, "loaded_state")
            .unwrap();

        c.builder.build_call(run_fn, &[loaded.into()], "").unwrap();
        c.builder.build_return(None).unwrap();

        if let Some(bb) = saved_block {
            c.builder.position_at_end(bb);
        }

        wrapper_fn
    };

    let wrapper_ptr = wrapper.as_global_value().as_pointer_value();

    let spawn_fn = *c
        .functions
        .get("expo_rt_spawn")
        .ok_or("expo_rt_spawn not declared")?;

    let pid = c
        .builder
        .build_call(
            spawn_fn,
            &[wrapper_ptr.into(), state_ptr.into(), state_size_i64.into()],
            "spawn_pid",
        )
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("expo_rt_spawn did not return a value")?
        .into_int_value();

    // `protocol_impls` stores generic `Process<…, M, R>` args; for `spawn Task.new(…)`
    // the state is monomorphized (e.g. `Task_$Int$`) so we must substitute `R` → `Int`
    // when building `Ref<M, R>` — same idea as `resolve_process_envelope_type`.
    let (msg_type, reply_type) = if let Some((base, type_args)) =
        crate::generics::try_parse_mangled_name(&mangled_state, c)
    {
        let impls = c
            .type_ctx
            .protocol_impls
            .get(&base)
            .ok_or_else(|| format!("`{base}` does not implement Process"))?;
        let (_, proto_args) = impls
            .iter()
            .find(|(proto, _)| proto == "Process")
            .ok_or_else(|| format!("`{base}` does not implement Process"))?;
        let si = c
            .type_ctx
            .structs
            .get(&base)
            .ok_or_else(|| format!("no struct `{base}` for Process impl"))?;
        let subst = build_substitution(&si.type_params, &type_args);
        let default_m_r = Type::Primitive(Primitive::String);
        let m = substitute(proto_args.get(1).unwrap_or(&default_m_r), &subst);
        let r = substitute(proto_args.get(2).unwrap_or(&default_m_r), &subst);
        (m, r)
    } else {
        let process_args = c
            .type_ctx
            .protocol_impls
            .get(&type_name)
            .and_then(|impls| {
                impls
                    .iter()
                    .find(|(proto, _)| proto == "Process")
                    .map(|(_, args)| args.clone())
            })
            .ok_or_else(|| format!("`{type_name}` does not implement Process"))?;
        let m = process_args
            .get(1)
            .cloned()
            .unwrap_or(Type::Primitive(Primitive::String));
        let r = process_args
            .get(2)
            .cloned()
            .unwrap_or(Type::Primitive(Primitive::String));
        (m, r)
    };

    let type_args = vec![msg_type, reply_type];
    let mangled = mangle_name("Ref", &type_args);
    if !c.struct_types.contains_key(&mangled) {
        c.monomorphize_struct("Ref", &type_args)?;
    }
    let ref_struct = *c
        .struct_types
        .get(&mangled)
        .ok_or("Ref struct type not found")?;

    let mut sv = ref_struct.get_undef();
    sv = c
        .builder
        .build_insert_value(sv, pid, 0, "wrap_pid")
        .unwrap()
        .into_struct_value();

    Ok(Some(sv.into()))
}

fn compile_receive<'ctx>(
    c: &mut Compiler<'ctx>,
    arms: &[expo_ast::ast::MatchArm],
    after_timeout: Option<&Expr>,
    after_body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let has_after = after_timeout.is_some();

    let raw_ptr = if let Some(timeout_expr) = after_timeout {
        let receive_timeout_fn = *c
            .functions
            .get("expo_rt_receive_timeout")
            .ok_or("expo_rt_receive_timeout not declared")?;

        let timeout_val = compile_expr(c, timeout_expr, function)?
            .ok_or("after timeout expression produced no value")?;

        c.builder
            .build_call(receive_timeout_fn, &[timeout_val.into()], "receive_msg")
            .unwrap()
            .try_as_basic_value()
            .left()
            .ok_or("expo_rt_receive_timeout did not return a value")?
    } else {
        let receive_fn = *c
            .functions
            .get("expo_rt_receive")
            .ok_or("expo_rt_receive not declared")?;

        c.builder
            .build_call(receive_fn, &[], "receive_msg")
            .unwrap()
            .try_as_basic_value()
            .left()
            .ok_or("expo_rt_receive did not return a value")?
    };

    let merge_bb = c.context.append_basic_block(function, "recv_end");

    {
        let ptr_val = raw_ptr.into_pointer_value();
        let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
        let null_ptr = ptr_ty.const_null();
        let is_null = c
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, ptr_val, null_ptr, "is_timeout")
            .unwrap();

        if has_after {
            let after_bb = c.context.append_basic_block(function, "recv_after");
            let got_msg_bb = c.context.append_basic_block(function, "recv_got_msg");

            c.builder
                .build_conditional_branch(is_null, after_bb, got_msg_bb)
                .unwrap();

            c.builder.position_at_end(after_bb);
            for stmt in after_body {
                crate::stmt::compile_statement(c, stmt, function)?;
            }
            if !c.current_block_terminated() {
                c.builder.build_unconditional_branch(merge_bb).unwrap();
            }

            c.builder.position_at_end(got_msg_bb);
        } else {
            let got_msg_bb = c.context.append_basic_block(function, "recv_got_msg");
            let empty_bb = c.context.append_basic_block(function, "recv_empty");

            c.builder
                .build_conditional_branch(is_null, empty_bb, got_msg_bb)
                .unwrap();

            c.builder.position_at_end(empty_bb);
            c.builder.build_return(None).unwrap();

            c.builder.position_at_end(got_msg_bb);
        }
    }

    let msg_type = c.process_msg_type.clone();
    let is_string = msg_type
        .as_ref()
        .is_none_or(|t| matches!(t, Type::Primitive(Primitive::String)));

    let subject_val = if is_string {
        let i8_type = c.context.i8_type();
        let payload = unsafe {
            c.builder
                .build_in_bounds_gep(
                    i8_type,
                    raw_ptr.into_pointer_value(),
                    &[c.context.i64_type().const_int(8, false)],
                    "recv_str_payload",
                )
                .unwrap()
        };
        payload.into()
    } else {
        let msg_ty = msg_type.unwrap();
        let llvm_ty = crate::types::to_llvm_type(&msg_ty, c.context, &c.struct_types)
            .ok_or_else(|| format!("no LLVM type for receive message `{msg_ty:?}`"))?;
        let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
        let typed_ptr = c
            .builder
            .build_pointer_cast(raw_ptr.into_pointer_value(), ptr_ty, "msg_typed_ptr")
            .unwrap();
        c.builder.build_load(llvm_ty, typed_ptr, "msg_val").unwrap()
    };

    if arms.is_empty() {
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }
        c.builder.position_at_end(merge_bb);
        return Ok(Some(subject_val));
    }

    let subject_type = if let Some(mt) = &c.process_msg_type {
        mt.clone()
    } else {
        crate::stmt::infer_type_from_llvm(c, &subject_val)
    };

    let subject_alloca = c
        .builder
        .build_alloca(subject_val.get_type(), "recv_subject")
        .unwrap();
    c.builder.build_store(subject_alloca, subject_val).unwrap();

    let fallthrough_bb = c.context.append_basic_block(function, "recv_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();
    let mut reachable_arm_count = 0usize;

    for (i, arm) in arms.iter().enumerate() {
        let body_bb = c
            .context
            .append_basic_block(function, &format!("recv_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            c.context
                .append_basic_block(function, &format!("recv_test_{}", i + 1))
        } else {
            fallthrough_bb
        };

        let saved_vars = c.variables.clone();

        let condition = crate::control::compile_pattern(
            c,
            &arm.pattern,
            subject_alloca,
            &subject_type,
            function,
        )?;

        let final_cond = if let Some(guard) = &arm.guard {
            let guard_val =
                compile_expr(c, guard, function)?.ok_or("receive guard produced no value")?;
            c.builder
                .build_and(condition, guard_val.into_int_value(), "guard_and")
                .unwrap()
        } else {
            condition
        };

        c.builder
            .build_conditional_branch(final_cond, body_bb, next_bb)
            .unwrap();

        c.builder.position_at_end(body_bb);
        let arm_val = crate::control::compile_body_as_value(c, &arm.body, function)?;
        let arm_terminated = c.current_block_terminated();
        if !arm_terminated {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            reachable_arm_count += 1;
        }
        let arm_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(val) = arm_val {
            incoming.push((val, arm_end_bb));
        }

        c.variables = saved_vars;
        c.builder.position_at_end(next_bb);
    }

    c.builder.position_at_end(fallthrough_bb);
    c.builder.build_unconditional_branch(merge_bb).unwrap();

    c.builder.position_at_end(merge_bb);

    if !incoming.is_empty() && incoming.len() == reachable_arm_count {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let undef = first_ty.const_zero();
            incoming.push((undef, fallthrough_bb));

            let phi = c.builder.build_phi(first_ty, "recv_result").unwrap();
            let refs: Vec<_> = incoming
                .iter()
                .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
                .collect();
            phi.add_incoming(&refs);
            return Ok(Some(phi.as_basic_value()));
        }
    }

    Ok(None)
}
