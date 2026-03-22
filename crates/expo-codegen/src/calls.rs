//! Function call compilation: regular calls, method calls, and generic call
//! dispatch including type inference and monomorphization triggers.

use expo_ast::ast::Arg;
use expo_typecheck::types::{Type, mangle_name, unwrap_indirect};
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue, StructValue};

use crate::compiler::Compiler;
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::stmt::infer_type_from_llvm;
use crate::structs::compile_struct_construction;
use crate::types::to_llvm_type;

/// Invokes a closure fat pointer (fn ptr + env ptr struct) with the given signature.
pub fn invoke_closure_fat_ptr<'ctx>(
    c: &mut Compiler<'ctx>,
    fat_ptr: StructValue<'ctx>,
    params: &[Type],
    return_type: &Type,
    args: &[Arg],
    function: FunctionValue<'ctx>,
    label: &str,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let fn_ptr = c
        .builder
        .build_extract_value(fat_ptr, 0, &format!("{label}_fn_ptr"))
        .unwrap()
        .into_pointer_value();
    let env_ptr = c
        .builder
        .build_extract_value(fat_ptr, 1, &format!("{label}_env_ptr"))
        .unwrap();

    let mut llvm_call_params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptr_ty.into()];
    for p in params {
        if let Some(lt) = to_llvm_type(p, c.context, &c.struct_types) {
            llvm_call_params.push(lt.into());
        }
    }
    let fn_type = match to_llvm_type(return_type, c.context, &c.struct_types) {
        Some(ret) => ret.fn_type(&llvm_call_params, false),
        None => c.context.void_type().fn_type(&llvm_call_params, false),
    };

    let mut compiled_args: Vec<inkwell::values::BasicMetadataValueEnum> = vec![env_ptr.into()];
    for (i, arg) in args.iter().enumerate() {
        let val = if i < params.len() {
            compile_expr_coerced(c, &arg.value, &params[i], function)?
        } else {
            compile_expr(c, &arg.value, function)?
        }
        .ok_or_else(|| format!("argument to {label} produced no value"))?;
        compiled_args.push(val.into());
    }

    let call_val = c
        .builder
        .build_indirect_call(fn_type, fn_ptr, &compiled_args, &format!("call_{label}"))
        .unwrap();

    Ok(call_val.try_as_basic_value().left())
}

/// Compiles a function call by name. Handles struct constructors, builtins
/// (`print`), direct function calls, and indirect calls through function
/// pointer variables.
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
        "panic" => compile_panic(c, args, function),
        "print_Int32" | "print_Int" | "print_Bool" | "print_Float" | "print_String" => {
            compile_print_builtin(c, name, args, function)
        }
        _ => {
            if let Some(callee) = c.functions.get(name).copied() {
                let param_types: Vec<Type> = c
                    .type_ctx
                    .functions
                    .get(name)
                    .map(|sig| sig.params.iter().map(|p| p.ty.clone()).collect())
                    .unwrap_or_default();

                let mut compiled_args = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let val = if i < param_types.len() {
                        compile_expr_coerced(c, &arg.value, &param_types[i], function)?
                    } else {
                        compile_expr(c, &arg.value, function)?
                    }
                    .ok_or_else(|| format!("argument to {name} produced no value"))?;
                    compiled_args.push(val.into());
                }

                let result = c
                    .builder
                    .build_call(callee, &compiled_args, &format!("call_{name}"))
                    .unwrap();

                Ok(result.try_as_basic_value().left())
            } else if let Some((var_ptr, raw_ty, _)) = c.variables.get(name).cloned() {
                let ty = unwrap_indirect(&raw_ty);
                let Type::Function {
                    params,
                    return_type,
                } = ty.clone()
                else {
                    return Err(format!("undefined function: {name}"));
                };
                let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
                let closure_struct_ty = c
                    .context
                    .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);

                let fat_ptr = c
                    .builder
                    .build_load(closure_struct_ty, var_ptr, &format!("{name}_closure"))
                    .unwrap()
                    .into_struct_value();

                invoke_closure_fat_ptr(
                    c,
                    fat_ptr,
                    &params,
                    return_type.as_ref(),
                    args,
                    function,
                    name,
                )
            } else if c.generic_fn_asts.contains_key(name) {
                compile_generic_call(c, name, args, function)
            } else {
                Err(format!("undefined function: {name}"))
            }
        }
    }
}

