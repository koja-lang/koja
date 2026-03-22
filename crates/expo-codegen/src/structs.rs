//! Struct compilation: field access, struct construction (both regular and
//! generic), and method calls on struct instances.

use expo_ast::ast::{ClosureParam, Expr};
use expo_typecheck::types::{Type, build_substitution, mangle_name, substitute, unwrap_indirect};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use crate::calls::compile_call;
use crate::compiler::Compiler;
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::generics::try_parse_mangled_name;
use crate::types::to_llvm_type;

/// Loads a value from `field_ptr`. When `field_type` is [`Type::Indirect`],
/// loads the heap pointer first, then dereferences it to get the inner value.
pub(crate) fn load_maybe_indirect<'ctx>(
    c: &mut Compiler<'ctx>,
    field_ptr: PointerValue<'ctx>,
    field_type: &Type,
    label: &str,
) -> BasicValueEnum<'ctx> {
    if let Type::Indirect(inner) = field_type {
        let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
        let heap_ptr = c
            .builder
            .build_load(ptr_ty, field_ptr, &format!("{label}_ptr"))
            .unwrap()
            .into_pointer_value();
        let _ = c.ensure_types_exist(inner);
        let inner_llvm_ty = to_llvm_type(inner, c.context, &c.struct_types)
            .expect("indirect inner type must have LLVM representation");
        c.builder
            .build_load(inner_llvm_ty, heap_ptr, &format!("{label}_deref"))
            .unwrap()
    } else {
        let llvm_ty = to_llvm_type(field_type, c.context, &c.struct_types)
            .expect("field type must have LLVM representation");
        c.builder.build_load(llvm_ty, field_ptr, label).unwrap()
    }
}

/// Stores `val` into `field_ptr`. When `field_type` is [`Type::Indirect`],
/// heap-allocates storage via `malloc`, writes the value there, and stores the
/// resulting pointer into `field_ptr`.
pub(crate) fn store_maybe_indirect<'ctx>(
    c: &mut Compiler<'ctx>,
    field_ptr: PointerValue<'ctx>,
    val: BasicValueEnum<'ctx>,
    field_type: &Type,
    label: &str,
) {
    if let Type::Indirect(inner) = field_type {
        let _ = c.ensure_types_exist(inner);
        let inner_llvm_ty = to_llvm_type(inner, c.context, &c.struct_types)
            .expect("indirect inner type must have LLVM representation");
        let size = llvm_type_size(inner_llvm_ty, c);
        let malloc_fn = *c.functions.get("malloc").expect("malloc not declared");
        let heap_ptr = c
            .builder
            .build_call(malloc_fn, &[size.into()], &format!("{label}_malloc"))
            .unwrap()
            .try_as_basic_value()
            .left()
            .unwrap()
            .into_pointer_value();
        c.builder.build_store(heap_ptr, val).unwrap();
        c.builder.build_store(field_ptr, heap_ptr).unwrap();
    } else {
        c.builder.build_store(field_ptr, val).unwrap();
    }
}

fn llvm_type_size<'ctx>(
    ty: BasicTypeEnum<'ctx>,
    c: &Compiler<'ctx>,
) -> inkwell::values::IntValue<'ctx> {
    match ty {
        BasicTypeEnum::StructType(st) => st
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::IntType(it) => it.size_of(),
        BasicTypeEnum::FloatType(ft) => ft.size_of(),
        BasicTypeEnum::PointerType(pt) => pt.size_of(),
        BasicTypeEnum::ArrayType(at) => at
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::VectorType(vt) => vt
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
    }
}

