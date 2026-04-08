//! Expression compilation: translates Expo expressions (literals, variables,
//! binary/unary ops, calls, closures, string interpolation, etc.) into LLVM IR.

use expo_ast::ast::{ClosureParam, Expr, Literal, MatchArm, Statement, StringPart, TypeExpr};
use expo_ast::span::Span;

use expo_typecheck::context::FnParam;
use expo_typecheck::types::{Primitive, Type, mangle_name, named, named_generic};
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue};
use std::collections::HashMap;

use crate::binary::construction::compile_binary_literal;
use crate::calls::compile_call;
use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::control::{
    compile_cond, compile_for, compile_if, compile_loop, compile_match, compile_ternary,
    compile_unless, compile_while,
};
use crate::debug::call_format;
use crate::drop::Ownership;
use crate::enums::compile_enum_construction;
use crate::generics::{monomorphize_impl_method, monomorphize_struct};
use crate::ops::{compile_binary, compile_unary};
use crate::stmt::{apply_coercion, coerce_numeric};
use crate::structs::{compile_field_access, compile_method_call, compile_struct_construction};
use crate::types::to_llvm_type;
use crate::util::{parse_int_literal, printf_format_spec};

/// Compiles an expression and coerces the result to the expected type.
/// Use when the target type is known (e.g. function arguments, struct fields).
pub fn compile_expr_coerced<'ctx>(
    c: &mut Compiler<'ctx>,
    expr: &Expr,
    expected: &Type,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let tv = compile_expr(c, expr, function)?;
    match tv {
        Some(tv) => {
            let v = coerce_numeric(c, tv.value, expected);
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
) -> ExprResult<'ctx> {
    match expr {
        Expr::Literal { value, .. } => compile_literal(c, value),

        Expr::Ident { name, .. } => {
            if let Some((ptr, ty, _)) = c.fn_state.variables.get(name) {
                let ty = ty.clone();
                let llvm_ty = to_llvm_type(&ty, c.context, &c.types.structs)
                    .ok_or_else(|| format!("cannot load variable of unsupported type: {name}"))?;
                let val = c.builder.build_load(llvm_ty, *ptr, name).unwrap();
                Ok(Some(TypedValue::new(val, ty)))
            } else if let Some(val) = c.constants.get(name) {
                let ty = c
                    .type_ctx
                    .constants
                    .get(name)
                    .cloned()
                    .unwrap_or(Type::Unknown);
                Ok(Some(TypedValue::new(*val, ty)))
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
                let fn_type = c
                    .type_ctx
                    .functions
                    .get(name)
                    .map(|sig| Type::Function {
                        params: sig.params.iter().map(FnParam::from).collect(),
                        return_type: Box::new(sig.return_type.clone()),
                    })
                    .unwrap_or(Type::Unknown);
                Ok(Some(TypedValue::new(fat_ptr.into(), fn_type)))
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
            if let Some((ptr, ty, _)) = c.fn_state.variables.get("self") {
                let ty = ty.clone();
                let llvm_ty = to_llvm_type(&ty, c.context, &c.types.structs)
                    .ok_or("cannot load self of unsupported type")?;
                let val = c.builder.build_load(llvm_ty, *ptr, "self").unwrap();
                Ok(Some(TypedValue::new(val, ty)))
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

        Expr::ShortClosure { params, body, span } => {
            let body_stmts = vec![Statement::Expr((**body).clone())];
            let ret_type = c
                .type_ctx
                .closure_info
                .get(span)
                .and_then(|ci| ci.return_type.clone())
                .unwrap_or(Type::Unit);
            compile_closure_core(c, params, ret_type, &body_stmts, function, *span)
        }

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

fn compile_literal<'ctx>(c: &Compiler<'ctx>, lit: &Literal) -> ExprResult<'ctx> {
    match lit {
        Literal::Int(s) => {
            let val = parse_int_literal(s)?;
            Ok(Some(TypedValue::new(
                c.context.i64_type().const_int(val as u64, true).into(),
                Type::Primitive(Primitive::I64),
            )))
        }
        Literal::Float(s) => {
            let val: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
            Ok(Some(TypedValue::new(
                c.context.f64_type().const_float(val).into(),
                Type::Primitive(Primitive::F64),
            )))
        }
        Literal::Bool(b) => Ok(Some(TypedValue::new(
            c.context
                .bool_type()
                .const_int(if *b { 1 } else { 0 }, false)
                .into(),
            Type::Primitive(Primitive::Bool),
        ))),
        Literal::String(_) => unreachable!("string literals use Expr::String, not Expr::Literal"),
        Literal::Unit => Ok(None),
    }
}

fn compile_string<'ctx>(
    c: &mut Compiler<'ctx>,
    parts: &[StringPart],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
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
        return Ok(Some(TypedValue::new(
            payload_ptr.into(),
            Type::Primitive(Primitive::String),
        )));
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
                let tv = compile_expr(c, expr, function)?
                    .ok_or("interpolated expression produced no value")?;
                let val = tv.value;

                let is_bool =
                    val.is_int_value() && val.into_int_value().get_type().get_bit_width() == 1;
                let is_plain_printf =
                    !val.is_struct_value() && !is_bool && printf_format_spec(&val).is_ok();

                if is_plain_printf {
                    fmt_string.push_str(printf_format_spec(&val).unwrap());
                    interp_values.push(val);
                } else {
                    let str_ptr = call_format(c, val, &tv.expo_type)?;
                    fmt_string.push_str("%s");
                    interp_values.push(str_ptr.into());
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

    let needed = c
        .call(snprintf, &size_args_meta, "interp_len")
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
        .call(malloc_fn, &[alloc_size.into()], "interp_base")
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

    c.call_void(snprintf, &write_args_meta, "interp_write");

    Ok(Some(TypedValue::new(
        payload.into(),
        Type::Primitive(Primitive::String),
    )))
}

fn resolve_closure_params<'ctx>(
    c: &Compiler<'ctx>,
    params: &[ClosureParam],
    span: Span,
) -> Vec<Type> {
    let all_annotated = params.iter().all(|p| {
        matches!(
            p,
            ClosureParam::Name {
                type_expr: Some(_),
                ..
            }
        )
    });

    if all_annotated {
        return params
            .iter()
            .map(|p| match p {
                ClosureParam::Name {
                    type_expr: Some(te),
                    ..
                } => c.resolve_type_expr(te),
                _ => unreachable!(),
            })
            .collect();
    }

    if let Some(ci) = c.type_ctx.closure_info.get(&span) {
        return ci.param_types.clone();
    }

    params
        .iter()
        .map(|p| match p {
            ClosureParam::Name {
                type_expr: Some(te),
                ..
            } => c.resolve_type_expr(te),
            _ => Type::Primitive(Primitive::I32),
        })
        .collect()
}

/// Compiles a block closure (`fn (params) -> type ... end`) into an anonymous
/// LLVM function and returns a fat pointer `{ fn_ptr, env_ptr }`. Every closure
/// function receives an implicit `env_ptr: ptr` as its first parameter.
fn compile_closure<'ctx>(
    c: &mut Compiler<'ctx>,
    params: &[ClosureParam],
    return_type: &Option<TypeExpr>,
    body: &[Statement],
    parent_fn: FunctionValue<'ctx>,
    span: Span,
) -> ExprResult<'ctx> {
    let ret_type = match return_type {
        Some(te) => c.resolve_type_expr(te),
        None => Type::Unit,
    };
    compile_closure_core(c, params, ret_type, body, parent_fn, span)
}