fn compile_generic_call<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let mut compiled_args = Vec::new();
    let mut arg_types = Vec::new();
    for arg in args {
        let val = compile_expr(c, &arg.value, function)?
            .ok_or_else(|| format!("argument to {name} produced no value"))?;
        arg_types.push(infer_type_from_llvm(c, &val));
        compiled_args.push(val);
    }

    let sig = c
        .type_ctx
        .functions
        .get(name)
        .ok_or_else(|| format!("no signature for generic function `{name}`"))?;

    let mut subst = std::collections::HashMap::new();
    for (param, arg_ty) in sig.params.iter().zip(arg_types.iter()) {
        if !expo_typecheck::types::unify(&param.ty, arg_ty, &mut subst) {
            return Err(format!(
                "type mismatch for argument `{}` in generic call to `{name}`",
                param.name
            ));
        }
    }

    let type_args: Vec<Type> = sig
        .type_params
        .iter()
        .map(|tp| subst.get(tp).cloned().unwrap_or(Type::Unknown))
        .collect();

    let mangled = mangle_name(name, &type_args);

    if !c.functions.contains_key(&mangled) {
        c.monomorphize_function(name, &type_args)?;
    }

    let callee = *c
        .functions
        .get(&mangled)
        .ok_or_else(|| format!("monomorphized function `{mangled}` not found"))?;

    let call_args: Vec<inkwell::values::BasicMetadataValueEnum> =
        compiled_args.iter().map(|v| (*v).into()).collect();

    let result = c
        .builder
        .build_call(callee, &call_args, &format!("call_{mangled}"))
        .unwrap();

    Ok(result.try_as_basic_value().left())
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

    let val =
        compile_expr(c, &args[0].value, function)?.ok_or("argument to print produced no value")?;

    let printf = *c.functions.get("printf").ok_or("printf not declared")?;

    let is_bool = val.is_int_value() && val.into_int_value().get_type().get_bit_width() == 1;

    if is_bool {
        let str_ptr = crate::util::bool_to_string_ptr(c, val.into_int_value());
        let fmt = c
            .builder
            .build_global_string_ptr("%s\n", "fmt_print_bool")
            .unwrap();
        c.builder
            .build_call(
                printf,
                &[fmt.as_pointer_value().into(), str_ptr.into()],
                "printf_call",
            )
            .unwrap();
    } else {
        let spec = crate::util::printf_format_spec(&val)?;
        let fmt_str = &format!("{spec}\n");
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
    }

    Ok(None)
}

fn compile_panic<'ctx>(
    c: &mut Compiler<'ctx>,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if args.len() != 1 {
        return Err("panic expects exactly 1 argument".to_string());
    }

    let val =
        compile_expr(c, &args[0].value, function)?.ok_or("argument to panic produced no value")?;

    c.emit_panic("panic: %s\n", &[val]);

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

    if name == "print_Bool" {
        let str_ptr = crate::util::bool_to_string_ptr(c, val.into_int_value());
        let fmt = c
            .builder
            .build_global_string_ptr("%s\n", "fmt_print_bool")
            .unwrap();
        c.builder
            .build_call(
                printf,
                &[fmt.as_pointer_value().into(), str_ptr.into()],
                "printf_call",
            )
            .unwrap();
        return Ok(None);
    }

    let fmt_str = match name {
        "print_Int32" => "%d\n",
        "print_Int" => "%lld\n",
        "print_Float" => "%f\n",
        "print_String" => "%s\n",
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