/// Compiles a field access expression (`receiver.field`). Handles both
/// direct variable access (via pointer GEP) and expression receivers
/// (which require a temporary alloca).
pub fn compile_field_access<'ctx>(
    c: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if let Expr::Ident { name, .. } = receiver {
        let (ptr, ty, _) = c
            .variables
            .get(name)
            .ok_or_else(|| format!("undefined variable: {name}"))?
            .clone();

        let struct_name = struct_name_from_type(&ty)
            .ok_or_else(|| format!("cannot access field on non-struct variable: {name}"))?;

        let struct_type = *c
            .struct_types
            .get(&struct_name)
            .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

        let field_idx = c
            .get_field_index(&struct_name, field)
            .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

        let field_ty = c
            .get_field_type(&struct_name, field)
            .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, ptr, field_idx, &format!("{name}.{field}"))
            .unwrap();

        let val = load_maybe_indirect(c, field_ptr, &field_ty, field);
        Ok(Some(val))
    } else {
        let recv_val = compile_expr(c, receiver, function)?
            .ok_or("field access on expression that produced no value")?;

        if !recv_val.is_struct_value() {
            return Err("field access on non-struct value".to_string());
        }

        let sv = recv_val.into_struct_value();
        let struct_name = sv
            .get_type()
            .get_name()
            .map(|n| n.to_str().unwrap_or("").to_string())
            .ok_or("cannot determine struct type for field access")?;

        let struct_type = *c
            .struct_types
            .get(&struct_name)
            .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

        let field_idx = c
            .get_field_index(&struct_name, field)
            .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

        let field_ty = c
            .get_field_type(&struct_name, field)
            .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

        let tmp_alloca = c.builder.build_alloca(struct_type, "tmp_struct").unwrap();
        c.builder.build_store(tmp_alloca, recv_val).unwrap();

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, tmp_alloca, field_idx, field)
            .unwrap();

        let val = load_maybe_indirect(c, field_ptr, &field_ty, field);
        Ok(Some(val))
    }
}

/// Compiles a method call (`receiver.method(args)`). Also handles qualified
/// module calls (e.g. `math.add()`) by delegating to `compile_call`.
pub fn compile_method_call<'ctx>(
    c: &mut Compiler<'ctx>,
    receiver: &Expr,
    method: &str,
    args: &[expo_ast::ast::Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if let Expr::Ident { name, .. } = receiver
        && c.type_ctx.imported_modules.contains_key(name)
    {
        return compile_call(c, method, args, function);
    }

    if let Expr::Ident { name, .. } = receiver {
        let is_type_name =
            c.type_ctx.structs.contains_key(name) || c.type_ctx.enums.contains_key(name);
        if is_type_name {
            return compile_static_call(c, name, method, args, function);
        }
    }

    let recv_val = compile_expr(c, receiver, function)?
        .ok_or("method call on expression that produced no value")?;

    if method == "clone" && args.is_empty() {
        return Ok(Some(recv_val));
    }

    let struct_name = resolve_struct_name(c, receiver, &recv_val)?;

    let mut mangled = format!("{}_{}", struct_name, method);

    if let Some((base, type_args)) = try_parse_mangled_name(&struct_name, c) {
        let method_type_params = lookup_method_type_params(c, &base, method);

        if !method_type_params.is_empty() {
            let method_type_args = infer_method_type_args(c, &base, method, &type_args, args)?;
            let method_suffix = mangle_name(method, &method_type_args);
            mangled = format!("{}_{}", struct_name, method_suffix);

            if !c.functions.contains_key(&mangled) {
                c.monomorphize_impl_method_generic(&base, method, &type_args, &method_type_args)?;
            }
        } else if !c.functions.contains_key(&mangled) {
            c.monomorphize_impl_method(&base, method, &type_args)?;
        }
    }

    let callee = *c
        .functions
        .get(&mangled)
        .ok_or_else(|| format!("undefined method `{method}` on `{struct_name}`"))?;

    let method_param_types: Vec<Type> = c
        .type_ctx
        .functions
        .get(&mangled)
        .map(|sig| sig.params.iter().skip(1).map(|p| p.ty.clone()).collect())
        .or_else(|| {
            let (base, type_args) = try_parse_mangled_name(&struct_name, c)?;
            let lookup = c
                .type_ctx
                .structs
                .get(&base)
                .map(|si| (&si.methods, &si.type_params))
                .or_else(|| {
                    c.type_ctx
                        .enums
                        .get(&base)
                        .map(|ei| (&ei.methods, &ei.type_params))
                });
            let (methods, type_params) = lookup?;
            let sig = methods.get(method)?;
            let subst = build_substitution(type_params, &type_args);
            Some(
                sig.params
                    .iter()
                    .map(|p| substitute(&p.ty, &subst))
                    .collect(),
            )
        })
        .unwrap_or_default();

    let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
    llvm_args.push(recv_val.into());

    for (i, arg) in args.iter().enumerate() {
        let val = if i < method_param_types.len() {
            compile_expr_coerced(c, &arg.value, &method_param_types[i], function)?
        } else {
            compile_expr(c, &arg.value, function)?
        }
        .ok_or_else(|| "method argument produced no value".to_string())?;
        llvm_args.push(val.into());
    }

    let result = c
        .builder
        .build_call(callee, &llvm_args, &format!("{mangled}_ret"))
        .unwrap();

    Ok(result.try_as_basic_value().left())
}