/// Core closure compilation shared by block closures and short closures.
fn compile_closure_core<'ctx>(
    c: &mut Compiler<'ctx>,
    params: &[ClosureParam],
    ret_type: Type,
    body: &[Statement],
    _parent_fn: FunctionValue<'ctx>,
    span: Span,
) -> ExprResult<'ctx> {
    let param_types = resolve_closure_params(c, params, span);

    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

    let mut llvm_meta_params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptr_ty.into()];
    for ty in &param_types {
        if let Some(lt) = to_llvm_type(ty, c.context, &c.types.structs) {
            llvm_meta_params.push(lt.into());
        }
    }

    let fn_type = match to_llvm_type(&ret_type, c.context, &c.types.structs) {
        Some(ret_llvm) => ret_llvm.fn_type(&llvm_meta_params, false),
        None => c.context.void_type().fn_type(&llvm_meta_params, false),
    };

    let closure_name = format!("__closure_{}", c.fn_state.closure_counter);
    c.fn_state.closure_counter += 1;

    let captures = c
        .type_ctx
        .closure_info
        .get(&span)
        .map(|ci| ci.captures.clone());

    // Read captured values from parent scope before saving variables
    let captured_values: Vec<(String, inkwell::values::BasicValueEnum<'ctx>, Type)> =
        if let Some(ref caps) = captures {
            caps.iter()
                .filter_map(|cap| {
                    let (ptr, ty, _) = c.fn_state.variables.get(&cap.name)?;
                    let llvm_ty = to_llvm_type(ty, c.context, &c.types.structs)?;
                    let val = c.builder.build_load(llvm_ty, *ptr, &cap.name).unwrap();
                    Some((cap.name.clone(), val, ty.clone()))
                })
                .collect()
        } else {
            Vec::new()
        };

    let closure_fn = c.module.add_function(&closure_name, fn_type, None);
    let entry = c.context.append_basic_block(closure_fn, "entry");

    let saved_vars = std::mem::take(&mut c.fn_state.variables);
    let saved_block = c.builder.get_insert_block();
    let saved_subst = {
        let mut extra = HashMap::<String, Type>::new();
        if let Type::Named {
            identifier,
            type_args,
        } = &ret_type
            && !type_args.is_empty()
        {
            let type_params = c.type_ctx.get_type(identifier).map(|ti| &ti.type_params);
            if let Some(tps) = type_params {
                for (tp, ta) in tps.iter().zip(type_args.iter()) {
                    extra.insert(tp.name.clone(), ta.clone());
                }
            }
        }
        if extra.is_empty() {
            None
        } else {
            let mut merged = c.fn_state.type_subst.clone();
            merged.extend(extra);
            Some(std::mem::replace(&mut c.fn_state.type_subst, merged))
        }
    };

    c.builder.position_at_end(entry);

    // Bind user params (offset by 1 for the env_ptr param)
    for (i, param) in params.iter().enumerate() {
        if let ClosureParam::Name { name, .. } = param {
            let ty = &param_types[i];
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.types.structs) {
                let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
                let param_val = closure_fn.get_nth_param((i + 1) as u32).unwrap();
                c.builder.build_store(alloca, param_val).unwrap();
                c.fn_state
                    .variables
                    .insert(name.clone(), (alloca, ty.clone(), Ownership::Unowned));
            }
        }
    }

    // Load captured variables from the env struct into local allocas
    if !captured_values.is_empty() {
        let env_ptr = closure_fn.get_nth_param(0).unwrap().into_pointer_value();
        let env_field_types: Vec<inkwell::types::BasicTypeEnum> = captured_values
            .iter()
            .filter_map(|(_, _, ty)| to_llvm_type(ty, c.context, &c.types.structs))
            .collect();
        let env_struct_ty = c.context.struct_type(&env_field_types, false);

        for (i, (name, _, ty)) in captured_values.iter().enumerate() {
            if let Some(llvm_ty) = to_llvm_type(ty, c.context, &c.types.structs) {
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
                c.fn_state
                    .variables
                    .insert(name.clone(), (alloca, ty.clone(), Ownership::Unowned));
            }
        }
    }

    let last_tv = crate::control::compile_body_as_value(c, body, closure_fn)?;
    if !c.current_block_terminated() {
        match last_tv {
            Some(tv) => c.builder.build_return(Some(&tv.value)).unwrap(),
            None => c.builder.build_return(None).unwrap(),
        };
    }

    c.fn_state.variables = saved_vars;
    if let Some(old) = saved_subst {
        c.fn_state.type_subst = old;
    }
    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }

    // Build the env_ptr: malloc + store captures, or null for non-capturing
    let env_ptr_val = if !captured_values.is_empty() {
        let env_field_types: Vec<inkwell::types::BasicTypeEnum> = captured_values
            .iter()
            .filter_map(|(_, _, ty)| to_llvm_type(ty, c.context, &c.types.structs))
            .collect();
        let env_struct_ty = c.context.struct_type(&env_field_types, false);
        let env_size = env_struct_ty.size_of().unwrap();

        let malloc = *c
            .functions
            .get("malloc")
            .expect("malloc not declared in builtins");
        let raw_ptr = c
            .call(malloc, &[env_size.into()], "env_alloc")
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

    let closure_type = Type::Function {
        params: param_types.iter().cloned().map(FnParam::borrow).collect(),
        return_type: Box::new(ret_type),
    };
    Ok(Some(TypedValue::new(fat_ptr.into(), closure_type)))
}

