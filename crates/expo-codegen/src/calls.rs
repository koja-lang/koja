use expo_ast::ast::Arg;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::structs::compile_struct_construction;

pub fn compile_call<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if c.struct_types.contains_key(name) {
        return compile_call_as_struct(c, name, args, function);
    }

    match name {
        "print" => compile_print(c, args, function),
        "print_i32" | "print_i64" | "print_bool" | "print_f64" | "print_string" => {
            compile_print_builtin(c, name, args, function)
        }
        _ => {
            let callee = *c
                .functions
                .get(name)
                .ok_or_else(|| format!("undefined function: {name}"))?;

            let mut compiled_args = Vec::new();
            for arg in args {
                let val = compile_expr(c, &arg.value, function)?
                    .ok_or_else(|| format!("argument to {name} produced no value"))?;
                compiled_args.push(val.into());
            }

            let result = c
                .builder
                .build_call(callee, &compiled_args, &format!("call_{name}"))
                .unwrap();

            Ok(result.try_as_basic_value().left())
        }
    }
}

fn compile_call_as_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let fields: Vec<expo_ast::ast::FieldInit> = args
        .iter()
        .map(|arg| expo_ast::ast::FieldInit {
            name: arg
                .name
                .clone()
                .unwrap_or_else(|| String::from("<unnamed>")),
            value: arg.value.clone(),
            span: arg.span,
        })
        .collect();

    compile_struct_construction(c, &[name.to_string()], &fields, function)
}

fn compile_print<'ctx>(
    c: &mut Compiler<'ctx>,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if args.len() != 1 {
        return Err("print expects exactly 1 argument".to_string());
    }

    let val = compile_expr(c, &args[0].value, function)?
        .ok_or("argument to print produced no value")?;

    let printf = *c.functions.get("printf").ok_or("printf not declared")?;

    let fmt_str = if val.is_int_value() {
        let width = val.into_int_value().get_type().get_bit_width();
        match width {
            1 => "%d\n",
            32 => "%d\n",
            64 => "%lld\n",
            _ => "%d\n",
        }
    } else if val.is_float_value() {
        "%f\n"
    } else if val.is_pointer_value() {
        "%s\n"
    } else {
        return Err("print: unsupported argument type".to_string());
    };

    let fmt = c
        .builder
        .build_global_string_ptr(fmt_str, "fmt_print")
        .unwrap();

    c.builder
        .build_call(
            printf,
            &[fmt.as_pointer_value().into(), val.into()],
            "printf_call",
        )
        .unwrap();

    Ok(None)
}

fn compile_print_builtin<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects exactly 1 argument"));
    }

    let val = compile_expr(c, &args[0].value, function)?
        .ok_or_else(|| format!("argument to {name} produced no value"))?;

    let printf = *c.functions.get("printf").ok_or("printf not declared")?;

    let fmt_str = match name {
        "print_i32" => "%d\n",
        "print_i64" => "%lld\n",
        "print_f64" => "%f\n",
        "print_bool" => "%d\n",
        "print_string" => "%s\n",
        _ => return Err(format!("unknown print builtin: {name}")),
    };

    let fmt = c
        .builder
        .build_global_string_ptr(fmt_str, &format!("fmt_{name}"))
        .unwrap();

    c.builder
        .build_call(
            printf,
            &[fmt.as_pointer_value().into(), val.into()],
            "printf_call",
        )
        .unwrap();

    Ok(None)
}
