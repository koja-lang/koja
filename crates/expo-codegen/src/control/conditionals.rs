//! Conditional compilation: if/else, unless, cond, and ternary expressions.

use std::collections::HashMap;

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_ir::IRBlockId;
use expo_ir::lower::conditionals::{lower_if_no_else, lower_unless};
use expo_ir::resolved::conditionals::{IRIf, IRUnless};
use expo_ir::values::{IRInstruction, IRValueId};
use expo_typecheck::types::Type;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::stmt::compile_statement;

use super::{coerce_to_bool, compile_body_as_value, emit_terminator};

/// Compiles a `cond` expression (multi-arm conditional). Each arm's condition is
/// tested in order; the first truthy branch executes. Returns a phi value when
/// all arms (including `else`) produce a value of the same type.
pub fn compile_cond<'ctx>(
    compiler: &mut Compiler<'ctx>,
    arms: &[CondArm],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if arms.is_empty() && else_body.is_none() {
        return Ok(None);
    }

    let merge_bb = compiler.context.append_basic_block(function, "cond_end");
    let fallthrough_bb = compiler.context.append_basic_block(function, "cond_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();
    let mut branch_expo_type: Option<Type> = None;

    for (i, arm) in arms.iter().enumerate() {
        let cond_val = compile_expr(compiler, &arm.condition, function)?
            .ok_or("cond arm produced no value")?
            .value;
        let cond_int = coerce_to_bool(compiler, cond_val, "cond arm condition")?;

        let body_bb = compiler
            .context
            .append_basic_block(function, &format!("cond_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            compiler
                .context
                .append_basic_block(function, &format!("cond_check_{}", i + 1))
        } else {
            fallthrough_bb
        };

        compiler
            .builder
            .build_conditional_branch(cond_int, body_bb, next_bb)
            .unwrap();

        compiler.builder.position_at_end(body_bb);
        let arm_tv = compile_body_as_value(compiler, &arm.body, function)?;
        if !compiler.current_block_terminated() {
            compiler
                .builder
                .build_unconditional_branch(merge_bb)
                .unwrap();
        }
        let arm_end_bb = compiler.builder.get_insert_block().unwrap();
        if let Some(tv) = arm_tv {
            if branch_expo_type.is_none() {
                branch_expo_type = Some(tv.expo_type.clone());
            }
            incoming.push((tv.value, arm_end_bb));
        }

        if next_bb != merge_bb && next_bb != fallthrough_bb {
            compiler.builder.position_at_end(next_bb);
        }
    }

    compiler.builder.position_at_end(fallthrough_bb);
    if let Some(body) = else_body {
        let else_tv = compile_body_as_value(compiler, body, function)?;
        if !compiler.current_block_terminated() {
            compiler
                .builder
                .build_unconditional_branch(merge_bb)
                .unwrap();
        }
        let else_end_bb = compiler.builder.get_insert_block().unwrap();
        if let Some(tv) = else_tv {
            if branch_expo_type.is_none() {
                branch_expo_type = Some(tv.expo_type.clone());
            }
            incoming.push((tv.value, else_end_bb));
        }
    } else {
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
    }

    compiler.builder.position_at_end(merge_bb);

    let expected_sources = arms.len() + if else_body.is_some() { 1 } else { 0 };
    if !incoming.is_empty() && incoming.len() == expected_sources {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let phi = compiler.builder.build_phi(first_ty, "condval").unwrap();
            for (v, bb) in &incoming {
                phi.add_incoming(&[(v, *bb)]);
            }
            let result_type = branch_expo_type.unwrap_or(Type::Unknown);
            return Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)));
        }
    }

    if !incoming.is_empty() && incoming.len() != expected_sources {
        return Err(format!(
            "cond arms have inconsistent types: {} of {} arms produce a value",
            incoming.len(),
            expected_sources
        ));
    }

    Ok(None)
}