fn compile_list_literal<'ctx>(
    c: &mut Compiler<'ctx>,
    elements: &[Expr],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let compiled: Vec<TypedValue<'ctx>> = elements
        .iter()
        .map(|e| {
            compile_expr(c, e, function)
                .and_then(|v| v.ok_or("list element produced no value".into()))
        })
        .collect::<Result<_, _>>()?;

    let elem_type = if let Some(subst) = c.fn_state.type_subst.get("T") {
        subst.clone()
    } else if let Some(first) = compiled.first() {
        first.expo_type.clone()
    } else {
        Type::Primitive(Primitive::I32)
    };
    let type_args = vec![elem_type.clone()];
    let mangled_type = mangle_name("List", &type_args);

    if !c.types.structs.contains_key(&mangled_type) {
        monomorphize_struct(c, "List", &type_args)?;
    }

    let new_fn_name = format!("{mangled_type}_new");
    if !c.functions.contains_key(&new_fn_name) {
        monomorphize_impl_method(c, "List", "new", &type_args, &[])?;
    }
    let append_fn_name = format!("{mangled_type}_append");
    if !c.functions.contains_key(&append_fn_name) {
        monomorphize_impl_method(c, "List", "append", &type_args, &[])?;
    }

    let new_fn = *c.functions.get(&new_fn_name).ok_or("List.new not found")?;
    let append_fn = *c
        .functions
        .get(&append_fn_name)
        .ok_or("List.append not found")?;

    let mut list_val = c
        .call(new_fn, &[], "list_new")
        .ok_or("List.new returned void")?;

    for elem in &compiled {
        let coerced = coerce_numeric(c, elem.value, &elem_type);
        list_val = c
            .call(append_fn, &[list_val.into(), coerced.into()], "list_append")
            .ok_or("List.append returned void")?;
    }

    let list_type = named_generic("List", vec![elem_type], c.type_ctx);
    Ok(Some(TypedValue::new(list_val, list_type)))
}

