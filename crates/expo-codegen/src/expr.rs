//! Expression compilation: translates Expo expressions (literals, variables,
//! binary/unary ops, calls, closures, string interpolation, etc.) into LLVM IR.

use expo_ast::ast::{
    ClosureParam, Expr, ExprKind, Literal, MatchArm, Pattern, Statement, StringPart, TypeExpr,
};
use expo_ast::span::Span;

use expo_ast::identifier::TypeIdentifier;
use expo_ast::types::{named_generic_std, named_std, type_identifier};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::{Primitive, Type, mangle_name};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, PointerValue};
use std::collections::HashMap;

use crate::binary::construction::compile_binary_literal;
use crate::calls::compile_call;
use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::control::{
    compile_body_as_value, compile_cond, compile_for, compile_if, compile_loop, compile_match,
    compile_pattern, compile_ternary, compile_unless, compile_while,
};
use expo_ir::resolved::closures::ResolvedClosure;
use expo_ir::resolved::strings::{ResolvedString, resolve_string};

use crate::debug::call_format;
use crate::drop::Ownership;
use crate::enums::compile_enum_construction;
use crate::generics::{monomorphize_impl_method, monomorphize_struct, try_parse_mangled_name};
use crate::ops::{compile_binary, compile_unary};
use crate::spawn;
use crate::stmt::{apply_coercion, coerce_numeric, compile_statement, compile_union_wrap};
use crate::structs::{compile_field_access, compile_method_call, compile_struct_construction};
use crate::types::to_llvm_type;
use crate::util::{parse_int_literal, printf_format_spec};

/// Compiles an expression and coerces the result to the expected type.
/// Use when the target type is known (e.g. function arguments, struct fields).
pub fn compile_expr_coerced<'ctx>(
    compiler: &mut Compiler<'ctx>,
    expression: &Expr,
    expected: &Type,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match compile_expr(compiler, expression, function)? {
        Some(typed_value) => {
            let value = coerce_numeric(compiler, typed_value.value, expected);
            let value = apply_coercion(compiler, value, expression)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// Top-level expression dispatch. Matches each AST expression variant and
/// delegates to the appropriate specialized compiler function.
pub fn compile_expr<'ctx>(
    compiler: &mut Compiler<'ctx>,
    expr: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    match &expr.kind {
        ExprKind::Literal { value, .. } => compile_literal(compiler, value),

        ExprKind::Ident { name, .. } => {
            if let Some((ptr, ty, _)) = compiler.fn_state.variables.get(name) {
                let ty = ty.clone();
                let llvm_ty = to_llvm_type(&ty, compiler.context, &compiler.types)
                    .ok_or_else(|| {
                        let fn_name = compiler.fn_state.self_type_name.as_deref().unwrap_or("(top)");
                        format!("cannot load variable of unsupported type: {name} (type: {ty:?}, in fn {fn_name})")
                    })?;
                let value = compiler.builder.build_load(llvm_ty, *ptr, name).unwrap();
                return Ok(Some(TypedValue::new(value, ty)));
            }

            if let Some(constant) = compiler.constants.get(name) {
                let ty = compiler
                    .type_ctx
                    .constants
                    .get(name)
                    .cloned()
                    .unwrap_or(Type::Unknown);
                return Ok(Some(TypedValue::new(*constant, ty)));
            }

            if compiler.module.get_function(name).is_some() {
                let thunk = compiler.get_or_create_thunk(name)?;
                let ptr_ty = compiler.context.ptr_type(AddressSpace::default());
                let closure_struct_ty = compiler
                    .context
                    .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
                let thunk_ptr = thunk.as_global_value().as_pointer_value();
                let null_env = ptr_ty.const_null();
                let mut fat_ptr = closure_struct_ty.get_undef();
                fat_ptr = compiler
                    .builder
                    .build_insert_value(fat_ptr, thunk_ptr, 0, "insert_fn")
                    .unwrap()
                    .into_struct_value();
                fat_ptr = compiler
                    .builder
                    .build_insert_value(fat_ptr, null_env, 1, "insert_env")
                    .unwrap()
                    .into_struct_value();
                let fn_type = compiler
                    .type_ctx
                    .functions
                    .get(name)
                    .map(|sig| Type::Function {
                        params: sig.params.iter().map(FnParam::from).collect(),
                        return_type: Box::new(sig.return_type.clone()),
                    })
                    .unwrap_or(Type::Unknown);
                return Ok(Some(TypedValue::new(fat_ptr.into(), fn_type)));
            }

            Err(format!("undefined variable: {name}"))
        }

        ExprKind::Group { expr: inner, .. } => compile_expr(compiler, inner, function),

        ExprKind::Binary {
            op, left, right, ..
        } => compile_binary(compiler, op, left, right, function),

        ExprKind::Unary { op, operand, .. } => compile_unary(compiler, op, operand, function),

        ExprKind::Call { callee, args, .. } => {
            if let ExprKind::Ident { name, .. } = &callee.kind {
                compile_call(compiler, name, args, function)
            } else {
                Err("only named function calls are supported".to_string())
            }
        }

        ExprKind::If {
            condition,
            then_body,
            else_body,
            ..
        } => compile_if(compiler, condition, then_body, else_body, function),

        ExprKind::StructConstruction {
            type_path, fields, ..
        } => {
            let resolved_id = expr.resolved_type.as_ref().and_then(type_identifier);
            compile_struct_construction(compiler, type_path, fields, resolved_id, function)
        }

        ExprKind::FieldAccess {
            receiver, field, ..
        } => compile_field_access(compiler, receiver, field, function),

        ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => compile_method_call(compiler, receiver, method, args, function),

        ExprKind::String { parts, .. } => compile_string(compiler, parts, function),

        ExprKind::Loop { body, .. } => compile_loop(compiler, body, function),

        ExprKind::While {
            condition, body, ..
        } => compile_while(compiler, condition, body, function),

        ExprKind::Self_ => {
            let (ptr, ty, _) = compiler
                .fn_state
                .variables
                .get("self")
                .ok_or("self used outside of impl method")?;
            let ty = ty.clone();
            let llvm_ty = to_llvm_type(&ty, compiler.context, &compiler.types)
                .ok_or("cannot load self of unsupported type")?;
            let value = compiler.builder.build_load(llvm_ty, *ptr, "self").unwrap();
            Ok(Some(TypedValue::new(value, ty)))
        }

        ExprKind::Cond {
            arms, else_body, ..
        } => compile_cond(compiler, arms, else_body, function),

        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
            ..
        } => {
            let resolved_id = expr.resolved_type.as_ref().and_then(type_identifier);
            compile_enum_construction(compiler, type_path, variant, data, resolved_id, function)
        }

        ExprKind::Match { subject, arms, .. } => compile_match(compiler, subject, arms, function),

        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => compile_ternary(compiler, condition, then_expr, else_expr, function),

        ExprKind::Closure {
            params,
            return_type,
            body,
        } => compile_closure(compiler, params, return_type, body, function, expr.span),

        ExprKind::ShortClosure { params, body } => {
            let body_stmts = vec![Statement::Expr((**body).clone())];
            let ret_type = compiler
                .closure_info_at(expr.span)
                .and_then(|ci| ci.return_type.clone())
                .unwrap_or(Type::Unit);
            compile_closure_core(compiler, params, ret_type, &body_stmts, function, expr.span)
        }

        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => compile_for(compiler, pattern, iterable, body, function),

        ExprKind::Unless {
            condition, body, ..
        } => compile_unless(compiler, condition, body, function),

        ExprKind::List { elements, .. } => compile_list_literal(compiler, elements, function),

        ExprKind::Map { entries, .. } => compile_map_literal(compiler, entries, function),

        ExprKind::BinaryLiteral { segments, .. } => {
            compile_binary_literal(compiler, segments, function)
        }

        ExprKind::Spawn {
            expr: spawn_expr, ..
        } => compile_spawn(compiler, spawn_expr, function),

        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
            ..
        } => compile_receive(
            compiler,
            arms,
            after_timeout.as_deref(),
            after_body,
            function,
        ),

        _ => Err(format!(
            "not yet supported in compilation: {:?}",
            std::mem::discriminant(&expr.kind)
        )),
    }
}

