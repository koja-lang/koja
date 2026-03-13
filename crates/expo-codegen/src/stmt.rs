use expo_ast::ast::{AssignTarget, Statement};
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::types::to_llvm_type;

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
            target, value, ..
        } => {
            let val = compile_expr(c, value, function)?
                .ok_or("assignment value produced no value")?;

            match target {
                AssignTarget::LValue(lvalue) => {
                    if lvalue.segments.len() == 1 {
                        let name = &lvalue.segments[0];
                        if let Some((ptr, _)) = c.variables.get(name) {
                            c.builder.build_store(*ptr, val).unwrap();
                        } else {
                            let alloca = c
                                .builder
                                .build_alloca(val.get_type(), name)
                                .unwrap();
                            c.builder.build_store(alloca, val).unwrap();
                            let ty = infer_type_from_llvm(c, &val);
                            c.variables.insert(name.clone(), (alloca, ty));
                        }
                    } else {
                        return Err("field assignment not yet supported in compilation".to_string());
                    }
                }
                AssignTarget::Pattern(pat) => {
                    if let expo_ast::ast::Pattern::Binding { name, .. } = pat {
                        let alloca = c
                            .builder
                            .build_alloca(val.get_type(), name)
                            .unwrap();
                        c.builder.build_store(alloca, val).unwrap();
                        let ty = infer_type_from_llvm(c, &val);
                        c.variables.insert(name.clone(), (alloca, ty));
                    } else {
                        return Err(
                            "destructuring patterns not yet supported in compilation".to_string()
                        );
                    }
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
            Err("break outside of loop not yet supported in compilation".to_string())
        }

        Statement::CompoundAssign {
            target, op, value, ..
        } => {
            let name = if target.segments.len() == 1 {
                &target.segments[0]
            } else {
                return Err("compound assignment to fields not yet supported".to_string());
            };

            let (ptr, var_ty) = c
                .variables
                .get(name)
                .ok_or_else(|| format!("undefined variable: {name}"))?
                .clone();

            let llvm_ty = to_llvm_type(&var_ty, c.context)
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

fn infer_type_from_llvm<'ctx>(
    _c: &Compiler<'ctx>,
    val: &BasicValueEnum<'ctx>,
) -> expo_typecheck::types::Type {
    use expo_typecheck::types::{Primitive, Type};

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
    } else {
        Type::Unknown
    }
}