/// Compiles an `if` / `else` expression.
///
/// Slice 2 split: the no-else form (`if cond ... end`) lowers to an
/// [`IRIf`] via [`lower_if_no_else`] and walks the result via
/// [`emit_if`] -- same machinery as `compile_unless`, polarity flipped
/// in lowering's slot assignment. The else-bearing form is a Shape 2
/// construct (two body blocks plus a value merge) and stays on the
/// existing AST-bound implementation below until slice 3.
pub fn compile_if<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[Statement],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let Some(else_stmts) = else_body else {
        let ir = lower_if_no_else(&mut compiler.fn_lower, condition, then_body);
        return emit_if(compiler, &ir, function);
    };

    let cond_val = compile_expr(compiler, condition, function)?
        .ok_or("if condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(compiler, cond_val, "if condition")?;

    let then_bb = compiler.context.append_basic_block(function, "then");
    let else_bb = compiler.context.append_basic_block(function, "else");
    let merge_bb = compiler.context.append_basic_block(function, "ifcont");

    compiler
        .builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    compiler.builder.position_at_end(then_bb);
    let then_tv = compile_body_as_value(compiler, then_body, function)?;
    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
    }
    let then_end_bb = compiler.builder.get_insert_block().unwrap();

    compiler.builder.position_at_end(else_bb);
    let else_tv = compile_body_as_value(compiler, else_stmts, function)?;
    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
    }
    let else_end_bb = compiler.builder.get_insert_block().unwrap();

    compiler.builder.position_at_end(merge_bb);

    if let (Some(then_tv), Some(else_tv)) = (&then_tv, &else_tv)
        && then_tv.value.get_type() == else_tv.value.get_type()
    {
        let phi = compiler
            .builder
            .build_phi(then_tv.value.get_type(), "ifval")
            .unwrap();
        phi.add_incoming(&[(&then_tv.value, then_end_bb), (&else_tv.value, else_end_bb)]);
        let result_type = then_tv.expo_type.clone();
        return Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)));
    }

    Ok(None)
}

/// Compiles an `unless` guard: `unless cond ... end`. Lowers to an
/// [`IRUnless`] via [`lower_unless`] and walks the result via
/// [`emit_unless`].
///
/// Lowering decides which block runs on truthy vs falsy conditions by
/// placing the body block on the entry `CondBranch`'s `otherwise`
/// slot; emission interprets that decision without any per-construct
/// branch-direction knowledge, so no `build_not(cond)` call is needed.
pub fn compile_unless<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let ir = lower_unless(&mut compiler.fn_lower, condition, body);
    emit_unless(compiler, &ir, function)
}