fn compile_literal<'ctx>(compiler: &Compiler<'ctx>, literal: &Literal) -> ExprResult<'ctx> {
    match literal {
        Literal::Int(text) => {
            let value = parse_int_literal(text)?;
            Ok(Some(TypedValue::new(
                compiler
                    .context
                    .i64_type()
                    .const_int(value as u64, true)
                    .into(),
                Type::Primitive(Primitive::I64),
            )))
        }
        Literal::Float(text) => {
            let value: f64 = text.parse().map_err(|_| format!("invalid float: {text}"))?;
            Ok(Some(TypedValue::new(
                compiler.context.f64_type().const_float(value).into(),
                Type::Primitive(Primitive::F64),
            )))
        }
        Literal::Bool(value) => Ok(Some(TypedValue::new(
            compiler
                .context
                .bool_type()
                .const_int(if *value { 1 } else { 0 }, false)
                .into(),
            Type::Primitive(Primitive::Bool),
        ))),
        Literal::String(_) => {
            unreachable!("string literals use ExprKind::String, not ExprKind::Literal")
        }
        Literal::Unit => Ok(None),
    }
}

fn compile_string<'ctx>(
    compiler: &mut Compiler<'ctx>,
    parts: &[StringPart],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_string(parts);

    if let ResolvedString::Literal { value } = resolved {
        let payload_ptr = compiler.create_string_global(value.as_bytes(), "str");
        return Ok(Some(TypedValue::new(
            payload_ptr.into(),
            Type::Primitive(Primitive::String),
        )));
    }

    let snprintf = *compiler
        .functions
        .get("snprintf")
        .ok_or("snprintf not declared")?;

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
                let typed_value = compile_expr(compiler, expr, function)?
                    .ok_or("interpolated expression produced no value")?;
                let value = typed_value.value;

                let is_bool =
                    value.is_int_value() && value.into_int_value().get_type().get_bit_width() == 1;
                let is_plain_printf =
                    !value.is_struct_value() && !is_bool && printf_format_spec(&value).is_ok();

                if is_plain_printf {
                    fmt_string.push_str(printf_format_spec(&value).unwrap());
                    interp_values.push(value);
                } else {
                    let str_ptr = call_format(compiler, value, &typed_value.expo_type)?;
                    fmt_string.push_str("%s");
                    interp_values.push(str_ptr.into());
                }
            }
        }
    }

    let fmt_global = compiler
        .builder
        .build_global_string_ptr(&fmt_string, "interp_fmt")
        .unwrap();
    let fmt_ptr = fmt_global.as_pointer_value();

    let i32_type = compiler.context.i32_type();
    let ptr_type = compiler.context.ptr_type(AddressSpace::default());
    let null_ptr = ptr_type.const_null();
    let zero = i32_type.const_int(0, false);

    let mut size_args: Vec<BasicValueEnum> = vec![null_ptr.into(), zero.into(), fmt_ptr.into()];
    size_args.extend_from_slice(&interp_values);
    let size_args_meta: Vec<_> = size_args.iter().map(|v| (*v).into()).collect();

    let needed = compiler
        .call(snprintf, &size_args_meta, "interp_len")
        .ok_or("snprintf did not return a value")?
        .into_int_value();

    let one = i32_type.const_int(1, false);
    let buf_size = compiler
        .builder
        .build_int_add(needed, one, "buf_size")
        .unwrap();

    let malloc_fn = *compiler
        .functions
        .get("malloc")
        .ok_or("malloc not declared")?;
    let i64_type = compiler.context.i64_type();
    let i8_type = compiler.context.i8_type();
    let needed_i64 = compiler
        .builder
        .build_int_z_extend(needed, i64_type, "needed_i64")
        .unwrap();
    let alloc_size = compiler
        .builder
        .build_int_add(needed_i64, i64_type.const_int(9, false), "interp_alloc_sz")
        .unwrap();
    let base_ptr = compiler
        .call(malloc_fn, &[alloc_size.into()], "interp_base")
        .ok_or("malloc did not return a value")?
        .into_pointer_value();

    let bit_length = compiler
        .builder
        .build_int_mul(needed_i64, i64_type.const_int(8, false), "bit_length")
        .unwrap();
    compiler.builder.build_store(base_ptr, bit_length).unwrap();

    let payload = unsafe {
        compiler
            .builder
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

    compiler.call_void(snprintf, &write_args_meta, "interp_write");

    Ok(Some(TypedValue::new(
        payload.into(),
        Type::Primitive(Primitive::String),
    )))
}