fn compile_map_literal<'ctx>(
    c: &mut Compiler<'ctx>,
    entries: &[(Expr, Expr)],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let (key_type, val_type) = if let (Some(k_subst), Some(v_subst)) = (
        c.fn_state.type_subst.get("K"),
        c.fn_state.type_subst.get("V"),
    ) {
        (k_subst.clone(), v_subst.clone())
    } else if let Some((first_k, first_v)) = entries.first() {
        let k_tv = compile_expr(c, first_k, function)?.ok_or("map key produced no value")?;
        let v_tv = compile_expr(c, first_v, function)?.ok_or("map value produced no value")?;
        (k_tv.expo_type, v_tv.expo_type)
    } else {
        return Err("empty map literal requires a type annotation".to_string());
    };

    let type_args = vec![key_type.clone(), val_type.clone()];
    let mangled_type = mangle_name("Map", &type_args);

    if !c.types.structs.contains_key(&mangled_type) {
        monomorphize_struct(c, "Map", &type_args)?;
    }

    let new_fn_name = format!("{mangled_type}_new");
    if !c.functions.contains_key(&new_fn_name) {
        monomorphize_impl_method(c, "Map", "new", &type_args, &[])?;
    }
    let put_fn_name = format!("{mangled_type}_put");
    if !c.functions.contains_key(&put_fn_name) {
        monomorphize_impl_method(c, "Map", "put", &type_args, &[])?;
    }

    let new_fn = *c.functions.get(&new_fn_name).ok_or("Map.new not found")?;
    let put_fn = *c.functions.get(&put_fn_name).ok_or("Map.put not found")?;

    let mut map_val = c
        .call(new_fn, &[], "map_new")
        .ok_or("Map.new returned void")?;

    for (key_expr, val_expr) in entries {
        let key = compile_expr(c, key_expr, function)?
            .ok_or("map key produced no value")?
            .value;
        let val = compile_expr(c, val_expr, function)?
            .ok_or("map value produced no value")?
            .value;
        let key = coerce_numeric(c, key, &key_type);
        let val = coerce_numeric(c, val, &val_type);
        map_val = c
            .call(put_fn, &[map_val.into(), key.into(), val.into()], "map_put")
            .ok_or("Map.put returned void")?;
    }

    let map_type = named_generic("Map", vec![key_type, val_type], c.type_ctx);
    Ok(Some(TypedValue::new(map_val, map_type)))
}

