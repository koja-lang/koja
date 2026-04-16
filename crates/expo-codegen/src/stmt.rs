//! Statement compilation: let bindings, assignments, compound assignments,
//! return, break, and expression statements.

use expo_ast::ast::{
    AssignTarget, BinOp, ClosureParam, CompoundOp, Expr, ExprKind, LValue, Literal, Pattern,
    Statement, StringPart, TypeExpr,
};
use expo_ast::span::Span;
use expo_typecheck::context::{Coercion, FnParam};
use expo_typecheck::types::{
    Primitive, Type, mangle_name, mangle_type, substitute, substitute_preserving,
};
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use std::collections::HashMap;

use expo_ir::resolved::fields::ResolvedFieldStep;
use expo_ir::resolved::ops::{OperandShape, ResolvedCompoundOp, resolve_compound_op};

use crate::compiler::Compiler;
use crate::drop::{Ownership, drop_live_variables};
use crate::expr::compile_expr;
use crate::generics::{ensure_types_exist, monomorphize_impl_method};
use crate::structs::infer_static_method_return_type;
use crate::types::to_llvm_type;

/// Compiles a single statement (assignment, return, break, or compound
/// assignment). Expression statements are compiled for side effects only.
pub fn compile_statement<'ctx>(
    compiler: &mut Compiler<'ctx>,
    stmt: &Statement,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let span = statement_span(stmt);
    compiler.debug.set_location(
        compiler.context,
        &compiler.builder,
        span.start.line,
        span.start.column,
    );

    match stmt {
        Statement::Expr(expr) => {
            compile_expr(compiler, expr, function)?;
            Ok(None)
        }

        Statement::Assignment {
            target,
            type_annotation,
            value,
            ..
        } => {
            compile_assignment(compiler, target, type_annotation, value, function)?;
            Ok(None)
        }

        Statement::Return { value, .. } => {
            if let Some(expr) = value {
                compiler.fn_state.tco.mark_tail();
                let val = compile_expr(compiler, expr, function)?.map(|tv| tv.value);
                compiler.fn_state.tco.clear_tail();
                if !compiler.current_block_terminated() {
                    let skip = match &expr.kind {
                        ExprKind::Ident { name, .. } => Some(name.as_str()),
                        _ => None,
                    };
                    drop_live_variables(compiler, skip);
                    if let Some(value) = val {
                        let value = apply_coercion(compiler, value, expr)?;
                        compiler.builder.build_return(Some(&value)).unwrap();
                    } else {
                        compiler.builder.build_return(None).unwrap();
                    }
                }
            } else {
                drop_live_variables(compiler, None);
                compiler.builder.build_return(None).unwrap();
            }
            Ok(None)
        }

        Statement::Break { .. } => {
            let exit_block = compiler
                .fn_state
                .loop_exit_stack
                .last()
                .ok_or("break outside of loop")?;
            compiler
                .builder
                .build_unconditional_branch(*exit_block)
                .unwrap();
            Ok(None)
        }

        Statement::CompoundAssign {
            target, op, value, ..
        } => {
            compile_compound_assign(compiler, target, op, value, function)?;
            Ok(None)
        }
    }
}