fn resolve_closure_params<'ctx>(
    compiler: &Compiler<'ctx>,
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
                    type_expr: Some(type_expr),
                    ..
                } => compiler.resolve_type_expr(type_expr),
                _ => unreachable!(),
            })
            .collect();
    }

    if let Some(closure_info) = compiler.closure_info_at(span) {
        return closure_info.param_types.clone();
    }

    params
        .iter()
        .map(|p| match p {
            ClosureParam::Name {
                type_expr: Some(type_expr),
                ..
            } => compiler.resolve_type_expr(type_expr),
            _ => Type::Primitive(Primitive::I32),
        })
        .collect()
}

fn resolve_closure(
    compiler: &mut Compiler,
    params: &[ClosureParam],
    return_type: Type,
    span: Span,
) -> ResolvedClosure {
    let parameter_types = resolve_closure_params(compiler, params, span);

    let closure_name = format!("__closure_{}", compiler.fn_state.closure_counter);
    compiler.fn_state.closure_counter += 1;

    let capture_names = compiler
        .closure_info_at(span)
        .map(|ci| ci.captures.iter().map(|cap| cap.name.clone()).collect())
        .unwrap_or_default();

    ResolvedClosure {
        capture_names,
        closure_name,
        parameter_types,
        return_type,
    }
}

/// Compiles a block closure (`fn (params) -> type ... end`) into an anonymous
/// LLVM function and returns a fat pointer `{ fn_ptr, env_ptr }`. Every closure
/// function receives an implicit `env_ptr: ptr` as its first parameter.
fn compile_closure<'ctx>(
    compiler: &mut Compiler<'ctx>,
    params: &[ClosureParam],
    return_type: &Option<TypeExpr>,
    body: &[Statement],
    parent_function: FunctionValue<'ctx>,
    span: Span,
) -> ExprResult<'ctx> {
    let ret_type = match return_type {
        Some(type_expr) => compiler.resolve_type_expr(type_expr),
        None => Type::Unit,
    };
    compile_closure_core(compiler, params, ret_type, body, parent_function, span)
}

/// Core closure compilation shared by block closures and short closures.
fn compile_closure_core<'ctx>(
    compiler: &mut Compiler<'ctx>,
    params: &[ClosureParam],
    ret_type: Type,
    body: &[Statement],
    _parent_function: FunctionValue<'ctx>,
    span: Span,
) -> ExprResult<'ctx> {
    let resolved = resolve_closure(compiler, params, ret_type, span);

    let ptr_ty = compiler.context.ptr_type(AddressSpace::default());

    let mut llvm_meta_params: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
    for ty in &resolved.parameter_types {
        if let Some(llvm_ty) = to_llvm_type(ty, compiler.context, &compiler.types) {
            llvm_meta_params.push(llvm_ty.into());
        }
    }

    let fn_type = match to_llvm_type(&resolved.return_type, compiler.context, &compiler.types) {
        Some(ret_llvm) => ret_llvm.fn_type(&llvm_meta_params, false),
        None => compiler
            .context
            .void_type()
            .fn_type(&llvm_meta_params, false),
    };

    let captured_values: Vec<(String, BasicValueEnum<'ctx>, Type)> = resolved
        .capture_names
        .iter()
        .filter_map(|name| {
            let (ptr, ty, _) = compiler.fn_state.variables.get(name)?;
            let llvm_ty = to_llvm_type(ty, compiler.context, &compiler.types)?;
            let value = compiler.builder.build_load(llvm_ty, *ptr, name).unwrap();
            Some((name.clone(), value, ty.clone()))
        })
        .collect();

    let closure_fn = compiler
        .module
        .add_function(&resolved.closure_name, fn_type, None);
    let entry = compiler.context.append_basic_block(closure_fn, "entry");

    let saved_vars = std::mem::take(&mut compiler.fn_state.variables);
    let saved_block = compiler.builder.get_insert_block();
    let saved_subst = {
        let mut extra = HashMap::<String, Type>::new();
        if let Type::Named {
            identifier,
            type_args,
        } = &resolved.return_type
            && !type_args.is_empty()
            && let Some(type_params) = compiler
                .type_ctx
                .get_type(identifier)
                .map(|ti| &ti.type_params)
        {
            for (type_param, type_arg) in type_params.iter().zip(type_args.iter()) {
                extra.insert(type_param.name.clone(), type_arg.clone());
            }
        }
        if extra.is_empty() {
            None
        } else {
            let mut merged = compiler.fn_state.type_subst.clone();
            merged.extend(extra);
            Some(std::mem::replace(&mut compiler.fn_state.type_subst, merged))
        }
    };

    compiler.builder.position_at_end(entry);

    for (i, param) in params.iter().enumerate() {
        if let ClosureParam::Name { name, .. } = param {
            let ty = &resolved.parameter_types[i];
            if let Some(llvm_ty) = to_llvm_type(ty, compiler.context, &compiler.types) {
                let alloca = compiler.builder.build_alloca(llvm_ty, name).unwrap();
                let param_val = closure_fn.get_nth_param((i + 1) as u32).unwrap();
                compiler.builder.build_store(alloca, param_val).unwrap();
                compiler
                    .fn_state
                    .variables
                    .insert(name.clone(), (alloca, ty.clone(), Ownership::Unowned));
            }
        }
    }

    if !captured_values.is_empty() {
        let env_ptr = closure_fn.get_nth_param(0).unwrap().into_pointer_value();
        let env_field_types: Vec<BasicTypeEnum> = captured_values
            .iter()
            .filter_map(|(_, _, ty)| to_llvm_type(ty, compiler.context, &compiler.types))
            .collect();
        let env_struct_ty = compiler.context.struct_type(&env_field_types, false);

        for (i, (name, _, ty)) in captured_values.iter().enumerate() {
            if let Some(llvm_ty) = to_llvm_type(ty, compiler.context, &compiler.types) {
                let field_ptr = compiler
                    .builder
                    .build_struct_gep(env_struct_ty, env_ptr, i as u32, &format!("cap_{name}"))
                    .unwrap();
                let value = compiler
                    .builder
                    .build_load(llvm_ty, field_ptr, &format!("load_{name}"))
                    .unwrap();
                let alloca = compiler.builder.build_alloca(llvm_ty, name).unwrap();
                compiler.builder.build_store(alloca, value).unwrap();
                compiler
                    .fn_state
                    .variables
                    .insert(name.clone(), (alloca, ty.clone(), Ownership::Unowned));
            }
        }
    }

    let last_typed_value = compile_body_as_value(compiler, body, closure_fn)?;
    if !compiler.current_block_terminated() {
        match last_typed_value {
            Some(typed_value) => compiler
                .builder
                .build_return(Some(&typed_value.value))
                .unwrap(),
            None => compiler.builder.build_return(None).unwrap(),
        };
    }

    compiler.fn_state.variables = saved_vars;
    if let Some(old) = saved_subst {
        compiler.fn_state.type_subst = old;
    }
    if let Some(block) = saved_block {
        compiler.builder.position_at_end(block);
    }

    let env_ptr_val = if !captured_values.is_empty() {
        let env_field_types: Vec<BasicTypeEnum> = captured_values
            .iter()
            .filter_map(|(_, _, ty)| to_llvm_type(ty, compiler.context, &compiler.types))
            .collect();
        let env_struct_ty = compiler.context.struct_type(&env_field_types, false);
        let env_size = env_struct_ty.size_of().unwrap();

        let malloc = *compiler
            .functions
            .get("malloc")
            .expect("malloc not declared in builtins");
        let raw_ptr = compiler
            .call(malloc, &[env_size.into()], "env_alloc")
            .unwrap()
            .into_pointer_value();

        for (i, (name, value, _)) in captured_values.iter().enumerate() {
            let field_ptr = compiler
                .builder
                .build_struct_gep(env_struct_ty, raw_ptr, i as u32, &format!("env_{name}"))
                .unwrap();
            compiler.builder.build_store(field_ptr, *value).unwrap();
        }

        raw_ptr
    } else {
        ptr_ty.const_null()
    };

    let closure_struct_ty = compiler
        .context
        .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
    let fn_ptr = closure_fn.as_global_value().as_pointer_value();
    let mut fat_ptr = closure_struct_ty.get_undef();
    fat_ptr = compiler
        .builder
        .build_insert_value(fat_ptr, fn_ptr, 0, "insert_fn")
        .unwrap()
        .into_struct_value();
    fat_ptr = compiler
        .builder
        .build_insert_value(fat_ptr, env_ptr_val, 1, "insert_env")
        .unwrap()
        .into_struct_value();

    let closure_type = Type::Function {
        params: resolved
            .parameter_types
            .iter()
            .cloned()
            .map(FnParam::borrow)
            .collect(),
        return_type: Box::new(resolved.return_type),
    };
    Ok(Some(TypedValue::new(fat_ptr.into(), closure_type)))
}