/// Compiles a `spawn T.start(config)` expression.
///
/// Delegates to [`crate::spawn`] helpers for each phase: AST extraction,
/// config serialization, wrapper generation, and `Ref<M, R>` construction.
fn compile_spawn<'ctx>(
    c: &mut Compiler<'ctx>,
    expr: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    use crate::spawn;

    let target = spawn::extract_spawn_target(expr)?;

    let config_expr = target
        .config_args
        .first()
        .map(|a| &a.value)
        .ok_or("spawn T.start(config) requires a config argument")?;
    let config_value = compile_expr(c, config_expr, function)?
        .ok_or_else(|| {
            format!(
                "{}.start() config argument produced no value",
                target.type_name
            )
        })?
        .value;

    let serialized = spawn::serialize_config(c, config_value)?;
    let mangled_state = spawn::resolve_mangled_state(&target.type_name, config_value);

    if let Some((base, type_args)) = crate::generics::try_parse_mangled_name(&mangled_state, c) {
        monomorphize_impl_method(c, &base, "start", &type_args, &[])?;
        monomorphize_impl_method(c, &base, "run", &type_args, &[])?;
    }

    let start_fn_name = format!("{mangled_state}_start");
    let start_fn = c
        .module
        .get_function(&start_fn_name)
        .ok_or_else(|| format!("undefined start function: {start_fn_name}"))?;

    let run_fn_name = format!("{mangled_state}_run");
    let run_fn = c
        .module
        .get_function(&run_fn_name)
        .ok_or_else(|| format!("undefined run function: {run_fn_name}"))?;

    let state_struct_type = c
        .types
        .structs
        .get(&mangled_state)
        .copied()
        .ok_or_else(|| format!("no LLVM struct for `{mangled_state}`"))?;

    let wrapper_name = format!("__spawn_{mangled_state}");
    let wrapper = if let Some(existing) = c.module.get_function(&wrapper_name) {
        existing
    } else {
        spawn::build_spawn_wrapper(
            c,
            &wrapper_name,
            serialized.llvm_type,
            state_struct_type,
            start_fn,
            run_fn,
            None,
        )?
    };

    let wrapper_ptr = wrapper.as_global_value().as_pointer_value();
    let spawn_fn = *c
        .functions
        .get("expo_rt_spawn")
        .ok_or("expo_rt_spawn not declared")?;

    let pid = c
        .call(
            spawn_fn,
            &[
                wrapper_ptr.into(),
                serialized.ptr.into(),
                serialized.size.into(),
            ],
            "spawn_pid",
        )
        .ok_or("expo_rt_spawn did not return a value")?
        .into_int_value();

    let (msg_type, reply_type) =
        spawn::resolve_process_msg_reply(c, &target.type_name, &mangled_state)?;

    spawn::build_ref_value(c, pid, msg_type, reply_type).map(Some)
}