/// Compiles a let binding or reassignment, handling type annotations,
/// list literal conversions, and numeric coercions.
fn compile_assignment<'ctx>(
    compiler: &mut Compiler<'ctx>,
    target: &AssignTarget,
    type_annotation: &Option<TypeExpr>,
    value: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let mut saved_subst = None;
    if let Some(type_expression) = type_annotation {
        let entries = resolve_annotation_subst(compiler, type_expression);
        if !entries.is_empty() {
            saved_subst = Some(compiler.fn_state.type_subst.clone());
            for (name, resolved_type) in entries {
                compiler.fn_state.type_subst.insert(name, resolved_type);
            }
        }
    }

    let val_tv =
        compile_expr(compiler, value, function)?.ok_or("assignment value produced no value")?;
    let raw_val = val_tv.value;
    let compiled_type = val_tv.expo_type;

    if let Some(saved) = saved_subst {
        compiler.fn_state.type_subst = saved;
    }

    let assigned_type = if let Some(type_expression) = type_annotation {
        let annotated = resolve_final_annotation_type(compiler, type_expression);
        let _ = ensure_types_exist(compiler, &annotated);
        annotated
    } else if compiled_type != Type::Unknown {
        compiled_type
    } else {
        infer_type_from_expr(compiler, value).unwrap_or(Type::Unknown)
    };

    let raw_val = if matches!(&value.kind, ExprKind::List { .. }) {
        convert_list_literal_if_needed(compiler, raw_val, &assigned_type)?
    } else {
        raw_val
    };

    let val = coerce_numeric(compiler, raw_val, &assigned_type);
    let val = apply_coercion(compiler, val, value)?;

    match target {
        AssignTarget::LValue(lvalue) => {
            if lvalue.segments.len() == 1 {
                let name = &lvalue.segments[0];
                if let Some((ptr, variable_type, _)) =
                    compiler.fn_state.variables.get(name).cloned()
                {
                    let store_val = coerce_numeric(compiler, val, &variable_type);
                    compiler.builder.build_store(ptr, store_val).unwrap();
                } else {
                    let ownership = ownership_for_expr(value, &assigned_type);
                    let alloca_type =
                        to_llvm_type(&assigned_type, compiler.context, &compiler.types)
                            .unwrap_or(val.get_type());
                    let alloca = compiler.build_entry_alloca(alloca_type, name);
                    compiler.builder.build_store(alloca, val).unwrap();
                    compiler
                        .fn_state
                        .variables
                        .insert(name.clone(), (alloca, assigned_type, ownership));
                }
            } else {
                compile_field_assignment(compiler, &lvalue.segments, val)?;
            }
        }
        AssignTarget::Pattern(pat) => {
            let Pattern::Binding { name, .. } = pat else {
                return Err("destructuring patterns not yet supported in compilation".to_string());
            };

            let ownership = ownership_for_expr(value, &assigned_type);
            let alloca_type = to_llvm_type(&assigned_type, compiler.context, &compiler.types)
                .unwrap_or(val.get_type());
            let alloca = compiler.build_entry_alloca(alloca_type, name);
            compiler.builder.build_store(alloca, val).unwrap();
            compiler
                .fn_state
                .variables
                .insert(name.clone(), (alloca, assigned_type, ownership));
        }
    }
    Ok(())
}

/// Compiles `target += value` (and other compound assignment operators) by
/// loading the current value, applying the operation, and storing the result.
fn compile_compound_assign<'ctx>(
    compiler: &mut Compiler<'ctx>,
    target: &LValue,
    op: &CompoundOp,
    value: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let (ptr, target_type) = if target.segments.len() == 1 {
        let name = &target.segments[0];
        let (ptr, variable_type, _) = compiler
            .fn_state
            .variables
            .get(name)
            .ok_or_else(|| format!("undefined variable: {name}"))?
            .clone();
        (ptr, variable_type)
    } else {
        resolve_field_ptr(compiler, &target.segments)?
    };

    let llvm_type = to_llvm_type(&target_type, compiler.context, &compiler.types)
        .ok_or("cannot load variable of unsupported type")?;
    let current = compiler.builder.build_load(llvm_type, ptr, "cur").unwrap();
    let rhs = compile_expr(compiler, value, function)?
        .ok_or("compound assignment value produced no value")?
        .value;
    let rhs = coerce_numeric(compiler, rhs, &target_type);

    let shape = if current.is_float_value() && rhs.is_float_value() {
        OperandShape::Float
    } else if current.is_int_value() && rhs.is_int_value() {
        OperandShape::Integer {
            bit_width: current.into_int_value().get_type().get_bit_width(),
        }
    } else {
        return Err("compound assignment requires matching numeric types".to_string());
    };

    let resolved = resolve_compound_op(op, &shape)?;
    let result = apply_compound_op(compiler, &resolved, current, rhs);
    compiler.builder.build_store(ptr, result).unwrap();

    Ok(())
}