struct ResolvedListLiteral {
    element_type: Type,
    result_type: Type,
}

fn resolve_list_literal(
    compiler: &mut Compiler,
    compiled_elements: &[TypedValue],
) -> Result<ResolvedListLiteral, String> {
    let element_type = if let Some(subst) = compiler.fn_state.type_subst.get("T") {
        subst.clone()
    } else if let Some(first) = compiled_elements.first() {
        first.expo_type.clone()
    } else {
        Type::Primitive(Primitive::I32)
    };

    let type_args = vec![element_type.clone()];
    let list_id = TypeIdentifier::std("List");
    let mangled_type = mangle_name(&list_id, &type_args);

    if !compiler.types.contains_monomorphized(&mangled_type) {
        monomorphize_struct(compiler, &list_id, &type_args)?;
    }
    if !compiler
        .functions
        .contains_key(&format!("{mangled_type}_new"))
    {
        monomorphize_impl_method(compiler, "List", "new", &type_args, &[])?;
    }
    if !compiler
        .functions
        .contains_key(&format!("{mangled_type}_append"))
    {
        monomorphize_impl_method(compiler, "List", "append", &type_args, &[])?;
    }

    let result_type = named_generic_std("List", vec![element_type.clone()]);

    Ok(ResolvedListLiteral {
        element_type,
        result_type,
    })
}

fn compile_list_literal<'ctx>(
    compiler: &mut Compiler<'ctx>,
    elements: &[Expr],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let compiled: Vec<TypedValue<'ctx>> = elements
        .iter()
        .map(|element| {
            compile_expr(compiler, element, function)
                .and_then(|v| v.ok_or("list element produced no value".into()))
        })
        .collect::<Result<_, _>>()?;

    let resolved = resolve_list_literal(compiler, &compiled)?;

    let mangled_type = mangle_name(
        &TypeIdentifier::std("List"),
        std::slice::from_ref(&resolved.element_type),
    );
    let new_fn = *compiler
        .functions
        .get(&format!("{mangled_type}_new"))
        .ok_or("List.new not found")?;
    let append_fn = *compiler
        .functions
        .get(&format!("{mangled_type}_append"))
        .ok_or("List.append not found")?;

    let mut list_val = compiler
        .call(new_fn, &[], "list_new")
        .ok_or("List.new returned void")?;

    for element in &compiled {
        let coerced = coerce_numeric(compiler, element.value, &resolved.element_type);
        list_val = compiler
            .call(append_fn, &[list_val.into(), coerced.into()], "list_append")
            .ok_or("List.append returned void")?;
    }

    Ok(Some(TypedValue::new(list_val, resolved.result_type)))
}

struct ResolvedMapLiteral {
    key_type: Type,
    result_type: Type,
    value_type: Type,
}

fn resolve_map_literal(
    compiler: &mut Compiler,
    key_type: &Type,
    value_type: &Type,
) -> Result<ResolvedMapLiteral, String> {
    let type_args = vec![key_type.clone(), value_type.clone()];
    let map_id = TypeIdentifier::std("Map");
    let mangled_type = mangle_name(&map_id, &type_args);

    if !compiler.types.contains_monomorphized(&mangled_type) {
        monomorphize_struct(compiler, &map_id, &type_args)?;
    }
    if !compiler
        .functions
        .contains_key(&format!("{mangled_type}_new"))
    {
        monomorphize_impl_method(compiler, "Map", "new", &type_args, &[])?;
    }
    if !compiler
        .functions
        .contains_key(&format!("{mangled_type}_put"))
    {
        monomorphize_impl_method(compiler, "Map", "put", &type_args, &[])?;
    }

    let result_type = named_generic_std("Map", vec![key_type.clone(), value_type.clone()]);

    Ok(ResolvedMapLiteral {
        key_type: key_type.clone(),
        result_type,
        value_type: value_type.clone(),
    })
}