fn lookup_method_type_params(c: &Compiler, base_type: &str, method: &str) -> Vec<String> {
    let methods = c
        .type_ctx
        .structs
        .get(base_type)
        .map(|si| &si.methods)
        .or_else(|| c.type_ctx.enums.get(base_type).map(|ei| &ei.methods));
    if let Some(methods) = methods
        && let Some(sig) = methods.get(method)
    {
        return sig.type_params.clone();
    }
    Vec::new()
}

fn infer_method_type_args(
    c: &Compiler,
    base_type: &str,
    method: &str,
    struct_type_args: &[Type],
    args: &[expo_ast::ast::Arg],
) -> Result<Vec<Type>, String> {
    let (methods, type_params) = c
        .type_ctx
        .structs
        .get(base_type)
        .map(|si| (&si.methods, &si.type_params))
        .or_else(|| {
            c.type_ctx
                .enums
                .get(base_type)
                .map(|ei| (&ei.methods, &ei.type_params))
        })
        .ok_or_else(|| format!("no type info for `{base_type}`"))?;

    let sig = methods
        .get(method)
        .ok_or_else(|| format!("no method `{method}` on `{base_type}`"))?;

    let struct_subst = expo_typecheck::types::build_substitution(type_params, struct_type_args);
    let substituted_params: Vec<_> = sig
        .params
        .iter()
        .map(|p| expo_typecheck::types::substitute(&p.ty, &struct_subst))
        .collect();

    let mut method_subst = std::collections::HashMap::new();
    for (i, arg) in args.iter().enumerate() {
        if i >= substituted_params.len() {
            break;
        }
        let arg_type = infer_arg_expo_type(c, &arg.value);
        if arg_type != Type::Unknown {
            expo_typecheck::types::unify(&substituted_params[i], &arg_type, &mut method_subst);
        }
    }

    Ok(sig
        .type_params
        .iter()
        .map(|tp| method_subst.get(tp).cloned().unwrap_or(Type::Unknown))
        .collect())
}

fn infer_arg_expo_type(c: &Compiler, expr: &Expr) -> Type {
    match expr {
        Expr::Ident { name, .. } => c
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
            .or_else(|| {
                let sig = c.type_ctx.functions.get(name)?;
                if sig.type_params.is_empty() {
                    Some(Type::Function {
                        params: sig.params.iter().map(|p| p.ty.clone()).collect(),
                        return_type: Box::new(sig.return_type.clone()),
                    })
                } else {
                    None
                }
            })
            .unwrap_or(Type::Unknown),
        Expr::Closure {
            params,
            return_type,
            ..
        } => {
            let param_types: Vec<Type> = params
                .iter()
                .filter_map(|p| {
                    if let ClosureParam::Name {
                        type_expr: Some(te),
                        ..
                    } = p
                    {
                        Some(c.resolve_type_expr(te))
                    } else {
                        None
                    }
                })
                .collect();
            let ret = match return_type {
                Some(te) => c.resolve_type_expr(te),
                None => Type::Unit,
            };
            Type::Function {
                params: param_types,
                return_type: Box::new(ret),
            }
        }
        _ => Type::Unknown,
    }
}

