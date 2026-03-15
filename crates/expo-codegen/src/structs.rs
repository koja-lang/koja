use expo_ast::ast::Expr;
use expo_typecheck::types::Type;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::calls::compile_call;
use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::types::to_llvm_type;

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
        let (ptr, ty) = c
            .variables
            .get(name)
            .ok_or_else(|| format!("undefined variable: {name}"))?
            .clone();

        let struct_name = match &ty {
            Type::Struct(n) => n.clone(),
            _ => {
                return Err(format!(
                    "cannot access field on non-struct variable: {name}"
                ));
            }
        };

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

        let field_llvm_ty = to_llvm_type(&field_ty, c.context, &c.struct_types)
            .ok_or_else(|| format!("unsupported field type for `{field}`"))?;

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, ptr, field_idx, &format!("{name}.{field}"))
            .unwrap();

        let val = c
            .builder
            .build_load(field_llvm_ty, field_ptr, field)
            .unwrap();
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

        let field_llvm_ty = to_llvm_type(&field_ty, c.context, &c.struct_types)
            .ok_or_else(|| format!("unsupported field type for `{field}`"))?;

        let tmp_alloca = c.builder.build_alloca(struct_type, "tmp_struct").unwrap();
        c.builder.build_store(tmp_alloca, recv_val).unwrap();

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, tmp_alloca, field_idx, field)
            .unwrap();

        let val = c
            .builder
            .build_load(field_llvm_ty, field_ptr, field)
            .unwrap();
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

    let recv_val = compile_expr(c, receiver, function)?
        .ok_or("method call on expression that produced no value")?;

    let struct_name = resolve_struct_name(c, receiver, &recv_val)?;

    let mangled = format!("{}_{}", struct_name, method);
    let callee = *c
        .functions
        .get(&mangled)
        .ok_or_else(|| format!("undefined method `{method}` on `{struct_name}`"))?;

    let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
    llvm_args.push(recv_val.into());

    for arg in args {
        let val = compile_expr(c, &arg.value, function)?
            .ok_or_else(|| "method argument produced no value".to_string())?;
        llvm_args.push(val.into());
    }

    let result = c
        .builder
        .build_call(callee, &llvm_args, &format!("{mangled}_ret"))
        .unwrap();

    Ok(result.try_as_basic_value().left())
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
    if let Some(info) = c.type_ctx.structs.get(struct_name) {
        if !info.type_params.is_empty() {
            return compile_generic_struct_construction(
                c,
                struct_name,
                info.clone(),
                fields,
                function,
            );
        }
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
        let field_idx = struct_info
            .fields
            .iter()
            .position(|(name, _)| name == &field_init.name)
            .ok_or_else(|| {
                format!(
                    "unknown field `{}` in struct `{}`",
                    field_init.name, struct_name
                )
            })? as u32;

        let val = compile_expr(c, &field_init.value, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;

        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, alloca, field_idx, &field_init.name)
            .unwrap();
        c.builder.build_store(field_ptr, val).unwrap();
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
    for (field_init_name, field_val) in &compiled_fields {
        if let Some((_, field_ty)) = info.fields.iter().find(|(n, _)| n == field_init_name) {
            let concrete = infer_type_from_llvm(c, field_val);
            expo_typecheck::types::unify(field_ty, &concrete, &mut subst);
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

    for (idx, (field_name, field_val)) in compiled_fields.iter().enumerate() {
        let field_ptr = c
            .builder
            .build_struct_gep(struct_type, alloca, idx as u32, field_name)
            .unwrap();
        c.builder.build_store(field_ptr, *field_val).unwrap();
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
        && let Some((_, ty)) = c.variables.get(name)
        && let Type::Struct(n) = ty
    {
        return Ok(n.clone());
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