fn compile_map_literal<'ctx>(
    compiler: &mut Compiler<'ctx>,
    entries: &[(Expr, Expr)],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let (key_type, val_type) = if let (Some(key_subst), Some(val_subst)) = (
        compiler.fn_state.type_subst.get("K"),
        compiler.fn_state.type_subst.get("V"),
    ) {
        (key_subst.clone(), val_subst.clone())
    } else if let Some((first_key, first_val)) = entries.first() {
        let key_typed =
            compile_expr(compiler, first_key, function)?.ok_or("map key produced no value")?;
        let val_typed =
            compile_expr(compiler, first_val, function)?.ok_or("map value produced no value")?;
        (key_typed.expo_type, val_typed.expo_type)
    } else {
        return Err("empty map literal requires a type annotation".to_string());
    };

    let resolved = resolve_map_literal(compiler, &key_type, &val_type)?;

    let mangled_type = mangle_name(
        &TypeIdentifier::std("Map"),
        &[resolved.key_type.clone(), resolved.value_type.clone()],
    );
    let new_fn = *compiler
        .functions
        .get(&format!("{mangled_type}_new"))
        .ok_or("Map.new not found")?;
    let put_fn = *compiler
        .functions
        .get(&format!("{mangled_type}_put"))
        .ok_or("Map.put not found")?;

    let mut map_val = compiler
        .call(new_fn, &[], "map_new")
        .ok_or("Map.new returned void")?;

    for (key_expr, val_expr) in entries {
        let key = compile_expr(compiler, key_expr, function)?
            .ok_or("map key produced no value")?
            .value;
        let val = compile_expr(compiler, val_expr, function)?
            .ok_or("map value produced no value")?
            .value;
        let key = coerce_numeric(compiler, key, &resolved.key_type);
        let val = coerce_numeric(compiler, val, &resolved.value_type);
        map_val = compiler
            .call(put_fn, &[map_val.into(), key.into(), val.into()], "map_put")
            .ok_or("Map.put returned void")?;
    }

    Ok(Some(TypedValue::new(map_val, resolved.result_type)))
}

/// Resolved spawn metadata: mangled names and optional generic decomposition.
struct ResolvedSpawn {
    generic_args: Option<(String, Vec<Type>)>,
    mangled_state: String,
    run_fn_name: String,
    start_fn_name: String,
    wrapper_name: String,
}

/// Computes the mangled names and function identifiers for a spawn expression.
fn resolve_spawn_info<'ctx>(
    compiler: &Compiler<'ctx>,
    type_name: &str,
    config_value: BasicValueEnum<'ctx>,
) -> ResolvedSpawn {
    let mangled_state = spawn::resolve_mangled_state(type_name, config_value);
    let generic_args = try_parse_mangled_name(&mangled_state, compiler);
    // Non-generic spawns must use the package-qualified method symbol so we
    // match the prefix emitted at definition time for user packages (e.g.
    // `myapp.Counter_start`). Generic monomorphizations keep the mangled
    // state key unchanged; their method symbols continue to be bare-keyed
    // until the generics flow is migrated.
    let method_prefix = if generic_args.is_some() {
        mangled_state.clone()
    } else {
        compiler
            .type_ctx
            .resolve_name(&mangled_state)
            .map(|id| compiler.method_symbol_prefix(&id.package, &id.name))
            .unwrap_or_else(|| mangled_state.clone())
    };
    ResolvedSpawn {
        generic_args,
        run_fn_name: format!("{method_prefix}_run"),
        start_fn_name: format!("{method_prefix}_start"),
        wrapper_name: format!("__spawn_{mangled_state}"),
        mangled_state,
    }
}

/// Compiles a `spawn T.start(config)` expression.
///
/// Delegates to [`crate::spawn`] helpers for each phase: AST extraction,
/// config serialization, wrapper generation, and `Ref<M, R>` construction.
fn compile_spawn<'ctx>(
    compiler: &mut Compiler<'ctx>,
    expr: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let target = spawn::extract_spawn_target(expr)?;

    let config_expr = target
        .config_args
        .first()
        .map(|a| &a.value)
        .ok_or("spawn T.start(config) requires a config argument")?;
    let config_value = compile_expr(compiler, config_expr, function)?
        .ok_or_else(|| {
            format!(
                "{}.start() config argument produced no value",
                target.type_name
            )
        })?
        .value;

    let serialized = spawn::serialize_config(compiler, config_value)?;
    let resolved = resolve_spawn_info(compiler, &target.type_name, config_value);

    if let Some((base, type_args)) = &resolved.generic_args {
        monomorphize_impl_method(compiler, base, "start", type_args, &[])?;
        monomorphize_impl_method(compiler, base, "run", type_args, &[])?;
    }

    let start_fn = compiler
        .module
        .get_function(&resolved.start_fn_name)
        .ok_or_else(|| format!("undefined start function: {}", resolved.start_fn_name))?;

    let run_fn = compiler
        .module
        .get_function(&resolved.run_fn_name)
        .ok_or_else(|| format!("undefined run function: {}", resolved.run_fn_name))?;

    let state_struct_type = compiler
        .resolve_name_current(&resolved.mangled_state)
        .and_then(|id| compiler.types.get_concrete(id))
        .or_else(|| compiler.types.get_monomorphized(&resolved.mangled_state))
        .ok_or_else(|| format!("no LLVM struct for `{}`", resolved.mangled_state))?;

    let wrapper = if let Some(existing) = compiler.module.get_function(&resolved.wrapper_name) {
        existing
    } else {
        spawn::build_spawn_wrapper(
            compiler,
            &resolved.wrapper_name,
            serialized.llvm_type,
            state_struct_type,
            start_fn,
            run_fn,
            None,
        )?
    };

    let wrapper_ptr = wrapper.as_global_value().as_pointer_value();
    let spawn_fn = *compiler
        .functions
        .get("expo_rt_spawn")
        .ok_or("expo_rt_spawn not declared")?;

    let pid = compiler
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
        spawn::resolve_process_msg_reply(compiler, &target.type_name, &resolved.mangled_state)?;

    spawn::build_ref_value(compiler, pid, msg_type, reply_type).map(Some)
}