/// Applies a resolved compound operation (e.g. `IntAdd`, `FloatMul`) to two
/// LLVM values and returns the result.
fn apply_compound_op<'ctx>(
    compiler: &Compiler<'ctx>,
    resolved: &ResolvedCompoundOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> BasicValueEnum<'ctx> {
    match resolved {
        ResolvedCompoundOp::FloatAdd => compiler
            .builder
            .build_float_add(lhs.into_float_value(), rhs.into_float_value(), "cfadd")
            .unwrap()
            .into(),
        ResolvedCompoundOp::FloatDiv => compiler
            .builder
            .build_float_div(lhs.into_float_value(), rhs.into_float_value(), "cfdiv")
            .unwrap()
            .into(),
        ResolvedCompoundOp::FloatMul => compiler
            .builder
            .build_float_mul(lhs.into_float_value(), rhs.into_float_value(), "cfmul")
            .unwrap()
            .into(),
        ResolvedCompoundOp::FloatSub => compiler
            .builder
            .build_float_sub(lhs.into_float_value(), rhs.into_float_value(), "cfsub")
            .unwrap()
            .into(),
        ResolvedCompoundOp::IntAdd => compiler
            .builder
            .build_int_add(lhs.into_int_value(), rhs.into_int_value(), "cadd")
            .unwrap()
            .into(),
        ResolvedCompoundOp::IntDiv => compiler
            .builder
            .build_int_signed_div(lhs.into_int_value(), rhs.into_int_value(), "cdiv")
            .unwrap()
            .into(),
        ResolvedCompoundOp::IntMul => compiler
            .builder
            .build_int_mul(lhs.into_int_value(), rhs.into_int_value(), "cmul")
            .unwrap()
            .into(),
        ResolvedCompoundOp::IntSub => compiler
            .builder
            .build_int_sub(lhs.into_int_value(), rhs.into_int_value(), "csub")
            .unwrap()
            .into(),
    }
}

/// Resolves a dotted field path to a sequence of field indices and types
/// by walking the type context. No LLVM emission.
pub(crate) fn resolve_field_path(
    compiler: &Compiler,
    segments: &[String],
) -> Result<(Type, Vec<ResolvedFieldStep>), String> {
    let variable_name = &segments[0];
    let (_, variable_type, _) = compiler
        .fn_state
        .variables
        .get(variable_name)
        .ok_or_else(|| format!("undefined variable: {variable_name}"))?
        .clone();

    let mut current_type = variable_type.clone();
    let mut steps = Vec::with_capacity(segments.len() - 1);

    for field_name in &segments[1..] {
        if !matches!(&current_type, Type::Named { .. }) {
            return Err(format!(
                "cannot access field `{field_name}` on non-struct type"
            ));
        }

        let field_index = compiler
            .struct_field_index_for_type(&current_type, field_name)
            .ok_or_else(|| {
                format!(
                    "unknown field `{field_name}` on struct `{}`",
                    current_type.display()
                )
            })?;

        let field_type = compiler
            .struct_field_type_for_type(&current_type, field_name)
            .ok_or_else(|| {
                format!(
                    "unknown field `{field_name}` on struct `{}`",
                    current_type.display()
                )
            })?;

        steps.push(ResolvedFieldStep {
            field_index,
            field_type: field_type.clone(),
        });

        current_type = field_type;
    }

    Ok((variable_type, steps))
}

/// Walks a dotted field path (`self.span.start.line`) and returns the LLVM
/// pointer to the final field plus its Expo type.
fn resolve_field_ptr<'ctx>(
    compiler: &Compiler<'ctx>,
    segments: &[String],
) -> Result<(PointerValue<'ctx>, Type), String> {
    let (base_type, steps) = resolve_field_path(compiler, segments)?;

    let variable_name = &segments[0];
    let (mut ptr, _, _) = compiler
        .fn_state
        .variables
        .get(variable_name)
        .unwrap()
        .clone();

    let mut current_type = base_type;
    for (i, step) in steps.iter().enumerate() {
        let struct_type = to_llvm_type(&current_type, compiler.context, &compiler.types)
            .map(|t| t.into_struct_type())
            .ok_or_else(|| format!("unknown struct type for field path segment {i}"))?;

        ptr = compiler
            .builder
            .build_struct_gep(
                struct_type,
                ptr,
                step.field_index,
                &format!("{variable_name}.{}", segments[i + 1]),
            )
            .unwrap();

        current_type = step.field_type.clone();
    }

    Ok((ptr, current_type))
}