fn compile_receive<'ctx>(
    c: &mut Compiler<'ctx>,
    arms: &[MatchArm],
    after_timeout: Option<&Expr>,
    after_body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let has_after = after_timeout.is_some();

    let raw_ptr = if let Some(timeout_expr) = after_timeout {
        let receive_timeout_fn = *c
            .functions
            .get("expo_rt_receive_timeout")
            .ok_or("expo_rt_receive_timeout not declared")?;

        let timeout_val = compile_expr(c, timeout_expr, function)?
            .ok_or("after timeout expression produced no value")?
            .value;

        c.call(receive_timeout_fn, &[timeout_val.into()], "receive_msg")
            .ok_or("expo_rt_receive_timeout did not return a value")?
    } else {
        let receive_fn = *c
            .functions
            .get("expo_rt_receive")
            .ok_or("expo_rt_receive not declared")?;

        c.call(receive_fn, &[], "receive_msg")
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
            c.builder.build_unreachable().unwrap();

            c.builder.position_at_end(got_msg_bb);
        }
    }

    let msg_type = c.fn_state.process_msg_type.clone();
    let is_process = msg_type.is_some()
        && !matches!(
            msg_type.as_ref().unwrap(),
            Type::Primitive(Primitive::String)
        );

    if is_process {
        return compile_receive_tagged(c, arms, raw_ptr, merge_bb, function);
    }

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
                    &[c.context.i64_type().const_int(16, false)],
                    "recv_str_payload",
                )
                .unwrap()
        };
        payload.into()
    } else {
        let msg_ty = msg_type.unwrap();
        let llvm_ty = crate::types::to_llvm_type(&msg_ty, c.context, &c.types.structs)
            .ok_or_else(|| format!("no LLVM type for receive message `{msg_ty:?}`"))?;
        let i8_type = c.context.i8_type();
        let payload_ptr = unsafe {
            c.builder
                .build_in_bounds_gep(
                    i8_type,
                    raw_ptr.into_pointer_value(),
                    &[c.context.i64_type().const_int(8, false)],
                    "recv_payload_ptr",
                )
                .unwrap()
        };
        c.builder
            .build_load(llvm_ty, payload_ptr, "msg_val")
            .unwrap()
    };

    if arms.is_empty() {
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }
        c.builder.position_at_end(merge_bb);
        let ty = c
            .fn_state
            .process_msg_type
            .clone()
            .unwrap_or(Type::Primitive(Primitive::String));
        return Ok(Some(TypedValue::new(subject_val, ty)));
    }

    let subject_type = c.fn_state.process_msg_type.clone().unwrap_or(Type::Unknown);

    let subject_alloca = c
        .builder
        .build_alloca(subject_val.get_type(), "recv_subject")
        .unwrap();
    c.builder.build_store(subject_alloca, subject_val).unwrap();

    let fallthrough_bb = c.context.append_basic_block(function, "recv_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();
    let mut reachable_arm_count = 0usize;

    let arm_refs: Vec<&MatchArm> = arms.iter().collect();
    let arm_ctx = ReceiveArmCtx {
        subject_alloca,
        subject_type: &subject_type,
        merge_bb,
        fallthrough_bb,
        prefix: "recv",
        function,
    };
    compile_receive_arms(
        c,
        &arm_refs,
        &arm_ctx,
        &mut incoming,
        &mut reachable_arm_count,
    )?;

    c.builder.position_at_end(merge_bb);
    build_receive_phi(
        c,
        &mut incoming,
        reachable_arm_count,
        &[fallthrough_bb],
        &subject_type,
    )
}

/// Control-flow context for compiling a set of receive arms.
struct ReceiveArmCtx<'a, 'ctx> {
    subject_alloca: inkwell::values::PointerValue<'ctx>,
    subject_type: &'a Type,
    merge_bb: inkwell::basic_block::BasicBlock<'ctx>,
    fallthrough_bb: inkwell::basic_block::BasicBlock<'ctx>,
    prefix: &'a str,
    function: FunctionValue<'ctx>,
}