/// Compiles a struct literal (`StructName { field: value, ... }`).
pub fn compile_struct_construction<'ctx>(
    c: &mut Compiler<'ctx>,
    type_path: &[String],
    fields: &[expo_ast::ast::FieldInit],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let struct_name = type_path
        .first()
        .ok_or("empty type path in struct construction")?;

    // For generic structs, compile field values first, infer type args, and monomorphize
    if let Some(info) = c.type_ctx.structs.get(struct_name)
        && !info.type_params.is_empty()
    {
        return compile_generic_struct_construction(c, struct_name, info.clone(), fields, function);
    }

    let struct_type = *c
        .struct_types
        .get(struct_name)
        .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

    let struct_info = c
        .type_ctx
        .structs
        .get(struct_name)
        .ok_or_else(|| format!("unknown struct: {struct_name}"))?;

    let alloca = c
        .builder
        .build_alloca(struct_type, &format!("{struct_name}_tmp"))
        .unwrap();

    for field_init in fields {
        let (field_idx, field_type) = struct_info
            .fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == &field_init.name)
            .map(|(i, (_, ty))| (i as u32, ty.clone()))
            .ok_or_else(|| {
                format!(
                    "unknown field `{}` in struct `{}`",
                    field_init.name, struct_name
                )
            })?;

        let coerce_ty = unwrap_indirect(&field_type);
        let val = compile_expr_coerced(c, &field_init.value, coerce_ty, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, alloca, field_idx, &field_init.name)
            .unwrap();
        store_maybe_indirect(c, field_ptr, val, &field_type, &field_init.name);
    }

    let struct_val = c
        .builder
        .build_load(struct_type, alloca, struct_name)
        .unwrap();
    Ok(Some(struct_val))
}