/// Compiles a multi-segment field assignment like `self.x = value`.
fn compile_field_assignment<'ctx>(
    compiler: &mut Compiler<'ctx>,
    segments: &[String],
    val: BasicValueEnum<'ctx>,
) -> Result<(), String> {
    let (ptr, field_type) = resolve_field_ptr(compiler, segments)?;
    let store_val = coerce_numeric(compiler, val, &field_type);
    compiler.builder.build_store(ptr, store_val).unwrap();
    Ok(())
}

/// Extends, truncates, or no-ops integer and float LLVM values so they match
/// the target primitive type when storing or passing values.
pub(crate) fn coerce_numeric<'ctx>(
    compiler: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    target: &Type,
) -> BasicValueEnum<'ctx> {
    let Type::Primitive(target_prim) = target else {
        return val;
    };

    if val.is_int_value() && target_prim.is_integer() {
        let int_value = val.into_int_value();
        let src_bits = int_value.get_type().get_bit_width();
        let dst_bits = int_bit_width(target_prim);
        if src_bits == dst_bits {
            return int_value.into();
        }
        let dst_type = compiler.context.custom_width_int_type(dst_bits);
        if dst_bits < src_bits {
            return compiler
                .builder
                .build_int_truncate(int_value, dst_type, "trunc")
                .unwrap()
                .into();
        }
        let signed = matches!(
            target_prim,
            Primitive::I8 | Primitive::I16 | Primitive::I32 | Primitive::I64
        );
        if signed {
            compiler
                .builder
                .build_int_s_extend(int_value, dst_type, "sext")
                .unwrap()
                .into()
        } else {
            compiler
                .builder
                .build_int_z_extend(int_value, dst_type, "zext")
                .unwrap()
                .into()
        }
    } else if val.is_float_value() && target_prim.is_float() {
        let float_value = val.into_float_value();
        let dst_is_f64 = *target_prim == Primitive::F64;
        if (float_value.get_type() == compiler.context.f64_type()) == dst_is_f64 {
            return float_value.into();
        }
        if dst_is_f64 {
            compiler
                .builder
                .build_float_ext(float_value, compiler.context.f64_type(), "fpext")
                .unwrap()
                .into()
        } else {
            compiler
                .builder
                .build_float_trunc(float_value, compiler.context.f32_type(), "fptrunc")
                .unwrap()
                .into()
        }
    } else {
        val
    }
}

/// Wraps a concrete value into a tagged union representation.
pub(crate) fn compile_union_wrap<'ctx>(
    compiler: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    source: &Type,
    target_union: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    if !matches!(target_union, Type::Union(_)) {
        return Ok(val);
    }

    let resolved = resolve_union_member(compiler, source, target_union)?;

    let alloca = compiler
        .builder
        .build_alloca(resolved.union_type, "union_wrap")
        .unwrap();

    let tag_ptr = compiler
        .builder
        .build_struct_gep(resolved.union_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = compiler.context.i8_type().const_int(resolved.tag, false);
    compiler.builder.build_store(tag_ptr, tag_val).unwrap();

    if resolved.union_type.count_fields() > 1 {
        let payload_ptr = compiler
            .builder
            .build_struct_gep(resolved.union_type, alloca, 1, "payload_ptr")
            .unwrap();
        compiler.builder.build_store(payload_ptr, val).unwrap();
    }

    let result = compiler
        .builder
        .build_load(resolved.union_type, alloca, "union_val")
        .unwrap();
    Ok(result)
}

/// Applies a recorded coercion to a compiled value, if one exists for the
/// given expression span. Currently handles union widening.
pub(crate) fn apply_coercion<'ctx>(
    compiler: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    expr: &Expr,
) -> Result<BasicValueEnum<'ctx>, String> {
    let span = expr_span(expr);
    let Some(coercion) = resolve_coercion(compiler, span) else {
        return Ok(val);
    };
    match coercion {
        Coercion::UnionWiden { source, target } => {
            let target_mangled = mangle_type(&target);
            if let Some(target_llvm) = compiler.types.get_monomorphized(&target_mangled)
                && val.get_type() == target_llvm.into()
            {
                return Ok(val);
            }
            compile_union_wrap(compiler, val, &source, &target)
        }
    }
}