/// Compiles a slice of receive arms against a loaded subject value.
/// Shared by both the plain and tagged receive paths. Appends to
/// `incoming` and `reachable_arm_count` for the caller's phi node.
fn compile_receive_arms<'ctx>(
    c: &mut Compiler<'ctx>,
    arms: &[&MatchArm],
    ctx: &ReceiveArmCtx<'_, 'ctx>,
    incoming: &mut Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)>,
    reachable_arm_count: &mut usize,
) -> Result<(), String> {
    for (i, arm) in arms.iter().enumerate() {
        let body_bb = c
            .context
            .append_basic_block(ctx.function, &format!("{}_body_{i}", ctx.prefix));
        let next_bb = if i + 1 < arms.len() {
            c.context
                .append_basic_block(ctx.function, &format!("{}_test_{}", ctx.prefix, i + 1))
        } else {
            ctx.fallthrough_bb
        };

        let saved_vars = c.fn_state.variables.clone();

        let condition = crate::control::compile_pattern(
            c,
            &arm.pattern,
            ctx.subject_alloca,
            ctx.subject_type,
            ctx.function,
        )?;

        let final_cond = if let Some(guard) = &arm.guard {
            let guard_val = compile_expr(c, guard, ctx.function)?
                .ok_or("receive guard produced no value")?
                .value;
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
        let arm_tv = crate::control::compile_body_as_value(c, &arm.body, ctx.function)?;
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(ctx.merge_bb).unwrap();
            *reachable_arm_count += 1;
        }
        let arm_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(tv) = arm_tv {
            incoming.push((tv.value, arm_end_bb));
        }

        c.fn_state.variables = saved_vars;
        c.builder.position_at_end(next_bb);
    }

    c.builder.position_at_end(ctx.fallthrough_bb);
    c.builder.build_unconditional_branch(ctx.merge_bb).unwrap();
    Ok(())
}

/// Builds a phi node from `incoming` values if they all share the same
/// LLVM type, adding zero-valued entries for each fallthrough block.
fn build_receive_phi<'ctx>(
    c: &Compiler<'ctx>,
    incoming: &mut Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)>,
    reachable_arm_count: usize,
    fallthrough_bbs: &[inkwell::basic_block::BasicBlock<'ctx>],
    result_type: &Type,
) -> ExprResult<'ctx> {
    if !incoming.is_empty() && incoming.len() == reachable_arm_count {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let undef = first_ty.const_zero();
            for &bb in fallthrough_bbs {
                incoming.push((undef, bb));
            }
            let phi = c.builder.build_phi(first_ty, "recv_result").unwrap();
            let refs: Vec<_> = incoming
                .iter()
                .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
                .collect();
            phi.add_incoming(&refs);
            return Ok(Some(TypedValue::new(
                phi.as_basic_value(),
                result_type.clone(),
            )));
        }
    }
    Ok(None)
}

