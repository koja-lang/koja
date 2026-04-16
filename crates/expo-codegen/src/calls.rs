//! Function call compilation: regular calls, method calls, and generic call
//! dispatch including type inference and monomorphization triggers.

use std::collections::HashMap;

use expo_ast::ast::{Arg, FieldInit};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::FnParam;
use expo_typecheck::types::{Type, mangle_method_suffix, substitute, unify, unwrap_indirect};
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
    StructConstructor {
        identifier: Option<TypeIdentifier>,
    },
}

fn resolve_call<'ctx>(c: &Compiler<'ctx>, name: &str) -> Result<ResolvedCall<'ctx>, String> {
    let resolved_id = c.resolve_name_current(name).cloned();
    let is_concrete_type = resolved_id
        .as_ref()
        .is_some_and(|id| c.types.get_concrete(id).is_some());
    if is_concrete_type || c.types.contains_monomorphized(name) {
        return Ok(ResolvedCall::StructConstructor {
            identifier: resolved_id,
        });
    }

    match name {
        "panic" => return Ok(ResolvedCall::Builtin(BuiltinCall::Panic)),
        "print" | "print_Bool" | "print_Float" | "print_Int" | "print_Int32" | "print_String" => {
            return Ok(ResolvedCall::Builtin(BuiltinCall::Print));
        }
        _ => {}
    }

    // When we're inside a method body, the unqualified call `foo(..)` can also
    // refer to another method on the same type. Build the candidate LLVM symbol
    // using the same package-qualifying rule as definition-site mangling so the
    // lookup succeeds for user packages (e.g. `crypto.HMAC_hmac_raw`) without
    // breaking stdlib symbols (e.g. `Int_hash`).
    let mangled_name = c.fn_state.self_type_name.as_ref().map(|tn| {
        let prefix = c.current_method_symbol_prefix(tn);
        format!("{prefix}_{name}")
    });
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
                .and_then(|tn| c.resolve_name_current(tn))
                .and_then(|id| c.type_ctx.get_type(id))
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
        ResolvedCall::StructConstructor { identifier } => {
            compile_call_as_struct(c, name, identifier.as_ref(), args, function)
        }
    }
}

struct ResolvedGenericCall<'ctx> {
    callee: FunctionValue<'ctx>,
    mangled_name: String,
    parameter_types: Vec<Type>,
    return_type: Type,
}

fn resolve_generic_call<'ctx>(
    compiler: &mut Compiler<'ctx>,
    name: &str,
    arg_types: &[Type],
) -> Result<ResolvedGenericCall<'ctx>, String> {
    let sig = compiler
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

    let mangled_name = mangle_method_suffix(name, &type_args);

    if !compiler.functions.contains_key(&mangled_name) {
        monomorphize_function(compiler, name, &type_args)?;
    }

    let callee = *compiler
        .functions
        .get(&mangled_name)
        .ok_or_else(|| format!("monomorphized function `{mangled_name}` not found"))?;

    let subst_map: HashMap<String, Type> = sig
        .type_params
        .iter()
        .zip(type_args.iter())
        .map(|(p, a)| (p.name.clone(), a.clone()))
        .collect();

    let parameter_types: Vec<Type> = sig
        .params
        .iter()
        .map(|p| substitute(&p.ty, &subst_map))
        .collect();

    let return_type = substitute(&sig.return_type, &subst_map);

    Ok(ResolvedGenericCall {
        callee,
        mangled_name,
        parameter_types,
        return_type,
    })
}

fn compile_generic_call<'ctx>(
    compiler: &mut Compiler<'ctx>,
    name: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let mut compiled_args = Vec::new();
    let mut arg_types = Vec::new();
    for arg in args {
        let tv = compile_expr(compiler, &arg.value, function)?
            .ok_or_else(|| format!("argument to {name} produced no value"))?;
        arg_types.push(tv.expo_type);
        compiled_args.push(tv.value);
    }

    let resolved = resolve_generic_call(compiler, name, &arg_types)?;

    let call_args: Vec<BasicMetadataValueEnum> = compiled_args
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if i < resolved.parameter_types.len() {
                coerce_numeric(compiler, *v, &resolved.parameter_types[i]).into()
            } else {
                (*v).into()
            }
        })
        .collect();

    Ok(compiler
        .call(
            resolved.callee,
            &call_args,
            &format!("call_{}", resolved.mangled_name),
        )
        .map(|v| TypedValue::new(v, resolved.return_type)))
}

fn compile_call_as_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    resolved_type: Option<&TypeIdentifier>,
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

    compile_struct_construction(c, &[name.to_string()], &fields, resolved_type, function)
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