/// Walks an [`IRUnless`] into LLVM IR.
///
/// Allocates LLVM basic blocks for the body and merge `IRBlockId`s,
/// executes the entry block's instruction sequence (today: zero or
/// one [`IRInstruction::Stub`] for the cond), then dispatches the
/// entry `CondBranch` from the current builder position via the
/// shared [`emit_terminator`]. Walks the body's AST statement stubs
/// next, then honors the declared `Branch(merge)` body terminator iff
/// the body has not already self-terminated. Leaves the builder
/// positioned at the merge block on exit and always returns `Ok(None)`
/// (statement-context construct).
fn emit_unless<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRUnless,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let body_bb = compiler.context.append_basic_block(function, "unless_body");
    let merge_bb = compiler.context.append_basic_block(function, "unless_end");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.body_block, body_bb);
    block_map.insert(ir.merge_block, merge_bb);

    let value_map = execute_instructions(compiler, &ir.entry_instructions, function)?;
    emit_terminator(
        compiler,
        &ir.entry_terminator,
        &block_map,
        &value_map,
        function,
    )?;

    compiler.builder.position_at_end(body_bb);
    for stmt in &ir.body_stmts {
        if compiler.current_block_terminated() {
            break;
        }
        compile_statement(compiler, stmt, function)?;
    }
    if !compiler.current_block_terminated() {
        let body_value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
        emit_terminator(
            compiler,
            &ir.body_terminator,
            &block_map,
            &body_value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(merge_bb);
    Ok(None)
}

/// Walks an [`IRIf`] (no-else form) into LLVM IR.
///
/// Mirror of [`emit_unless`]: same allocation / execute /
/// dispatch / walk / dispatch / position sequence, with `IRIf`
/// field accesses and `then` / `ifcont` LLVM block labels in place
/// of `unless_body` / `unless_end`. The polarity difference between
/// the two constructs is fully encoded in lowering's slot
/// assignment on `entry_terminator`; emission is polarity-blind.
///
/// The duplication relative to [`emit_unless`] is the cost of the
/// slice 2 commitment to direct construct names; both walkers
/// dissolve in slice 5+ when [`expo_ir::IRBasicBlock`] is promoted
/// to first-class and `body_stmts` retires (statement-level
/// lowering). The truly construct-agnostic mechanic
/// ([`execute_instructions`]) is already shared.
fn emit_if<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRIf,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let body_bb = compiler.context.append_basic_block(function, "then");
    let merge_bb = compiler.context.append_basic_block(function, "ifcont");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.body_block, body_bb);
    block_map.insert(ir.merge_block, merge_bb);

    let value_map = execute_instructions(compiler, &ir.entry_instructions, function)?;
    emit_terminator(
        compiler,
        &ir.entry_terminator,
        &block_map,
        &value_map,
        function,
    )?;

    compiler.builder.position_at_end(body_bb);
    for stmt in &ir.body_stmts {
        if compiler.current_block_terminated() {
            break;
        }
        compile_statement(compiler, stmt, function)?;
    }
    if !compiler.current_block_terminated() {
        let body_value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
        emit_terminator(
            compiler,
            &ir.body_terminator,
            &block_map,
            &body_value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(merge_bb);
    Ok(None)
}

/// Execute a block's instruction sequence into LLVM, building the
/// value map that subsequent operands (typically the block's
/// terminator) resolve [`IROperand::Local`] references against.
///
/// Today the only instruction variant is
/// [`IRInstruction::Stub`], which delegates back to
/// [`compile_expr`] on the carried AST. Each future Expr kind that
/// learns to lower replaces its `Stub` site with a typed instruction
/// variant; this dispatch grows correspondingly.
fn execute_instructions<'ctx>(
    compiler: &mut Compiler<'ctx>,
    instructions: &[IRInstruction],
    function: FunctionValue<'ctx>,
) -> Result<HashMap<IRValueId, BasicValueEnum<'ctx>>, String> {
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    for instruction in instructions {
        match instruction {
            IRInstruction::Stub { dest, expr } => {
                let value = compile_expr(compiler, expr, function)?
                    .ok_or("instruction stub expression produced no value")?
                    .value;
                value_map.insert(*dest, value);
            }
        }
    }
    Ok(value_map)
}

/// Compiles a ternary expression (`condition ? then_expr : else_expr`).
/// Always value-producing when both branches yield the same type.
pub fn compile_ternary<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let cond_val = compile_expr(compiler, condition, function)?
        .ok_or("ternary condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(compiler, cond_val, "ternary condition")?;

    let then_bb = compiler.context.append_basic_block(function, "tern_then");
    let else_bb = compiler.context.append_basic_block(function, "tern_else");
    let merge_bb = compiler.context.append_basic_block(function, "tern_cont");

    compiler
        .builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    compiler.builder.position_at_end(then_bb);
    let then_tv = compile_expr(compiler, then_expr, function)?;
    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
    }
    let then_end_bb = compiler.builder.get_insert_block().unwrap();

    compiler.builder.position_at_end(else_bb);
    let else_tv = compile_expr(compiler, else_expr, function)?;
    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
    }
    let else_end_bb = compiler.builder.get_insert_block().unwrap();

    compiler.builder.position_at_end(merge_bb);

    if let (Some(ttv), Some(etv)) = (&then_tv, &else_tv)
        && ttv.value.get_type() == etv.value.get_type()
    {
        let phi = compiler
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
