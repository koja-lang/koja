//! Conditional compilation: if/else, unless, cond, and ternary expressions.

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_typecheck::types::Type;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::stmt::compile_statement;

use super::{coerce_to_bool, compile_body_as_value};

/// Compiles a `cond` expression (multi-arm conditional). Each arm's condition is
/// tested in order; the first truthy branch executes. Returns a phi value when
/// all arms (including `else`) produce a value of the same type.
pub fn compile_cond<'ctx>(
    c: &mut Compiler<'ctx>,
    arms: &[CondArm],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if arms.is_empty() && else_body.is_none() {
        return Ok(None);
    }

    let merge_bb = c.context.append_basic_block(function, "cond_end");
    let fallthrough_bb = c.context.append_basic_block(function, "cond_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();
    let mut branch_expo_type: Option<Type> = None;

    for (i, arm) in arms.iter().enumerate() {
        let cond_val = compile_expr(c, &arm.condition, function)?
            .ok_or("cond arm produced no value")?
            .value;
        let cond_int = coerce_to_bool(c, cond_val, "cond arm condition")?;

        let body_bb = c
            .context
            .append_basic_block(function, &format!("cond_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            c.context
                .append_basic_block(function, &format!("cond_check_{}", i + 1))
        } else {
            fallthrough_bb
        };

        c.builder
            .build_conditional_branch(cond_int, body_bb, next_bb)
            .unwrap();

        c.builder.position_at_end(body_bb);
        let arm_tv = compile_body_as_value(c, &arm.body, function)?;
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }
        let arm_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(tv) = arm_tv {
            if branch_expo_type.is_none() {
                branch_expo_type = Some(tv.expo_type.clone());
            }
            incoming.push((tv.value, arm_end_bb));
        }

        if next_bb != merge_bb && next_bb != fallthrough_bb {
            c.builder.position_at_end(next_bb);
        }
    }

    c.builder.position_at_end(fallthrough_bb);
    if let Some(body) = else_body {
        let else_tv = compile_body_as_value(c, body, function)?;
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }
        let else_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(tv) = else_tv {
            if branch_expo_type.is_none() {
                branch_expo_type = Some(tv.expo_type.clone());
            }
            incoming.push((tv.value, else_end_bb));
        }
    } else {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }

    c.builder.position_at_end(merge_bb);

    let expected_sources = arms.len() + if else_body.is_some() { 1 } else { 0 };
    if !incoming.is_empty() && incoming.len() == expected_sources {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let phi = c.builder.build_phi(first_ty, "condval").unwrap();
            for (v, bb) in &incoming {
                phi.add_incoming(&[(v, *bb)]);
            }
            let result_type = branch_expo_type.unwrap_or(Type::Unknown);
            return Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)));
        }
    }

    Ok(None)
}

/// Compiles an `if`/`else` expression. Returns a phi value when both branches
/// produce a value of the same type, otherwise returns `None`.
pub fn compile_if<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[Statement],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let cond_val = compile_expr(c, condition, function)?
        .ok_or("if condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(c, cond_val, "if condition")?;

    let then_bb = c.context.append_basic_block(function, "then");
    let else_bb = c.context.append_basic_block(function, "else");
    let merge_bb = c.context.append_basic_block(function, "ifcont");

    c.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    let then_tv = compile_body_as_value(c, then_body, function)?;
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let then_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(else_bb);
    let else_tv = if let Some(else_stmts) = else_body {
        compile_body_as_value(c, else_stmts, function)?
    } else {
        None
    };
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let else_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(merge_bb);

    if let (Some(then_tv), Some(else_tv)) = (&then_tv, &else_tv)
        && then_tv.value.get_type() == else_tv.value.get_type()
    {
        let phi = c
            .builder
            .build_phi(then_tv.value.get_type(), "ifval")
            .unwrap();
        phi.add_incoming(&[(&then_tv.value, then_end_bb), (&else_tv.value, else_end_bb)]);
        let result_type = then_tv.expo_type.clone();
        return Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)));
    }

    Ok(None)
}

/// Compiles an `unless` guard: `unless cond ... end`. Negates the condition
/// and delegates to `compile_if` with no else branch.
pub fn compile_unless<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let cond_val = compile_expr(c, condition, function)?
        .ok_or("unless condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(c, cond_val, "unless condition")?;
    let negated = c.builder.build_not(cond_int, "unless_neg").unwrap();

    let then_bb = c.context.append_basic_block(function, "unless_body");
    let merge_bb = c.context.append_basic_block(function, "unless_end");

    c.builder
        .build_conditional_branch(negated, then_bb, merge_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }

    c.builder.position_at_end(merge_bb);
    Ok(None)
}

/// Compiles a ternary expression (`condition ? then_expr : else_expr`).
/// Always value-producing when both branches yield the same type.
pub fn compile_ternary<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let cond_val = compile_expr(c, condition, function)?
        .ok_or("ternary condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(c, cond_val, "ternary condition")?;

    let then_bb = c.context.append_basic_block(function, "tern_then");
    let else_bb = c.context.append_basic_block(function, "tern_else");
    let merge_bb = c.context.append_basic_block(function, "tern_cont");

    c.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    let then_tv = compile_expr(c, then_expr, function)?;
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let then_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(else_bb);
    let else_tv = compile_expr(c, else_expr, function)?;
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let else_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(merge_bb);

    if let (Some(ttv), Some(etv)) = (&then_tv, &else_tv)
        && ttv.value.get_type() == etv.value.get_type()
    {
        let phi = c
            .builder
            .build_phi(ttv.value.get_type(), "ternval")
            .unwrap();
        phi.add_incoming(&[(&ttv.value, then_end_bb), (&etv.value, else_end_bb)]);
        return Ok(Some(TypedValue::new(
            phi.as_basic_value(),
            ttv.expo_type.clone(),
        )));
    }

    Ok(None)
}
