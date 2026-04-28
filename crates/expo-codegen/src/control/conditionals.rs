//! Conditional compilation: if/else, unless, cond, and ternary expressions.

use std::collections::HashMap;

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_ir::IRBlockId;
use expo_ir::resolved::conditionals::{IRIf, IRIfElse, IRTernary, IRUnless};
use expo_ir::values::{IRInstruction, IRValueId};
use expo_typecheck::types::Type;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::stmt::compile_statement;

use super::instructions::execute_instructions;
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
/// Slice 3 split: both forms now route through the IR pipeline.
///
/// - No-else form (`if cond ... end`) lowers to an [`IRIf`] via
///   [`expo_ir::Lowerer::lower_if_no_else`] and walks via
///   [`emit_if`] -- same machinery as `compile_unless`, polarity
///   flipped in lowering's slot assignment.
/// - With-else form (`if cond ... else ... end`) lowers to an
///   [`IRIfElse`] via [`expo_ir::Lowerer::lower_if_else`] and walks
///   via [`emit_if_else`]. The merge phi is synthesized at emit
///   time when both arms produce a value (mirrors the legacy
///   `Ok(None)` fall-through when either arm is statement-only).
///
/// `resolved_type` is the parent [`expo_ast::ast::Expr`]'s
/// typecheck-resolved type when available; threaded into lowering
/// for documentation / future-proofing of `IRIfElse::merge_phi_ty`.
/// The actual phi LLVM type is derived from the then-arm's compiled
/// value at emit time, so a `None` here doesn't break the value
/// path.
pub fn compile_if<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[Statement],
    else_body: &Option<Vec<Statement>>,
    resolved_type: Option<&Type>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let Some(else_stmts) = else_body else {
        let ir = compiler.lowerer().lower_if_no_else(condition, then_body);
        return emit_if(compiler, &ir, function);
    };

    let merge_phi_ty = resolved_type.cloned().unwrap_or(Type::Unknown);
    let ir = compiler
        .lowerer()
        .lower_if_else(condition, then_body, else_stmts, merge_phi_ty);
    emit_if_else(compiler, &ir, function)
}

/// Compiles an `unless` guard: `unless cond ... end`. Lowers to an
/// [`IRUnless`] via [`expo_ir::Lowerer::lower_unless`] and walks the
/// result via [`emit_unless`].
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
    let ir = compiler.lowerer().lower_unless(condition, body);
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

    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    execute_instructions(
        compiler,
        &ir.entry_instructions,
        function,
        None,
        &mut value_map,
    )?;
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

    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    execute_instructions(
        compiler,
        &ir.entry_instructions,
        function,
        None,
        &mut value_map,
    )?;
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