struct ResolvedReceive {
    envelope_type: Type,
    has_timeout: bool,
}

fn resolve_receive(
    compiler: &Compiler,
    after_timeout: Option<&Expr>,
) -> Result<ResolvedReceive, String> {
    let envelope_type = compiler
        .fn_state
        .process_msg_type
        .clone()
        .ok_or("receive requires a typed Process envelope; no message type found")?;

    Ok(ResolvedReceive {
        envelope_type,
        has_timeout: after_timeout.is_some(),
    })
}

fn compile_receive<'ctx>(
    compiler: &mut Compiler<'ctx>,
    arms: &[MatchArm],
    after_timeout: Option<&Expr>,
    after_body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_receive(compiler, after_timeout)?;

    let raw_ptr = if let Some(timeout_expr) = after_timeout {
        let receive_timeout_fn = *compiler
            .functions
            .get("expo_rt_receive_timeout")
            .ok_or("expo_rt_receive_timeout not declared")?;

        let timeout_val = compile_expr(compiler, timeout_expr, function)?
            .ok_or("after timeout expression produced no value")?
            .value;

        compiler
            .call(receive_timeout_fn, &[timeout_val.into()], "receive_msg")
            .ok_or("expo_rt_receive_timeout did not return a value")?
    } else {
        let receive_fn = *compiler
            .functions
            .get("expo_rt_receive")
            .ok_or("expo_rt_receive not declared")?;

        compiler
            .call(receive_fn, &[], "receive_msg")
            .ok_or("expo_rt_receive did not return a value")?
    };

    let merge_block = compiler.context.append_basic_block(function, "recv_end");

    let ptr_val = raw_ptr.into_pointer_value();
    let ptr_ty = compiler.context.ptr_type(AddressSpace::default());
    let null_ptr = ptr_ty.const_null();
    let is_null = compiler
        .builder
        .build_int_compare(IntPredicate::EQ, ptr_val, null_ptr, "is_timeout")
        .unwrap();

    if resolved.has_timeout {
        let after_block = compiler.context.append_basic_block(function, "recv_after");
        let got_msg_block = compiler
            .context
            .append_basic_block(function, "recv_got_msg");

        compiler
            .builder
            .build_conditional_branch(is_null, after_block, got_msg_block)
            .unwrap();

        compiler.builder.position_at_end(after_block);
        for stmt in after_body {
            compile_statement(compiler, stmt, function)?;
        }
        if !compiler.current_block_terminated() {
            compiler
                .builder
                .build_unconditional_branch(merge_block)
                .unwrap();
        }

        compiler.builder.position_at_end(got_msg_block);
    } else {
        let got_msg_block = compiler
            .context
            .append_basic_block(function, "recv_got_msg");
        let empty_block = compiler.context.append_basic_block(function, "recv_empty");

        compiler
            .builder
            .build_conditional_branch(is_null, empty_block, got_msg_block)
            .unwrap();

        compiler.builder.position_at_end(empty_block);
        compiler.builder.build_unreachable().unwrap();

        compiler.builder.position_at_end(got_msg_block);
    }

    compile_receive_tagged(
        compiler,
        arms,
        raw_ptr,
        merge_block,
        &resolved.envelope_type,
        function,
    )
}

/// Control-flow context for compiling a set of receive arms.
struct ReceiveArmCtx<'a, 'ctx> {
    subject_alloca: PointerValue<'ctx>,
    subject_type: &'a Type,
    merge_block: BasicBlock<'ctx>,
    fallthrough_block: BasicBlock<'ctx>,
    prefix: &'a str,
    function: FunctionValue<'ctx>,
}

/// Compiles a slice of receive arms against a loaded subject value.
/// Shared by both the plain and tagged receive paths. Appends to
/// `incoming` and `reachable_arm_count` for the caller's phi node.
fn compile_receive_arms<'ctx>(
    compiler: &mut Compiler<'ctx>,
    arms: &[&MatchArm],
    arm_context: &ReceiveArmCtx<'_, 'ctx>,
    incoming: &mut Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)>,
    reachable_arm_count: &mut usize,
) -> Result<(), String> {
    for (i, arm) in arms.iter().enumerate() {
        let body_block = compiler.context.append_basic_block(
            arm_context.function,
            &format!("{}_body_{i}", arm_context.prefix),
        );
        let next_block = if i + 1 < arms.len() {
            compiler.context.append_basic_block(
                arm_context.function,
                &format!("{}_test_{}", arm_context.prefix, i + 1),
            )
        } else {
            arm_context.fallthrough_block
        };

        let saved_vars = compiler.fn_state.variables.clone();

        let condition = compile_pattern(
            compiler,
            &arm.pattern,
            arm_context.subject_alloca,
            arm_context.subject_type,
            arm_context.function,
        )?;

        let final_cond = if let Some(guard) = &arm.guard {
            let guard_val = compile_expr(compiler, guard, arm_context.function)?
                .ok_or("receive guard produced no value")?
                .value;
            compiler
                .builder
                .build_and(condition, guard_val.into_int_value(), "guard_and")
                .unwrap()
        } else {
            condition
        };

        compiler
            .builder
            .build_conditional_branch(final_cond, body_block, next_block)
            .unwrap();

        compiler.builder.position_at_end(body_block);
        let arm_typed_value = compile_body_as_value(compiler, &arm.body, arm_context.function)?;
        if !compiler.current_block_terminated() {
            compiler
                .builder
                .build_unconditional_branch(arm_context.merge_block)
                .unwrap();
            *reachable_arm_count += 1;
        }
        let arm_end_block = compiler.builder.get_insert_block().unwrap();
        if let Some(typed_value) = arm_typed_value {
            incoming.push((typed_value.value, arm_end_block));
        }

        compiler.fn_state.variables = saved_vars;
        compiler.builder.position_at_end(next_block);
    }

    compiler
        .builder
        .position_at_end(arm_context.fallthrough_block);
    compiler
        .builder
        .build_unconditional_branch(arm_context.merge_block)
        .unwrap();
    Ok(())
}