/// Compiles a `receive` expression in a Process context where the mailbox
/// uses tagged messages. The raw buffer layout is [tag: 8 bytes, payload].
/// Tag 0 = business message (Pair<M, Option<ReplyTo<R>>>), tag 1 = Lifecycle.
///
/// Arms are partitioned by their TypedBinding type annotation: arms whose
/// resolved type matches `Lifecycle` go to the lifecycle branch, all others
/// go to the business branch.
fn compile_receive_tagged<'ctx>(
    c: &mut Compiler<'ctx>,
    arms: &[MatchArm],
    raw_ptr: BasicValueEnum<'ctx>,
    merge_bb: inkwell::basic_block::BasicBlock<'ctx>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    use expo_ast::ast::Pattern;

    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let envelope_type = c.fn_state.process_msg_type.clone().unwrap();

    let tag_val = c
        .builder
        .build_load(i8_type, raw_ptr.into_pointer_value(), "recv_tag")
        .unwrap()
        .into_int_value();

    let payload_ptr = unsafe {
        c.builder
            .build_in_bounds_gep(
                i8_type,
                raw_ptr.into_pointer_value(),
                &[i64_type.const_int(8, false)],
                "recv_payload",
            )
            .unwrap()
    };

    let mut business_arms: Vec<&MatchArm> = Vec::new();
    let mut lifecycle_arms: Vec<&MatchArm> = Vec::new();

    for arm in arms {
        if let Pattern::TypedBinding { type_expr, .. } = &arm.pattern {
            let resolved = c.resolve_type_expr(type_expr);
            if matches!(&resolved, Type::Named { identifier, type_args } if identifier.name == "Lifecycle" && type_args.is_empty())
            {
                lifecycle_arms.push(arm);
                continue;
            }
        }
        business_arms.push(arm);
    }

    let has_lifecycle = !lifecycle_arms.is_empty();

    let biz_bb = c.context.append_basic_block(function, "recv_tag_business");
    let lc_bb = if has_lifecycle {
        c.context.append_basic_block(function, "recv_tag_lifecycle")
    } else {
        merge_bb
    };
    let default_bb = c.context.append_basic_block(function, "recv_tag_default");

    c.builder
        .build_switch(
            tag_val,
            default_bb,
            &[
                (i8_type.const_int(0, false), biz_bb),
                (i8_type.const_int(1, false), lc_bb),
            ],
        )
        .unwrap();

    c.builder.position_at_end(default_bb);
    c.builder.build_unreachable().unwrap();

    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();
    let mut reachable_arm_count = 0usize;
    let mut fallthrough_bbs: Vec<inkwell::basic_block::BasicBlock<'ctx>> = Vec::new();

    // --- Business arms (tag 0) ---
    c.builder.position_at_end(biz_bb);
    let env_llvm = crate::types::to_llvm_type(&envelope_type, c.context, &c.types.structs)
        .ok_or_else(|| format!("no LLVM type for envelope `{}`", envelope_type.display()))?;
    let biz_val = c
        .builder
        .build_load(env_llvm, payload_ptr, "biz_msg")
        .unwrap();
    let biz_alloca = c
        .builder
        .build_alloca(biz_val.get_type(), "biz_subject")
        .unwrap();
    c.builder.build_store(biz_alloca, biz_val).unwrap();

    let biz_fallthrough = c.context.append_basic_block(function, "recv_biz_none");
    let biz_ctx = ReceiveArmCtx {
        subject_alloca: biz_alloca,
        subject_type: &envelope_type,
        merge_bb,
        fallthrough_bb: biz_fallthrough,
        prefix: "recv_biz",
        function,
    };
    compile_receive_arms(
        c,
        &business_arms,
        &biz_ctx,
        &mut incoming,
        &mut reachable_arm_count,
    )?;
    fallthrough_bbs.push(biz_fallthrough);

    // --- Lifecycle arms (tag 1) ---
    if has_lifecycle {
        c.builder.position_at_end(lc_bb);

        let lifecycle_type = named("Lifecycle");
        let lc_llvm = crate::types::to_llvm_type(&lifecycle_type, c.context, &c.types.structs)
            .ok_or("no LLVM type for Lifecycle enum")?;
        let lc_val = c
            .builder
            .build_load(lc_llvm, payload_ptr, "lc_msg")
            .unwrap();
        let lc_alloca = c
            .builder
            .build_alloca(lc_val.get_type(), "lc_subject")
            .unwrap();
        c.builder.build_store(lc_alloca, lc_val).unwrap();

        let lc_fallthrough = c.context.append_basic_block(function, "recv_lc_none");
        let lc_ctx = ReceiveArmCtx {
            subject_alloca: lc_alloca,
            subject_type: &lifecycle_type,
            merge_bb,
            fallthrough_bb: lc_fallthrough,
            prefix: "recv_lc",
            function,
        };
        compile_receive_arms(
            c,
            &lifecycle_arms,
            &lc_ctx,
            &mut incoming,
            &mut reachable_arm_count,
        )?;
        fallthrough_bbs.push(lc_fallthrough);
    }

    c.builder.position_at_end(merge_bb);
    let result_type = c.fn_state.process_msg_type.clone().unwrap_or(Type::Unknown);
    build_receive_phi(
        c,
        &mut incoming,
        reachable_arm_count,
        &fallthrough_bbs,
        &result_type,
    )
}