fn compile_generic_struct_construction<'ctx>(
    c: &mut Compiler<'ctx>,
    struct_name: &str,
    info: expo_typecheck::context::StructInfo,
    fields: &[expo_ast::ast::FieldInit],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    use crate::stmt::infer_type_from_llvm;

    let mut compiled_fields: Vec<(String, BasicValueEnum<'ctx>)> = Vec::new();
    for field_init in fields {
        let val = compile_expr(c, &field_init.value, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
        compiled_fields.push((field_init.name.clone(), val));
    }

    let mut subst = std::collections::HashMap::new();
    for (i, (field_init_name, field_val)) in compiled_fields.iter().enumerate() {
        if let Some((_, field_ty)) = info.fields.iter().find(|(n, _)| n == field_init_name) {
            let concrete = if let expo_ast::ast::Expr::Ident { name, .. } = &fields[i].value
                && let Some((_, var_ty, _)) = c.variables.get(name)
            {
                var_ty.clone()
            } else {
                infer_type_from_llvm(c, field_val)
            };
            if !expo_typecheck::types::unify(field_ty, &concrete, &mut subst) {
                return Err(format!(
                    "type mismatch for field `{field_init_name}` in generic struct `{struct_name}`"
                ));
            }
        }
    }

    let type_args: Vec<expo_typecheck::types::Type> = info
        .type_params
        .iter()
        .map(|tp| {
            subst
                .get(tp)
                .cloned()
                .unwrap_or(expo_typecheck::types::Type::Unknown)
        })
        .collect();

    let mangled = expo_typecheck::types::mangle_name(struct_name, &type_args);

    if !c.struct_types.contains_key(&mangled) {
        c.monomorphize_struct(struct_name, &type_args)?;
    }

    let struct_type = *c
        .struct_types
        .get(&mangled)
        .ok_or_else(|| format!("monomorphized struct `{mangled}` not found"))?;

    let alloca = c
        .builder
        .build_alloca(struct_type, &format!("{mangled}_tmp"))
        .unwrap();

    for (field_name, field_val) in &compiled_fields {
        let field_idx = c
            .get_field_index(&mangled, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` in struct `{struct_name}`"))?;
        let field_type = c.get_field_type(&mangled, field_name);
        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, alloca, field_idx, field_name)
            .unwrap();
        if let Some(ref ft) = field_type {
            store_maybe_indirect(c, field_ptr, *field_val, ft, field_name);
        } else {
            c.builder.build_store(field_ptr, *field_val).unwrap();
        }
    }

    let struct_val = c.builder.build_load(struct_type, alloca, &mangled).unwrap();
    Ok(Some(struct_val))
}

fn resolve_struct_name<'ctx>(
    c: &Compiler<'ctx>,
    receiver: &Expr,
    recv_val: &BasicValueEnum<'ctx>,
) -> Result<String, String> {
    if let Expr::Ident { name, .. } = receiver
        && let Some((_, ty, _)) = c.variables.get(name)
        && let Some(sn) = struct_name_from_type(ty)
    {
        return Ok(sn);
    }

    if recv_val.is_struct_value() {
        let sv = recv_val.into_struct_value();
        let st = sv.get_type();
        if let Some(n) = st.get_name()
            && let Ok(s) = n.to_str()
        {
            return Ok(s.to_string());
        }
    }

    Err("cannot determine struct type for method call".to_string())
}

fn struct_name_from_type(ty: &Type) -> Option<String> {
    match ty {
        Type::Indirect(inner) => struct_name_from_type(inner),
        Type::Struct(n) | Type::Enum(n) => Some(n.clone()),
        Type::GenericInstance {
            base, type_args, ..
        } => Some(mangle_name(base, type_args)),
        _ => None,
    }
}

fn compile_static_call<'ctx>(
    c: &mut Compiler<'ctx>,
    type_name: &str,
    method: &str,
    args: &[expo_ast::ast::Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let type_params = c
        .type_ctx
        .structs
        .get(type_name)
        .map(|si| &si.type_params)
        .or_else(|| c.type_ctx.enums.get(type_name).map(|ei| &ei.type_params));

    let type_args: Vec<Type> = if let Some(tp) = type_params
        && !tp.is_empty()
    {
        tp.iter()
            .filter_map(|name| c.type_subst.get(name).cloned())
            .collect()
    } else {
        Vec::new()
    };

    let mangled_type = if type_args.is_empty() {
        type_name.to_string()
    } else {
        let m = mangle_name(type_name, &type_args);
        if !c.struct_types.contains_key(&m) {
            if c.type_ctx.structs.contains_key(type_name) {
                c.monomorphize_struct(type_name, &type_args)?;
            } else {
                c.monomorphize_enum(type_name, &type_args)?;
            }
        }
        m
    };

    let mangled_fn = format!("{}_{}", mangled_type, method);

    if !c.functions.contains_key(&mangled_fn) {
        if !type_args.is_empty() {
            c.monomorphize_impl_method(type_name, method, &type_args)?;
        } else {
            return Err(format!(
                "undefined static function `{method}` on `{type_name}`"
            ));
        }
    }

    let callee = *c
        .functions
        .get(&mangled_fn)
        .ok_or_else(|| format!("undefined static function `{method}` on `{mangled_type}`"))?;

    let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
    for arg in args {
        let val = compile_expr(c, &arg.value, function)?
            .ok_or_else(|| "static call argument produced no value".to_string())?;
        llvm_args.push(val.into());
    }

    let result = c
        .builder
        .build_call(callee, &llvm_args, &format!("{mangled_fn}_ret"))
        .unwrap();

    Ok(result.try_as_basic_value().left())
}
