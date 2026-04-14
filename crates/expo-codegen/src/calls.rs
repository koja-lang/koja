//! Function call compilation: regular calls, method calls, and generic call
//! dispatch including type inference and monomorphization triggers.

use std::collections::HashMap;

use expo_ast::ast::{Arg, FieldInit};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::{Type, mangle_name, substitute, unify, unwrap_indirect};
use inkwell::AddressSpace;
use inkwell::types::{BasicMetadataTypeEnum, BasicType};
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue, StructValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::debug::call_format;
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::generics::monomorphize_function;
use crate::stmt::coerce_numeric;
use crate::structs::compile_struct_construction;
use crate::types::to_llvm_type;

enum BuiltinCall {
    Panic,
    Print,
}

enum ResolvedCall<'ctx> {
    Builtin(BuiltinCall),
    ClosureVariable {
        params: Vec<FnParam>,
        return_type: Type,
        var_ptr: PointerValue<'ctx>,
    },
    Direct {
        callee: FunctionValue<'ctx>,
        param_types: Vec<Type>,
        return_type: Type,
    },
    Generic,
    StructConstructor,
}

fn resolve_call<'ctx>(c: &Compiler<'ctx>, name: &str) -> Result<ResolvedCall<'ctx>, String> {
    if c.types.get_stdlib(name).is_some() || c.types.contains_monomorphized(name) {
        return Ok(ResolvedCall::StructConstructor);
    }

    match name {
        "panic" => return Ok(ResolvedCall::Builtin(BuiltinCall::Panic)),
        "print" | "print_Bool" | "print_Float" | "print_Int" | "print_Int32" | "print_String" => {
            return Ok(ResolvedCall::Builtin(BuiltinCall::Print));
        }
        _ => {}
    }

    let mangled_name = c
        .fn_state
        .self_type_name
        .as_ref()
        .map(|tn| format!("{tn}_{name}"));
    let callee_opt = c
        .functions
        .get(name)
        .or_else(|| {
            mangled_name
                .as_ref()
                .and_then(|mn| c.functions.get(mn.as_str()))
        })
        .copied();

    if let Some(callee) = callee_opt {
        let sig = c.type_ctx.functions.get(name).or_else(|| {
            c.fn_state
                .self_type_name
                .as_ref()
                .and_then(|tn| c.type_ctx.find_type(tn))
                .and_then(|ti| ti.functions.get(name))
        });
        let param_types: Vec<Type> = sig
            .map(|s| s.params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default();
        let return_type = sig.map(|s| s.return_type.clone()).unwrap_or(Type::Unknown);
        return Ok(ResolvedCall::Direct {
            callee,
            param_types,
            return_type,
        });
    }

    if let Some((var_ptr, raw_ty, _)) = c.fn_state.variables.get(name).cloned() {
        let ty = unwrap_indirect(&raw_ty);
        let Type::Function {
            params,
            return_type,
        } = ty.clone()
        else {
            return Err(format!("undefined function: {name}"));
        };
        return Ok(ResolvedCall::ClosureVariable {
            params,
            return_type: *return_type,
            var_ptr,
        });
    }

    if c.generic_fn_asts.contains_key(name) {
        return Ok(ResolvedCall::Generic);
    }

    Err(format!("undefined function: {name}"))
}

/// Invokes a closure fat pointer (fn ptr + env ptr struct) with the given signature.
pub fn invoke_closure_fat_ptr<'ctx>(
    c: &mut Compiler<'ctx>,
    fat_ptr: StructValue<'ctx>,
    params: &[FnParam],
    return_type: &Type,
    args: &[Arg],
    function: FunctionValue<'ctx>,
    label: &str,
) -> ExprResult<'ctx> {
    let ptr_ty = c.context.ptr_type(AddressSpace::default());
    let fn_ptr = c
        .builder
        .build_extract_value(fat_ptr, 0, &format!("{label}_fn_ptr"))
        .unwrap()
        .into_pointer_value();
    let env_ptr = c
        .builder
        .build_extract_value(fat_ptr, 1, &format!("{label}_env_ptr"))
        .unwrap();

    let mut llvm_call_params: Vec<BasicMetadataTypeEnum> = vec![ptr_ty.into()];
    for fp in params {
        if let Some(lt) = to_llvm_type(&fp.ty, c.context, &c.types) {
            llvm_call_params.push(lt.into());
        }
    }
    let fn_type = match to_llvm_type(return_type, c.context, &c.types) {
        Some(ret) => ret.fn_type(&llvm_call_params, false),
        None => c.context.void_type().fn_type(&llvm_call_params, false),
    };

    let mut compiled_args: Vec<BasicMetadataValueEnum> = vec![env_ptr.into()];
    for (i, arg) in args.iter().enumerate() {
        let val = if i < params.len() {
            compile_expr_coerced(c, &arg.value, &params[i].ty, function)?
        } else {
            compile_expr(c, &arg.value, function)?.map(|tv| tv.value)
        }
        .ok_or_else(|| format!("argument to {label} produced no value"))?;
        compiled_args.push(val.into());
    }

    let call_val = c
        .builder
        .build_indirect_call(fn_type, fn_ptr, &compiled_args, &format!("call_{label}"))
        .unwrap();

    Ok(call_val
        .try_as_basic_value()
        .basic()
        .map(|v| TypedValue::new(v, return_type.clone())))
}

