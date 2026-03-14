use expo_ast::ast::{Arg, BinOp, Expr, Literal, StringPart};
use expo_ast::span::Span;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::calls::compile_call;
use crate::compiler::Compiler;
use crate::control::{
    compile_cond, compile_if, compile_loop, compile_match, compile_ternary, compile_while,
};
use crate::enums::compile_enum_construction;
use crate::ops::{compile_binary, compile_unary};
use crate::structs::{compile_field_access, compile_method_call, compile_struct_construction};
use crate::types::to_llvm_type;

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
            } else {
                Err(format!("undefined variable: {name}"))
            }
        }

        Expr::Group { expr, .. } => compile_expr(c, expr, function),

        Expr::Binary {
            op: BinOp::Pipe,
            left,
            right,
            ..
        } => compile_pipe(c, left, right, function),

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
            let clean: String = s.chars().filter(|c| *c != '_').collect();
            let val: i64 = if let Some(hex) = clean
                .strip_prefix("0x")
                .or_else(|| clean.strip_prefix("0X"))
            {
                i64::from_str_radix(hex, 16).map_err(|_| format!("invalid hex integer: {s}"))?
            } else if let Some(bin) = clean
                .strip_prefix("0b")
                .or_else(|| clean.strip_prefix("0B"))
            {
                i64::from_str_radix(bin, 2).map_err(|_| format!("invalid binary integer: {s}"))?
            } else {
                clean
                    .parse()
                    .map_err(|_| format!("integer literals cannot exceed {}", i64::MAX))?
            };
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
        Literal::None => Ok(None),
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

                if val.is_int_value() {
                    let width = val.into_int_value().get_type().get_bit_width();
                    let spec = match width {
                        1 => "%d",
                        32 => "%d",
                        64 => "%lld",
                        _ => "%d",
                    };
                    fmt_string.push_str(spec);
                    interp_values.push(val);
                } else if val.is_float_value() {
                    fmt_string.push_str("%f");
                    interp_values.push(val);
                } else if val.is_pointer_value() {
                    fmt_string.push_str("%s");
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

    if let Some(sig) = c
        .type_ctx
        .enums
        .get(enum_name)
        .and_then(|ei| ei.methods.get("to_string"))
    {
        let _ = sig;
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

fn compile_pipe<'ctx>(
    c: &mut Compiler<'ctx>,
    left: &Expr,
    right: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let pipe_arg = Arg {
        name: None,
        value: left.clone(),
        span: Span::default(),
    };

    match right {
        Expr::Ident { name, .. } => compile_call(c, name, &[pipe_arg], function),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                let mut new_args = vec![pipe_arg];
                new_args.extend(args.iter().cloned());
                compile_call(c, name, &new_args, function)
            } else {
                Err("pipe right-hand side must be a named function call".to_string())
            }
        }
        _ => Err("pipe right-hand side must be a function or function call".to_string()),
    }
}