/// Resolves type annotation substitutions needed before compiling the RHS
/// of an assignment. Returns `(param_name, type_arg)` pairs to insert into
/// `type_subst` so generic type parameters are available during compilation.
fn resolve_annotation_subst(
    compiler: &Compiler,
    type_annotation: &TypeExpr,
) -> Vec<(String, Type)> {
    let annotated = compiler.resolve_type_expr(type_annotation);
    match &annotated {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let Some(type_params) = compiler
                .type_ctx
                .get_type(identifier)
                .map(|type_info| type_info.type_params.clone())
            else {
                return Vec::new();
            };
            type_params
                .iter()
                .zip(type_args.iter())
                .map(|(param, arg)| {
                    let concrete = substitute(arg, &compiler.fn_state.type_subst);
                    (param.name.clone(), concrete)
                })
                .collect()
        }
        Type::Pointer(inner) => {
            let Some(type_info) = compiler
                .type_ctx
                .resolve_name("CPtr")
                .and_then(|id| compiler.type_ctx.get_type(id))
            else {
                return Vec::new();
            };
            if type_info.type_params.is_empty() {
                return Vec::new();
            }
            vec![(type_info.type_params[0].name.clone(), *inner.clone())]
        }
        _ => Vec::new(),
    }
}

/// Resolves the final annotated type after the RHS has been compiled,
/// substituting generic type args with their concrete bindings.
fn resolve_final_annotation_type(compiler: &Compiler, type_annotation: &TypeExpr) -> Type {
    let annotated = compiler.resolve_type_expr(type_annotation);
    match annotated {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let resolved_args: Vec<Type> = type_args
                .iter()
                .map(|t| substitute_preserving(t, &compiler.fn_state.type_subst))
                .collect();
            Type::Named {
                identifier,
                type_args: resolved_args,
            }
        }
        other => other,
    }
}

/// Attempts to derive the Expo type directly from the expression AST. Returns
/// `Some(Type::Function{..})` for closures so the variable is stored with the
/// correct callable type rather than being misidentified as a string pointer.
fn infer_type_from_expr(compiler: &Compiler, expr: &Expr) -> Option<Type> {
    if let ExprKind::MethodCall {
        receiver,
        method,
        args,
        ..
    } = &expr.kind
    {
        if let ExprKind::Ident {
            name: type_name, ..
        } = &receiver.kind
        {
            let is_type_name = compiler.type_ctx.resolve_name(type_name).is_some();
            if is_type_name {
                return infer_static_method_return_type(compiler, type_name, method, args);
            }

            if let Some((_, receiver_type, _)) = compiler.fn_state.variables.get(type_name)
                && matches!(receiver_type, Type::Primitive(_))
            {
                let ret = infer_instance_method_return_type(compiler, receiver_type, method);
                if ret.is_some() {
                    return ret;
                }
            }
        }

        let receiver_type = infer_receiver_type(compiler, receiver);
        if let Some(ref resolved_type) = receiver_type
            && matches!(resolved_type, Type::Primitive(_))
        {
            let ret = infer_instance_method_return_type(compiler, resolved_type, method);
            if ret.is_some() {
                return ret;
            }
        }
    }
    if let ExprKind::Closure {
        params,
        return_type,
        ..
    } = &expr.kind
    {
        let param_types: Vec<Type> = params
            .iter()
            .map(|closure_param| match closure_param {
                ClosureParam::Name {
                    type_expr: Some(type_expression),
                    ..
                } => compiler.resolve_type_expr(type_expression),
                _ => Type::Primitive(Primitive::I32),
            })
            .collect();
        let ret = match return_type {
            Some(type_expression) => compiler.resolve_type_expr(type_expression),
            None => Type::Unit,
        };
        return Some(Type::Function {
            params: param_types.into_iter().map(FnParam::borrow).collect(),
            return_type: Box::new(ret),
        });
    }
    if let ExprKind::Ident { name, .. } = &expr.kind
        && let Some(sig) = compiler.type_ctx.functions.get(name)
        && sig.type_params.is_empty()
    {
        return Some(Type::Function {
            params: sig.params.iter().map(FnParam::from).collect(),
            return_type: Box::new(sig.return_type.clone()),
        });
    }
    if let ExprKind::Call { callee, .. } = &expr.kind
        && let ExprKind::Ident { name, .. } = &callee.kind
        && let Some(sig) = compiler.type_ctx.functions.get(name)
        && sig.type_params.is_empty()
    {
        return Some(sig.return_type.clone());
    }
    if matches!(&expr.kind, ExprKind::Receive { .. }) {
        return compiler.fn_state.process_msg_type.clone();
    }
    if let ExprKind::Binary {
        op: BinOp::Concat,
        left,
        ..
    } = &expr.kind
    {
        return infer_type_from_expr(compiler, left).or_else(|| {
            if let ExprKind::Ident { name, .. } = &left.kind {
                compiler
                    .fn_state
                    .variables
                    .get(name)
                    .map(|(_, variable_type, _)| variable_type.clone())
            } else if matches!(&left.kind, ExprKind::BinaryLiteral { .. }) {
                Some(Type::Primitive(Primitive::Binary))
            } else {
                None
            }
        });
    }
    None
}

