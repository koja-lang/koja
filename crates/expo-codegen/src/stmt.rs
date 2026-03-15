//! Statement compilation: let bindings, assignments, compound assignments,
//! return, break, and expression statements.

use expo_ast::ast::{AssignTarget, ClosureParam, Expr, Statement};
use expo_typecheck::types::{GenericKind, Primitive, Type, mangle_name};
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::types::to_llvm_type;

/// Compiles a single statement (assignment, return, break, or compound
/// assignment). Expression statements are compiled for side effects only.
pub fn compile_statement<'ctx>(
    c: &mut Compiler<'ctx>,
    stmt: &Statement,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match stmt {
        Statement::Expr(expr) => {
            compile_expr(c, expr, function)?;
            Ok(None)
        }

        Statement::Assignment {
            target,
            type_annotation,
            value,
            ..
        } => {
            let raw_val =
                compile_expr(c, value, function)?.ok_or("assignment value produced no value")?;

            let ty = if let Some(te) = type_annotation {
                let annotated = c.resolve_type_expr(te);
                let _ = c.ensure_types_exist(&annotated);
                annotated
            } else {
                infer_type_from_expr(c, value).unwrap_or_else(|| infer_type_from_llvm(c, &raw_val))
            };

            let val = coerce_numeric(c, raw_val, &ty);

            match target {
                AssignTarget::LValue(lvalue) => {
                    if lvalue.segments.len() == 1 {
                        let name = &lvalue.segments[0];
                        if let Some((ptr, var_ty)) = c.variables.get(name).cloned() {
                            let store_val = coerce_numeric(c, val, &var_ty);
                            c.builder.build_store(ptr, store_val).unwrap();
                        } else {
                            let alloca_ty = to_llvm_type(&ty, c.context, &c.struct_types)
                                .unwrap_or(val.get_type());
                            let alloca = c.builder.build_alloca(alloca_ty, name).unwrap();
                            c.builder.build_store(alloca, val).unwrap();
                            c.variables.insert(name.clone(), (alloca, ty));
                        }
                    } else {
                        compile_field_assignment(c, &lvalue.segments, val)?;
                    }
                }
                AssignTarget::Pattern(pat) => {
                    let expo_ast::ast::Pattern::Binding { name, .. } = pat else {
                        return Err(
                            "destructuring patterns not yet supported in compilation".to_string()
                        );
                    };

                    let alloca_ty =
                        to_llvm_type(&ty, c.context, &c.struct_types).unwrap_or(val.get_type());
                    let alloca = c.builder.build_alloca(alloca_ty, name).unwrap();
                    c.builder.build_store(alloca, val).unwrap();
                    c.variables.insert(name.clone(), (alloca, ty));
                }
            }
            Ok(None)
        }

        Statement::Return { value, .. } => {
            if let Some(expr) = value {
                let val = compile_expr(c, expr, function)?;
                if let Some(v) = val {
                    c.builder.build_return(Some(&v)).unwrap();
                } else {
                    c.builder.build_return(None).unwrap();
                }
            } else {
                c.builder.build_return(None).unwrap();
            }
            Ok(None)
        }

        Statement::Break { .. } => {
            let exit_block = c.loop_exit_stack.last().ok_or("break outside of loop")?;
            c.builder.build_unconditional_branch(*exit_block).unwrap();
            Ok(None)
        }

        Statement::CompoundAssign {
            target, op, value, ..
        } => {
            if target.segments.len() != 1 {
                return Err("compound assignment to fields not yet supported".to_string());
            }
            let name = &target.segments[0];

            let (ptr, var_ty) = c
                .variables
                .get(name)
                .ok_or_else(|| format!("undefined variable: {name}"))?
                .clone();

            let llvm_ty = to_llvm_type(&var_ty, c.context, &c.struct_types)
                .ok_or("cannot load variable of unsupported type")?;
            let current = c.builder.build_load(llvm_ty, ptr, name).unwrap();
            let rhs = compile_expr(c, value, function)?
                .ok_or("compound assignment value produced no value")?;

            if current.is_int_value() && rhs.is_int_value() {
                let l = current.into_int_value();
                let r = rhs.into_int_value();
                let result = match op {
                    expo_ast::ast::CompoundOp::Add => {
                        c.builder.build_int_add(l, r, "cadd").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Sub => {
                        c.builder.build_int_sub(l, r, "csub").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Mul => {
                        c.builder.build_int_mul(l, r, "cmul").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Div => {
                        c.builder.build_int_signed_div(l, r, "cdiv").unwrap()
                    }
                };
                c.builder.build_store(ptr, result).unwrap();
            } else if current.is_float_value() && rhs.is_float_value() {
                let l = current.into_float_value();
                let r = rhs.into_float_value();
                let result = match op {
                    expo_ast::ast::CompoundOp::Add => {
                        c.builder.build_float_add(l, r, "cfadd").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Sub => {
                        c.builder.build_float_sub(l, r, "cfsub").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Mul => {
                        c.builder.build_float_mul(l, r, "cfmul").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Div => {
                        c.builder.build_float_div(l, r, "cfdiv").unwrap()
                    }
                };
                c.builder.build_store(ptr, result).unwrap();
            } else {
                return Err("compound assignment requires matching numeric types".to_string());
            }

            Ok(None)
        }
    }
}

fn compile_field_assignment<'ctx>(
    c: &mut Compiler<'ctx>,
    segments: &[String],
    val: BasicValueEnum<'ctx>,
) -> Result<(), String> {
    let var_name = &segments[0];
    let (mut ptr, ty) = c
        .variables
        .get(var_name)
        .ok_or_else(|| format!("undefined variable: {var_name}"))?
        .clone();

    let mut current_type = ty;

    for field_name in &segments[1..] {
        let struct_name = match &current_type {
            Type::Struct(n) => n.clone(),
            Type::GenericInstance {
                base,
                type_args,
                kind: GenericKind::Struct,
            } => mangle_name(base, type_args),
            _ => {
                return Err(format!(
                    "cannot access field `{field_name}` on non-struct type"
                ));
            }
        };

        let struct_type = *c
            .struct_types
            .get(&struct_name)
            .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

        let field_idx = c
            .get_field_index(&struct_name, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` on struct `{struct_name}`"))?;

        let field_ty = c
            .get_field_type(&struct_name, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` on struct `{struct_name}`"))?;

        ptr = c
            .builder
            .build_struct_gep(
                struct_type,
                ptr,
                field_idx,
                &format!("{var_name}.{field_name}"),
            )
            .unwrap();

        current_type = field_ty;
    }

    c.builder.build_store(ptr, val).unwrap();
    Ok(())
}

/// Attempts to derive the Expo type directly from the expression AST. Returns
/// `Some(Type::Function{..})` for closures so the variable is stored with the
/// correct callable type rather than being misidentified as a string pointer.
fn infer_type_from_expr(c: &Compiler, expr: &Expr) -> Option<Type> {
    if let Expr::Closure {
        params,
        return_type,
        ..
    } = expr
    {
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| match p {
                ClosureParam::Name {
                    type_expr: Some(te),
                    ..
                } => c.resolve_type_expr(te),
                _ => Type::Primitive(expo_typecheck::types::Primitive::I32),
            })
            .collect();
        let ret = match return_type {
            Some(te) => c.resolve_type_expr(te),
            None => Type::Unit,
        };
        return Some(Type::Function {
            params: param_types,
            return_type: Box::new(ret),
        });
    }
    None
}

/// Parses a mangled enum name like `Option_$i32$` and reconstructs a
/// `GenericInstance` type so unification works in generic function calls.
fn parse_mangled_enum_type(mangled: &str, c: &Compiler) -> Option<Type> {
    use expo_typecheck::types::{GenericKind, Primitive};

    let sep = mangled.find("_$")?;
    let base = &mangled[..sep];
    if !c.type_ctx.generic_enum_asts.contains_key(base) {
        return None;
    }
    if !mangled.ends_with('$') {
        return None;
    }
    let inner = &mangled[sep + 2..mangled.len() - 1];
    let type_args: Vec<Type> = inner
        .split('.')
        .map(|s| {
            if s == "unit" {
                Type::Unit
            } else if let Some(p) = Primitive::from_name(s) {
                Type::Primitive(p)
            } else {
                Type::Struct(s.to_string())
            }
        })
        .collect();
    Some(Type::GenericInstance {
        base: base.to_string(),
        type_args,
        kind: GenericKind::Enum,
    })
}

/// Reconstructs an Expo type from an LLVM value by inspecting bit widths and
/// struct names. Used when assigning to a new variable without a type annotation.
pub fn infer_type_from_llvm<'ctx>(c: &Compiler<'ctx>, val: &BasicValueEnum<'ctx>) -> Type {
    use expo_typecheck::types::Primitive;

    if val.is_int_value() {
        match val.into_int_value().get_type().get_bit_width() {
            1 => Type::Primitive(Primitive::Bool),
            8 => Type::Primitive(Primitive::I8),
            16 => Type::Primitive(Primitive::I16),
            32 => Type::Primitive(Primitive::I32),
            64 => Type::Primitive(Primitive::I64),
            _ => Type::Unknown,
        }
    } else if val.is_float_value() {
        Type::Primitive(Primitive::F64)
    } else if val.is_pointer_value() {
        Type::Primitive(Primitive::String)
    } else if val.is_struct_value() {
        let sv = val.into_struct_value();
        let st = sv.get_type();
        if let Some(name) = st.get_name()
            && let Ok(name_str) = name.to_str()
        {
            if c.type_ctx.structs.contains_key(name_str)
                || c.mono_struct_info.contains_key(name_str)
            {
                return Type::Struct(name_str.to_string());
            }
            if c.type_ctx.enums.contains_key(name_str) {
                return Type::Enum(name_str.to_string());
            }
            if c.mono_enum_variants.contains_key(name_str) {
                if let Some(gi) = parse_mangled_enum_type(name_str, c) {
                    return gi;
                }
                return Type::Enum(name_str.to_string());
            }
        }
        Type::Unknown
    } else {
        Type::Unknown
    }
}

fn coerce_numeric<'ctx>(
    c: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    target: &Type,
) -> BasicValueEnum<'ctx> {
    let Type::Primitive(target_prim) = target else {
        return val;
    };

    if val.is_int_value() && target_prim.is_integer() {
        let iv = val.into_int_value();
        let src_bits = iv.get_type().get_bit_width();
        let dst_bits = int_bit_width(target_prim);
        if src_bits == dst_bits {
            return iv.into();
        }
        let dst_type = c.context.custom_width_int_type(dst_bits);
        if dst_bits < src_bits {
            return c
                .builder
                .build_int_truncate(iv, dst_type, "trunc")
                .unwrap()
                .into();
        }
        let signed = matches!(
            target_prim,
            Primitive::I8 | Primitive::I16 | Primitive::I32 | Primitive::I64
        );
        if signed {
            c.builder
                .build_int_s_extend(iv, dst_type, "sext")
                .unwrap()
                .into()
        } else {
            c.builder
                .build_int_z_extend(iv, dst_type, "zext")
                .unwrap()
                .into()
        }
    } else if val.is_float_value() && target_prim.is_float() {
        let fv = val.into_float_value();
        let dst_is_f64 = *target_prim == Primitive::F64;
        if (fv.get_type() == c.context.f64_type()) == dst_is_f64 {
            return fv.into();
        }
        if dst_is_f64 {
            c.builder
                .build_float_ext(fv, c.context.f64_type(), "fpext")
                .unwrap()
                .into()
        } else {
            c.builder
                .build_float_trunc(fv, c.context.f32_type(), "fptrunc")
                .unwrap()
                .into()
        }
    } else {
        val
    }
}

fn int_bit_width(p: &Primitive) -> u32 {
    match p {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 => 64,
        _ => 0,
    }
}