/// Compiles a function call by name. Handles struct constructors, builtins
/// (`print`), direct function calls, and indirect calls through function
/// pointer variables.
pub fn compile_call<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_call(c, name)?;

    match resolved {
        ResolvedCall::Builtin(BuiltinCall::Panic) => compile_panic(c, args, function),
        ResolvedCall::Builtin(BuiltinCall::Print) => compile_print(c, args, function),
        ResolvedCall::ClosureVariable {
            params,
            return_type,
            var_ptr,
        } => {
            let ptr_ty = c.context.ptr_type(AddressSpace::default());
            let closure_struct_ty = c
                .context
                .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let fat_ptr = c
                .builder
                .build_load(closure_struct_ty, var_ptr, &format!("{name}_closure"))
                .unwrap()
                .into_struct_value();
            invoke_closure_fat_ptr(c, fat_ptr, &params, &return_type, args, function, name)
        }
        ResolvedCall::Direct {
            callee,
            param_types,
            return_type,
        } => {
            let mut compiled_args: Vec<BasicMetadataValueEnum> = Vec::new();
            for (i, arg) in args.iter().enumerate() {
                let val = if i < param_types.len() {
                    compile_expr_coerced(c, &arg.value, &param_types[i], function)?
                } else {
                    compile_expr(c, &arg.value, function)?.map(|tv| tv.value)
                }
                .ok_or_else(|| format!("argument to {name} produced no value"))?;
                compiled_args.push(val.into());
            }
            Ok(c.call(callee, &compiled_args, &format!("call_{name}"))
                .map(|v| TypedValue::new(v, return_type)))
        }
        ResolvedCall::Generic => compile_generic_call(c, name, args, function),
        ResolvedCall::StructConstructor => compile_call_as_struct(c, name, args, function),
    }
}

fn compile_generic_call<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let mut compiled_args = Vec::new();
    let mut arg_types = Vec::new();
    for arg in args {
        let tv = compile_expr(c, &arg.value, function)?
            .ok_or_else(|| format!("argument to {name} produced no value"))?;
        arg_types.push(tv.expo_type);
        compiled_args.push(tv.value);
    }

    let sig = c
        .type_ctx
        .functions
        .get(name)
        .ok_or_else(|| format!("no signature for generic function `{name}`"))?;

    let mut subst = HashMap::new();
    for (param, arg_ty) in sig.params.iter().zip(arg_types.iter()) {
        if !unify(&param.ty, arg_ty, &mut subst) {
            return Err(format!(
                "type mismatch for argument `{}` in generic call to `{name}`",
                param.name
            ));
        }
    }

    let type_args: Vec<Type> = sig
        .type_params
        .iter()
        .map(|tp| subst.get(&tp.name).cloned().unwrap_or(Type::Unknown))
        .collect();

    let mangled = mangle_name(name, &type_args);

    if !c.functions.contains_key(&mangled) {
        monomorphize_function(c, name, &type_args)?;
    }

    let callee = *c
        .functions
        .get(&mangled)
        .ok_or_else(|| format!("monomorphized function `{mangled}` not found"))?;

    let subst_map: HashMap<String, Type> = sig
        .type_params
        .iter()
        .zip(type_args.iter())
        .map(|(p, a)| (p.name.clone(), a.clone()))
        .collect();

    let call_args: Vec<BasicMetadataValueEnum> = compiled_args
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if let Some(param) = sig.params.get(i) {
                let concrete = substitute(&param.ty, &subst_map);
                coerce_numeric(c, *v, &concrete).into()
            } else {
                (*v).into()
            }
        })
        .collect();

    let ret_type = substitute(&sig.return_type, &subst_map);

    Ok(c.call(callee, &call_args, &format!("call_{mangled}"))
        .map(|v| TypedValue::new(v, ret_type)))
}

fn compile_call_as_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let fields: Vec<FieldInit> = args
        .iter()
        .map(|arg| FieldInit {
            name: arg
                .name
                .clone()
                .unwrap_or_else(|| String::from("<unnamed>")),
            value: arg.value.clone(),
            span: arg.span,
        })
        .collect();

    compile_struct_construction(c, &[name.to_string()], &fields, None, function)
}

fn compile_print<'ctx>(
    c: &mut Compiler<'ctx>,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if args.len() != 1 {
        return Err("print expects exactly 1 argument".to_string());
    }

    let tv =
        compile_expr(c, &args[0].value, function)?.ok_or("argument to print produced no value")?;

    let str_ptr = call_format(c, tv.value, &tv.expo_type)?;

    let printf = *c.functions.get("printf").ok_or("printf not declared")?;
    let fmt = c
        .builder
        .build_global_string_ptr("%s\n", "fmt_print")
        .unwrap();
    c.call_void(
        printf,
        &[fmt.as_pointer_value().into(), str_ptr.into()],
        "printf_call",
    );

    Ok(None)
}

fn compile_panic<'ctx>(
    c: &mut Compiler<'ctx>,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if args.len() != 1 {
        return Err("panic expects exactly 1 argument".to_string());
    }

    let val = compile_expr(c, &args[0].value, function)?
        .ok_or("argument to panic produced no value")?
        .value;

    c.emit_panic("%s", &[val]);

    Ok(None)
}