/// Builds a phi node from `incoming` values if they all share the same
/// LLVM type, adding zero-valued entries for each fallthrough block.
fn build_receive_phi<'ctx>(
    compiler: &Compiler<'ctx>,
    incoming: &mut Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)>,
    reachable_arm_count: usize,
    fallthrough_blocks: &[BasicBlock<'ctx>],
    result_type: &Type,
) -> ExprResult<'ctx> {
    if !incoming.is_empty() && incoming.len() == reachable_arm_count {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let undef = first_ty.const_zero();
            for &block in fallthrough_blocks {
                incoming.push((undef, block));
            }
            let phi = compiler.builder.build_phi(first_ty, "recv_result").unwrap();
            let phi_entries: Vec<_> = incoming
                .iter()
                .map(|(v, block)| (v as &dyn BasicValue, *block))
                .collect();
            phi.add_incoming(&phi_entries);
            return Ok(Some(TypedValue::new(
                phi.as_basic_value(),
                result_type.clone(),
            )));
        }
    }
    Ok(None)
}

/// Loads an IOReady value from the mailbox payload pointer.
fn load_io_ready_from_payload<'ctx>(
    compiler: &mut Compiler<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, String> {
    let io_ready_type = named_std("IOReady");
    let io_ready_llvm = to_llvm_type(&io_ready_type, compiler.context, &compiler.types)
        .ok_or("no LLVM type for IOReady enum")?;
    Ok(compiler
        .builder
        .build_load(io_ready_llvm, payload_ptr, label)
        .unwrap())
}

struct ResolvedTaggedReceive<'a> {
    business_arms: Vec<&'a MatchArm>,
    envelope_type: Type,
    io_ready_arms: Vec<&'a MatchArm>,
    lifecycle_arms: Vec<&'a MatchArm>,
    m_has_io_ready: bool,
}

fn resolve_tagged_receive<'a>(
    compiler: &Compiler,
    arms: &'a [MatchArm],
    envelope_type: &Type,
) -> ResolvedTaggedReceive<'a> {
    let envelope_type = envelope_type.clone();

    let m_type = if let Type::Named { type_args, .. } = &envelope_type {
        type_args.first().cloned()
    } else {
        None
    };

    let m_has_io_ready = m_type.as_ref().is_some_and(|m| {
        if let Type::Union(members) = m {
            members.iter().any(|member| {
                matches!(member, Type::Named { identifier, .. } if identifier.name == "IOReady")
            })
        } else {
            false
        }
    });

    let mut business_arms: Vec<&MatchArm> = Vec::new();
    let mut io_ready_arms: Vec<&MatchArm> = Vec::new();
    let mut lifecycle_arms: Vec<&MatchArm> = Vec::new();

    for arm in arms {
        if let Pattern::TypedBinding { type_expr, .. } = &arm.pattern {
            let resolved = compiler.resolve_type_expr(type_expr);
            if matches!(&resolved, Type::Named { identifier, type_args } if identifier.name == "IOReady" && type_args.is_empty())
            {
                io_ready_arms.push(arm);
                continue;
            }
            if matches!(&resolved, Type::Named { identifier, type_args } if identifier.name == "Lifecycle" && type_args.is_empty())
            {
                lifecycle_arms.push(arm);
                continue;
            }
        }
        business_arms.push(arm);
    }

    ResolvedTaggedReceive {
        business_arms,
        envelope_type,
        io_ready_arms,
        lifecycle_arms,
        m_has_io_ready,
    }
}