/// Looks up the return type of an instance method on a given receiver type.
fn infer_instance_method_return_type(
    compiler: &Compiler,
    receiver_type: &Type,
    method: &str,
) -> Option<Type> {
    match receiver_type {
        Type::Primitive(primitive) => compiler
            .type_ctx
            .find_type(primitive.display())
            .and_then(|type_info| type_info.functions.get(method))
            .map(|sig| sig.return_type.clone()),
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                compiler
                    .type_ctx
                    .get_type(identifier)
                    .and_then(|type_info| type_info.functions.get(method))
                    .map(|sig| sig.return_type.clone())
            } else {
                let (methods, type_params) = compiler
                    .type_ctx
                    .get_type(identifier)
                    .map(|type_info| (&type_info.functions, &type_info.type_params))?;
                let sig = methods.get(method)?;
                let subst: HashMap<String, Type> = type_params
                    .iter()
                    .zip(type_args.iter())
                    .map(|(type_param, type_arg)| (type_param.name.clone(), type_arg.clone()))
                    .collect();
                Some(substitute(&sig.return_type, &subst))
            }
        }
        _ => None,
    }
}

/// Infers the Expo type of a receiver expression without compiling it.
fn infer_receiver_type(compiler: &Compiler, expr: &Expr) -> Option<Type> {
    match &expr.kind {
        ExprKind::String { .. } => Some(Type::Primitive(Primitive::String)),
        ExprKind::Literal { value, .. } => match value {
            Literal::Int(_) => Some(Type::Primitive(Primitive::I64)),
            Literal::Float(_) => Some(Type::Primitive(Primitive::F64)),
            Literal::Bool(_) => Some(Type::Primitive(Primitive::Bool)),
            Literal::String(_) => Some(Type::Primitive(Primitive::String)),
            Literal::Unit => Some(Type::Unit),
        },
        ExprKind::Ident { name, .. } => compiler
            .fn_state
            .variables
            .get(name)
            .map(|(_, variable_type, _)| variable_type.clone()),
        ExprKind::MethodCall {
            receiver, method, ..
        } => {
            let receiver_type = infer_receiver_type(compiler, receiver)?;
            infer_instance_method_return_type(compiler, &receiver_type, method)
        }
        ExprKind::Call { callee, .. } => {
            if let ExprKind::Ident { name, .. } = &callee.kind {
                compiler
                    .type_ctx
                    .functions
                    .get(name)
                    .map(|sig| sig.return_type.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// When a list literal `[a, b, c]` is assigned to a non-List type that
/// implements `ListLiteral<T>` (e.g. `Set<T>`), calls `from_list` to convert.
fn convert_list_literal_if_needed<'ctx>(
    compiler: &mut Compiler<'ctx>,
    list_val: BasicValueEnum<'ctx>,
    target_type: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    let (base, identifier, type_args) = match target_type {
        Type::Named {
            identifier,
            type_args,
        } if identifier.name != "List" && !type_args.is_empty() => (
            identifier.name.clone(),
            identifier.clone(),
            type_args.clone(),
        ),
        _ => return Ok(list_val),
    };

    let target_mangled = mangle_name(&identifier, &type_args);
    let from_list_fn_name = format!("{target_mangled}_from_list");
    if !compiler.functions.contains_key(&from_list_fn_name) {
        monomorphize_impl_method(compiler, &base, "from_list", &type_args, &[])?;
    }
    let from_list_fn = *compiler
        .functions
        .get(&from_list_fn_name)
        .ok_or_else(|| format!("{base} does not implement ListLiteral (no from_list)"))?;

    let result = compiler
        .call(from_list_fn, &[list_val.into()], "from_list")
        .ok_or("from_list returned void")?;

    Ok(result)
}

/// Resolved tag index and LLVM struct type for wrapping a value into a union.
struct ResolvedUnionMember<'ctx> {
    tag: u64,
    union_type: StructType<'ctx>,
}

/// Finds the tag index and LLVM type for wrapping `source` into `target_union`.
fn resolve_union_member<'ctx>(
    compiler: &Compiler<'ctx>,
    source: &Type,
    target_union: &Type,
) -> Result<ResolvedUnionMember<'ctx>, String> {
    let Type::Union(members) = target_union else {
        return Err("resolve_union_member called with non-union target".to_string());
    };

    let source_mangled = mangle_type(source);
    let union_mangled = mangle_type(target_union);

    let tag = members
        .iter()
        .position(|m| mangle_type(m) == source_mangled)
        .ok_or_else(|| {
            format!(
                "{} is not a member of union {}",
                source.display(),
                target_union.display()
            )
        })? as u64;

    let union_type = compiler
        .types
        .get_monomorphized(&union_mangled)
        .ok_or_else(|| format!("union type {} not registered", union_mangled))?;

    Ok(ResolvedUnionMember { tag, union_type })
}

/// Looks up a recorded coercion for the given span from the type context.
fn resolve_coercion(compiler: &Compiler, span: Span) -> Option<Coercion> {
    compiler.type_ctx.coercions.get(&span).cloned()
}

/// Returns the LLVM bit width for an integer primitive type.
fn int_bit_width(primitive: &Primitive) -> u32 {
    match primitive {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 => 64,
        _ => 0,
    }
}

/// Extracts the source span from a statement for debug location tracking.
fn statement_span(stmt: &Statement) -> Span {
    match stmt {
        Statement::Expr(expr) => expr_span(expr),
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => *span,
    }
}

/// Extracts the source span from an expression.
fn expr_span(expr: &Expr) -> Span {
    expr.span
}

/// Determines ownership semantics for an assigned value based on its
/// expression kind and type (e.g. string literals are unowned, constructed
/// values are owned).
fn ownership_for_expr(expr: &Expr, assigned_type: &Type) -> Ownership {
    if is_concat_expr(expr) {
        return Ownership::Owned;
    }
    if matches!(
        assigned_type,
        Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits)
    ) {
        return match &expr.kind {
            ExprKind::BinaryLiteral { .. } => Ownership::Owned,
            ExprKind::Receive { .. } => Ownership::Owned,
            _ => Ownership::Unowned,
        };
    }
    if !matches!(assigned_type, Type::Primitive(Primitive::String)) {
        return Ownership::Owned;
    }
    match &expr.kind {
        ExprKind::String { parts, .. } => {
            let has_interpolation = parts
                .iter()
                .any(|part| matches!(part, StringPart::Interpolation { .. }));
            if has_interpolation {
                Ownership::Owned
            } else {
                Ownership::Unowned
            }
        }
        ExprKind::Receive { .. } => Ownership::Owned,
        _ => Ownership::Unowned,
    }
}

/// Returns `true` if the expression is a binary concat operation (`<>`).
fn is_concat_expr(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Binary {
            op: BinOp::Concat,
            ..
        }
    )
}