/// Walks an [`IRIfElse`] (with-else form) into LLVM IR.
///
/// Allocates LLVM blocks for the then / else / merge IR ids,
/// executes the entry instruction sequence (no Phi possible there),
/// dispatches the canonicalized entry `CondBranch`, then walks each
/// arm's AST statements via [`compile_body_as_value`] -- which
/// returns the trailing-expression value when the arm ends in an
/// expression statement and otherwise leaves the arm
/// statement-shaped.
///
/// The merge phi is synthesized inline (rather than via
/// [`execute_instructions`]) for two reasons: the actual end blocks
/// of each arm are known only after walking the AST stubs (nested
/// control flow can move the builder past `then_bb` / `else_bb`),
/// and the arms may diverge or be statement-only, in which case the
/// construct returns `Ok(None)` instead of producing a phi --
/// matching the legacy `compile_if` semantics. The pre-allocated
/// `ir.merge_phi_dest` and `ir.merge_phi_ty` carry forward to the
/// later slice that lifts statement-level lowering, when the phi
/// can be pre-staged like ternary's.
fn emit_if_else<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRIfElse,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let then_bb = compiler.context.append_basic_block(function, "then");
    let else_bb = compiler.context.append_basic_block(function, "else");
    let merge_bb = compiler.context.append_basic_block(function, "ifcont");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.then_block, then_bb);
    block_map.insert(ir.else_block, else_bb);
    block_map.insert(ir.merge_block, merge_bb);

    let mut entry_value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    execute_instructions(
        compiler,
        &ir.entry_instructions,
        function,
        None,
        &mut entry_value_map,
    )?;
    emit_terminator(
        compiler,
        &ir.entry_terminator,
        &block_map,
        &entry_value_map,
        function,
    )?;

    compiler.builder.position_at_end(then_bb);
    let (then_tv, then_end_bb) = walk_arm_value(compiler, &ir.then_stmts, function)?;
    if !compiler.current_block_terminated() {
        let body_value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
        emit_terminator(
            compiler,
            &ir.then_terminator,
            &block_map,
            &body_value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(else_bb);
    let (else_tv, else_end_bb) = walk_arm_value(compiler, &ir.else_stmts, function)?;
    if !compiler.current_block_terminated() {
        let body_value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
        emit_terminator(
            compiler,
            &ir.else_terminator,
            &block_map,
            &body_value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(merge_bb);

    if let (Some(then_tv), Some(else_tv)) = (&then_tv, &else_tv)
        && then_tv.value.get_type() == else_tv.value.get_type()
    {
        let phi = compiler
            .builder
            .build_phi(then_tv.value.get_type(), "ifval")
            .unwrap();
        phi.add_incoming(&[(&then_tv.value, then_end_bb), (&else_tv.value, else_end_bb)]);
        return Ok(Some(TypedValue::new(
            phi.as_basic_value(),
            then_tv.expo_type.clone(),
        )));
    }

    Ok(None)
}

/// Walks an [`IRTernary`] into LLVM IR.
///
/// Same skeleton as [`emit_if_else`] -- allocate three blocks,
/// execute entry, dispatch entry terminator, walk both arms,
/// position at merge -- but each arm executes a pre-instructionized
/// sequence (no AST stubs survived lowering) and the merge runs
/// through [`execute_instructions`] with the pre-staged
/// [`expo_ir::values::IRInstruction::Phi`] in `merge_instructions`.
/// The block map is updated after each arm so the executor can
/// resolve the phi's incoming edges to the *actual* end blocks
/// (nested control flow inside an arm can move the builder past
/// the arm's nominal block).
///
/// Ternary always produces a value (typecheck rejects mismatched
/// arms), so unlike [`emit_if_else`] there's no `Ok(None)`
/// fall-through; either both arms diverge (in which case the
/// builder is parked on a dead merge block and the surrounding
/// flow handles it) or the phi materializes a `TypedValue`.
fn emit_ternary<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRTernary,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let then_bb = compiler.context.append_basic_block(function, "tern_then");
    let else_bb = compiler.context.append_basic_block(function, "tern_else");
    let merge_bb = compiler.context.append_basic_block(function, "tern_cont");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.then_block, then_bb);
    block_map.insert(ir.else_block, else_bb);
    block_map.insert(ir.merge_block, merge_bb);

    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    execute_instructions(
        compiler,
        &ir.entry_instructions,
        function,
        None,
        &mut value_map,
    )?;
    emit_terminator(
        compiler,
        &ir.entry_terminator,
        &block_map,
        &value_map,
        function,
    )?;

    compiler.builder.position_at_end(then_bb);
    execute_instructions(
        compiler,
        &ir.then_instructions,
        function,
        None,
        &mut value_map,
    )?;
    let then_end_bb = compiler.builder.get_insert_block().unwrap();
    if !compiler.current_block_terminated() {
        emit_terminator(
            compiler,
            &ir.then_terminator,
            &block_map,
            &value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(else_bb);
    execute_instructions(
        compiler,
        &ir.else_instructions,
        function,
        None,
        &mut value_map,
    )?;
    let else_end_bb = compiler.builder.get_insert_block().unwrap();
    if !compiler.current_block_terminated() {
        emit_terminator(
            compiler,
            &ir.else_terminator,
            &block_map,
            &value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(merge_bb);
    block_map.insert(ir.then_block, then_end_bb);
    block_map.insert(ir.else_block, else_end_bb);
    execute_instructions(
        compiler,
        &ir.merge_instructions,
        function,
        Some(&block_map),
        &mut value_map,
    )?;

    let value = value_map.get(&ir.merge_value).copied().ok_or(
        "IRTernary: merge phi did not register an LLVM value (both arms may have diverged)",
    )?;
    let result_type = ir
        .merge_instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::Phi { ty, .. } => Some(ty.clone()),
            _ => None,
        })
        .unwrap_or(Type::Unknown);
    Ok(Some(TypedValue::new(value, result_type)))
}

/// Walk a body's AST statements with the builder positioned at the
/// arm's entry block, capturing the trailing-expression value (if
/// the body ends in an expression statement) and the actual LLVM
/// block where control sits when the arm finishes. Nested control
/// flow inside the body can move the builder past the entry block,
/// so the captured end block may differ from where we started.
fn walk_arm_value<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<(Option<TypedValue<'ctx>>, BasicBlock<'ctx>), String> {
    let tv = compile_body_as_value(compiler, body, function)?;
    let end_bb = compiler.builder.get_insert_block().unwrap();
    Ok((tv, end_bb))
}

/// Compiles a ternary expression (`condition ? then_expr : else_expr`).
///
/// Lowers to an [`IRTernary`] via
/// [`expo_ir::Lowerer::lower_ternary`] and walks via
/// [`emit_ternary`]. Both arms are pure expressions, so lowering
/// fully instructionizes them and pre-stages the merge
/// [`expo_ir::values::IRInstruction::Phi`]. Typecheck guarantees
/// the two arms unify; `resolved_type` carries that resolved type
/// from the parent [`expo_ast::ast::Expr`].
pub fn compile_ternary<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    resolved_type: &Type,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let ir =
        compiler
            .lowerer()
            .lower_ternary(condition, then_expr, else_expr, resolved_type.clone());
    emit_ternary(compiler, &ir, function)
}