/// Compiles a `receive` expression in a Process context where the mailbox
/// uses tagged messages. The raw buffer layout is [tag: 8 bytes, payload].
/// Tag 0 = business message (Pair<M, Option<ReplyTo<R>>>), tag 1 = Lifecycle,
/// tag 2 = IOReady.
fn compile_receive_tagged<'ctx>(
    compiler: &mut Compiler<'ctx>,
    arms: &[MatchArm],
    raw_ptr: BasicValueEnum<'ctx>,
    merge_block: BasicBlock<'ctx>,
    envelope_type: &Type,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_tagged_receive(compiler, arms, envelope_type);

    let has_io_ready = !resolved.io_ready_arms.is_empty();
    let has_lifecycle = !resolved.lifecycle_arms.is_empty();
    let needs_synth_io = resolved.m_has_io_ready && !has_io_ready;

    let i8_type = compiler.context.i8_type();
    let i64_type = compiler.context.i64_type();

    let tag_val = compiler
        .builder
        .build_load(i8_type, raw_ptr.into_pointer_value(), "recv_tag")
        .unwrap()
        .into_int_value();

    let payload_ptr = unsafe {
        compiler
            .builder
            .build_in_bounds_gep(
                i8_type,
                raw_ptr.into_pointer_value(),
                &[i64_type.const_int(8, false)],
                "recv_payload",
            )
            .unwrap()
    };

    let env_llvm = to_llvm_type(&resolved.envelope_type, compiler.context, &compiler.types)
        .ok_or_else(|| {
            format!(
                "no LLVM type for envelope `{}`",
                resolved.envelope_type.display()
            )
        })?;
    let env_struct = env_llvm.into_struct_type();

    let business_alloca = compiler
        .builder
        .build_alloca(env_struct, "biz_subject")
        .unwrap();

    let business_block = compiler
        .context
        .append_basic_block(function, "recv_tag_business");
    let business_dispatch = compiler
        .context
        .append_basic_block(function, "recv_biz_dispatch");
    let lifecycle_block = if has_lifecycle {
        compiler
            .context
            .append_basic_block(function, "recv_tag_lifecycle")
    } else {
        merge_block
    };
    let io_ready_block = if has_io_ready {
        Some(
            compiler
                .context
                .append_basic_block(function, "recv_tag_io_ready"),
        )
    } else {
        None
    };
    let synth_io_block = if needs_synth_io {
        Some(
            compiler
                .context
                .append_basic_block(function, "recv_tag_io_synth"),
        )
    } else {
        None
    };
    let default_block = compiler
        .context
        .append_basic_block(function, "recv_tag_default");

    let mut switch_cases = vec![
        (i8_type.const_int(0, false), business_block),
        (i8_type.const_int(1, false), lifecycle_block),
    ];
    if let Some(io_block) = io_ready_block {
        switch_cases.push((i8_type.const_int(2, false), io_block));
    } else if let Some(synth_block) = synth_io_block {
        switch_cases.push((i8_type.const_int(2, false), synth_block));
    }

    compiler
        .builder
        .build_switch(tag_val, default_block, &switch_cases)
        .unwrap();

    compiler.builder.position_at_end(default_block);
    compiler.builder.build_unreachable().unwrap();

    let mut incoming: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();
    let mut reachable_arm_count = 0usize;
    let mut fallthrough_blocks: Vec<BasicBlock<'ctx>> = Vec::new();

    // --- Synthetic IOReady → business envelope (tag 2) ---
    if let Some(synth_block) = synth_io_block {
        compiler.builder.position_at_end(synth_block);

        let io_ready_val = load_io_ready_from_payload(compiler, payload_ptr, "synth_io")?;

        let io_ready_type = named_std("IOReady");
        let m_type = if let Type::Named { type_args, .. } = &resolved.envelope_type {
            type_args.first().cloned()
        } else {
            None
        };
        let m_ref = m_type.as_ref().unwrap();
        let wrapped_m = compile_union_wrap(compiler, io_ready_val, &io_ready_type, m_ref)?;

        let option_reply_type = if let Type::Named { type_args, .. } = &resolved.envelope_type {
            type_args.get(1).cloned().unwrap_or(Type::Unknown)
        } else {
            Type::Unknown
        };
        let option_reply_llvm = to_llvm_type(&option_reply_type, compiler.context, &compiler.types)
            .ok_or("no LLVM type for Option<ReplyTo<R>>")?
            .into_struct_type();
        let mut option_none = option_reply_llvm.get_undef();
        option_none = compiler
            .builder
            .build_insert_value(option_none, i8_type.const_int(1, false), 0, "synth_none")
            .unwrap()
            .into_struct_value();

        let mut pair_val = env_struct.get_undef();
        pair_val = compiler
            .builder
            .build_insert_value(pair_val, wrapped_m, 0, "synth_first")
            .unwrap()
            .into_struct_value();
        pair_val = compiler
            .builder
            .build_insert_value(pair_val, option_none, 1, "synth_second")
            .unwrap()
            .into_struct_value();

        compiler
            .builder
            .build_store(business_alloca, pair_val)
            .unwrap();
        compiler
            .builder
            .build_unconditional_branch(business_dispatch)
            .unwrap();
    }

    // --- Business arms (tag 0) ---
    compiler.builder.position_at_end(business_block);
    let business_value = compiler
        .builder
        .build_load(env_struct, payload_ptr, "biz_msg")
        .unwrap();
    compiler
        .builder
        .build_store(business_alloca, business_value)
        .unwrap();
    compiler
        .builder
        .build_unconditional_branch(business_dispatch)
        .unwrap();

    // --- Business dispatch (shared by tag 0 and synthetic tag 2) ---
    compiler.builder.position_at_end(business_dispatch);

    let business_fallthrough = compiler
        .context
        .append_basic_block(function, "recv_biz_none");
    let business_context = ReceiveArmCtx {
        subject_alloca: business_alloca,
        subject_type: &resolved.envelope_type,
        merge_block,
        fallthrough_block: business_fallthrough,
        prefix: "recv_biz",
        function,
    };
    compile_receive_arms(
        compiler,
        &resolved.business_arms,
        &business_context,
        &mut incoming,
        &mut reachable_arm_count,
    )?;
    fallthrough_blocks.push(business_fallthrough);

    // --- Lifecycle arms (tag 1) ---
    if has_lifecycle {
        compiler.builder.position_at_end(lifecycle_block);

        let lifecycle_type = named_std("Lifecycle");
        let lifecycle_llvm = to_llvm_type(&lifecycle_type, compiler.context, &compiler.types)
            .ok_or("no LLVM type for Lifecycle enum")?;
        let lifecycle_value = compiler
            .builder
            .build_load(lifecycle_llvm, payload_ptr, "lc_msg")
            .unwrap();
        let lifecycle_alloca = compiler
            .builder
            .build_alloca(lifecycle_value.get_type(), "lc_subject")
            .unwrap();
        compiler
            .builder
            .build_store(lifecycle_alloca, lifecycle_value)
            .unwrap();

        let lifecycle_fallthrough = compiler
            .context
            .append_basic_block(function, "recv_lc_none");
        let lifecycle_context = ReceiveArmCtx {
            subject_alloca: lifecycle_alloca,
            subject_type: &lifecycle_type,
            merge_block,
            fallthrough_block: lifecycle_fallthrough,
            prefix: "recv_lc",
            function,
        };
        compile_receive_arms(
            compiler,
            &resolved.lifecycle_arms,
            &lifecycle_context,
            &mut incoming,
            &mut reachable_arm_count,
        )?;
        fallthrough_blocks.push(lifecycle_fallthrough);
    }

    // --- IOReady arms (tag 2, explicit) ---
    if let Some(io_ready_block) = io_ready_block {
        compiler.builder.position_at_end(io_ready_block);

        let io_ready_type = named_std("IOReady");
        let io_ready_value = load_io_ready_from_payload(compiler, payload_ptr, "io_msg")?;
        let io_ready_alloca = compiler
            .builder
            .build_alloca(io_ready_value.get_type(), "io_subject")
            .unwrap();
        compiler
            .builder
            .build_store(io_ready_alloca, io_ready_value)
            .unwrap();

        let io_ready_fallthrough = compiler
            .context
            .append_basic_block(function, "recv_io_none");
        let io_ready_context = ReceiveArmCtx {
            subject_alloca: io_ready_alloca,
            subject_type: &io_ready_type,
            merge_block,
            fallthrough_block: io_ready_fallthrough,
            prefix: "recv_io",
            function,
        };
        compile_receive_arms(
            compiler,
            &resolved.io_ready_arms,
            &io_ready_context,
            &mut incoming,
            &mut reachable_arm_count,
        )?;
        fallthrough_blocks.push(io_ready_fallthrough);
    }

    compiler.builder.position_at_end(merge_block);
    build_receive_phi(
        compiler,
        &mut incoming,
        reachable_arm_count,
        &fallthrough_blocks,
        &resolved.envelope_type,
    )
}
