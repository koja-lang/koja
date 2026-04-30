//! Conditional compilation: if/else, unless, cond, and ternary expressions.

use std::collections::HashMap;

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_ir::IRBlockId;
use expo_ir::resolved::conditionals::{IRCond, IRIf, IRIfElse, IRTernary, IRUnless};
use expo_ir::values::{IRInstruction, IRValueId};
use expo_typecheck::types::Type;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};

use super::instructions::execute_instructions;
use super::terminator::emit_terminator;
use super::{register_block, walk_body};

/// Compiles a `cond` expression (multi-arm conditional).
///
/// Lowers to an [`IRCond`] via [`expo_ir::Lowerer::lower_cond`] and
/// walks via [`emit_cond`]. N-arm generalization of the shape-2
/// conditional pattern from
/// [`compile_if`]'s with-else branch: arm bodies remain AST stubs
/// (until Phase 4g), the merge phi is synthesized inline at emit
/// time when every arm + else (when present) produces a matching
/// value. The empty-and-no-else case short-circuits at the shim
/// before lowering, matching legacy behavior.
///
/// `resolved_type` is the parent [`expo_ast::ast::Expr`]'s
/// typecheck-resolved type when available; threaded into lowering
/// for `IRCond::merge_phi_ty`. The actual phi LLVM type is derived
/// from the first arm's compiled value at emit time, so a `None`
/// here doesn't break the value path.
pub fn compile_cond<'ctx>(
    compiler: &mut Compiler<'ctx>,
    arms: &[CondArm],
    else_body: &Option<Vec<Statement>>,
    resolved_type: Option<&Type>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if arms.is_empty() && else_body.is_none() {
        return Ok(None);
    }

    let merge_phi_ty = resolved_type.cloned().unwrap_or(Type::Unknown);
    let ir = compiler
        .lowerer()
        .lower_cond(arms, else_body.as_deref(), merge_phi_ty)?;
    emit_cond(compiler, &ir, function)
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
        let ir = compiler.lowerer().lower_if_no_else(condition, then_body)?;
        return emit_if(compiler, &ir, function);
    };

    let merge_phi_ty = resolved_type.cloned().unwrap_or(Type::Unknown);
    let ir = compiler
        .lowerer()
        .lower_if_else(condition, then_body, else_stmts, merge_phi_ty)?;
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
    let ir = compiler.lowerer().lower_unless(condition, body)?;
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
    let body_bb = register_block(compiler, function, ir.body.id, "unless_body");
    let merge_bb = register_block(compiler, function, ir.merge_block, "unless_end");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.body.id, body_bb);
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
    walk_body(compiler, &ir.body, &block_map, function)?;

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
    let body_bb = register_block(compiler, function, ir.body.id, "then");
    let merge_bb = register_block(compiler, function, ir.merge_block, "ifcont");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.body.id, body_bb);
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
    walk_body(compiler, &ir.body, &block_map, function)?;

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
    let then_bb = register_block(compiler, function, ir.then.id, "then");
    let else_bb = register_block(compiler, function, ir.else_arm.id, "else");
    let merge_bb = register_block(compiler, function, ir.merge_block, "ifcont");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.then.id, then_bb);
    block_map.insert(ir.else_arm.id, else_bb);
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
        &ir.then.instructions,
        function,
        None,
        &mut value_map,
    )?;
    let then_end_bb = compiler.builder.get_insert_block().unwrap();
    if !compiler.current_block_terminated() {
        emit_terminator(
            compiler,
            &ir.then.terminator,
            &block_map,
            &value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(else_bb);
    execute_instructions(
        compiler,
        &ir.else_arm.instructions,
        function,
        None,
        &mut value_map,
    )?;
    let else_end_bb = compiler.builder.get_insert_block().unwrap();
    if !compiler.current_block_terminated() {
        emit_terminator(
            compiler,
            &ir.else_arm.terminator,
            &block_map,
            &value_map,
            function,
        )?;
    }

    compiler.builder.position_at_end(merge_bb);
    block_map.insert(ir.then.id, then_end_bb);
    block_map.insert(ir.else_arm.id, else_end_bb);
    execute_instructions(
        compiler,
        &ir.merge_instructions,
        function,
        Some(&block_map),
        &mut value_map,
    )?;

    let Some(merge_value) = ir.merge_value else {
        return Ok(None);
    };
    let value = value_map
        .get(&merge_value)
        .copied()
        .ok_or("IRIfElse: pre-staged merge phi did not register an LLVM value")?;
    Ok(Some(TypedValue::new(value, ir.result_ty.clone())))
}

/// Walks an [`IRCond`] into LLVM IR. N-arm generalization of
/// [`emit_if_else`].
///
/// Allocates LLVM blocks for `arms[1..N].check_block` (skipping
/// `arms[0]`'s, which is the construct's implicit entry and runs at
/// the call-site builder position), every `arms[*].body_block`,
/// the optional `else_block`, and `merge_block`. For each arm:
/// position the builder at the arm's check, execute
/// `check_instructions`, dispatch the canonicalized
/// `check_terminator`; then position at the body, walk the AST
/// statements via [`compile_body_as_value`] to capture the
/// trailing-expression value (when present) and the actual end
/// block (which may differ from `body_block` when the body
/// contains nested control flow), and emit `body_terminator` if
/// the arm has not self-terminated.
///
/// Like [`emit_if_else`], the merge phi is synthesized inline
/// (rather than via [`execute_instructions`]) because the actual
/// end blocks are known only after walking the AST stubs and arms
/// may diverge or be statement-only. Unlike [`emit_if_else`], the
/// value-merge contract is *all-or-nothing* (matches legacy
/// `compile_cond` semantics): every arm + else (when present) must
/// produce a matching-LLVM-typed value, or the construct returns
/// `Ok(None)` (when no arms produced) or `Err` (when some-but-not-
/// all produced). Typecheck normally catches the partial-production
/// case at the source level, so the `Err` arm is defensive.
fn emit_cond<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRCond,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let merge_bb = register_block(compiler, function, ir.merge_block, "cond_end");

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(ir.merge_block, merge_bb);

    let body_bbs: Vec<BasicBlock<'ctx>> = ir
        .arms
        .iter()
        .enumerate()
        .map(|(i, arm)| {
            let bb = register_block(compiler, function, arm.body.id, &format!("cond_body_{i}"));
            block_map.insert(arm.body.id, bb);
            bb
        })
        .collect();

    for (i, arm) in ir.arms.iter().enumerate().skip(1) {
        let bb = register_block(
            compiler,
            function,
            arm.check_block,
            &format!("cond_check_{i}"),
        );
        block_map.insert(arm.check_block, bb);
    }

    let else_bb = ir.else_arm.as_ref().map(|else_block| {
        let bb = register_block(compiler, function, else_block.id, "cond_else");
        block_map.insert(else_block.id, bb);
        bb
    });

    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();

    for (i, arm) in ir.arms.iter().enumerate() {
        if i > 0 {
            let check_bb = block_map[&arm.check_block];
            compiler.builder.position_at_end(check_bb);
        }

        execute_instructions(
            compiler,
            &arm.check_instructions,
            function,
            None,
            &mut value_map,
        )?;
        emit_terminator(
            compiler,
            &arm.check_terminator,
            &block_map,
            &value_map,
            function,
        )?;

        compiler.builder.position_at_end(body_bbs[i]);
        execute_instructions(
            compiler,
            &arm.body.instructions,
            function,
            None,
            &mut value_map,
        )?;
        let arm_end_bb = compiler.builder.get_insert_block().unwrap();
        if !compiler.current_block_terminated() {
            emit_terminator(
                compiler,
                &arm.body.terminator,
                &block_map,
                &value_map,
                function,
            )?;
        }
        block_map.insert(arm.body.id, arm_end_bb);
    }

    if let (Some(else_bb), Some(else_block)) = (else_bb, ir.else_arm.as_ref()) {
        compiler.builder.position_at_end(else_bb);
        execute_instructions(
            compiler,
            &else_block.instructions,
            function,
            None,
            &mut value_map,
        )?;
        let else_end_bb = compiler.builder.get_insert_block().unwrap();
        if !compiler.current_block_terminated() {
            emit_terminator(
                compiler,
                &else_block.terminator,
                &block_map,
                &value_map,
                function,
            )?;
        }
        block_map.insert(else_block.id, else_end_bb);
    }

    compiler.builder.position_at_end(merge_bb);
    execute_instructions(
        compiler,
        &ir.merge_instructions,
        function,
        Some(&block_map),
        &mut value_map,
    )?;

    let Some(merge_value) = ir.merge_value else {
        return Ok(None);
    };
    let value = value_map
        .get(&merge_value)
        .copied()
        .ok_or("IRCond: pre-staged merge phi did not register an LLVM value")?;
    Ok(Some(TypedValue::new(value, ir.result_ty.clone())))
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
    let then_bb = register_block(compiler, function, ir.then_block, "tern_then");
    let else_bb = register_block(compiler, function, ir.else_block, "tern_else");
    let merge_bb = register_block(compiler, function, ir.merge_block, "tern_cont");

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
