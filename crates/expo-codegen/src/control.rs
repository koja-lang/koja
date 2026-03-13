use expo_ast::ast::{Expr, Statement};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::stmt::compile_statement;

pub fn compile_if<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[Statement],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let cond_val = compile_expr(c, condition, function)?.ok_or("if condition produced no value")?;

    let cond_int = if cond_val.is_int_value() {
        let iv = cond_val.into_int_value();
        if iv.get_type().get_bit_width() == 1 {
            iv
        } else {
            c.builder
                .build_int_compare(IntPredicate::NE, iv, iv.get_type().const_zero(), "ifcond")
                .unwrap()
        }
    } else {
        return Err("if condition must be a boolean".to_string());
    };

    let then_bb = c.context.append_basic_block(function, "then");
    let else_bb = c.context.append_basic_block(function, "else");
    let merge_bb = c.context.append_basic_block(function, "ifcont");

    c.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    let mut then_val: Option<BasicValueEnum> = None;
    for (i, stmt) in then_body.iter().enumerate() {
        if c.current_block_terminated() {
            break;
        }
        if i == then_body.len() - 1
            && let Statement::Expr(expr) = stmt
        {
            then_val = compile_expr(c, expr, function)?;
            continue;
        }
        compile_statement(c, stmt, function)?;
    }
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let then_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(else_bb);
    let mut else_val: Option<BasicValueEnum> = None;
    if let Some(else_stmts) = else_body {
        for (i, stmt) in else_stmts.iter().enumerate() {
            if c.current_block_terminated() {
                break;
            }
            if i == else_stmts.len() - 1
                && let Statement::Expr(expr) = stmt
            {
                else_val = compile_expr(c, expr, function)?;
                continue;
            }
            compile_statement(c, stmt, function)?;
        }
    }
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let else_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(merge_bb);

    if let (Some(tv), Some(ev)) = (&then_val, &else_val)
        && tv.get_type() == ev.get_type()
    {
        let phi = c.builder.build_phi(tv.get_type(), "ifval").unwrap();
        phi.add_incoming(&[(tv, then_end_bb), (ev, else_end_bb)]);
        return Ok(Some(phi.as_basic_value()));
    }

    Ok(None)
}

pub fn compile_loop<'ctx>(
    c: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let loop_header = c.context.append_basic_block(function, "loop_header");
    let loop_body = c.context.append_basic_block(function, "loop_body");
    let loop_exit = c.context.append_basic_block(function, "loop_exit");

    c.builder.build_unconditional_branch(loop_header).unwrap();

    c.builder.position_at_end(loop_header);
    c.builder.build_unconditional_branch(loop_body).unwrap();

    c.builder.position_at_end(loop_body);
    c.loop_exit_stack.push(loop_exit);

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(loop_header).unwrap();
    }

    c.loop_exit_stack.pop();
    c.builder.position_at_end(loop_exit);

    Ok(None)
}
