//! Conditional compilation: if/else, unless, cond, and ternary expressions.
//!
//! Slice 3 collapsed every shim into a five-step recursive lowering:
//! create a [`expo_ir::CFGBuilder`], mint an entry block, call the
//! corresponding `Lowerer::lower_*` method (which builds the
//! per-construct CFG into the builder), then walk the resulting
//! blocks via [`super::walk_function_blocks`]. The merge block's
//! placeholder self-branch terminator is left unset by lowering --
//! [`super::walk_function_blocks`] detects it and leaves the LLVM
//! builder positioned at the merge block so the surrounding
//! `compile_expr` continuation continues writing into it.

use expo_ast::ast::{CondArm, Expr, Statement};
use expo_ir::{CFGBuilder, IROperand};
use expo_typecheck::types::Type;
use inkwell::values::FunctionValue;

use crate::compiler::{Compiler, ExprResult, TypedValue};

use super::walk_function_blocks;

/// AST-level emitter for `cond ... end`. The else-bearing case and
/// the non-empty no-else case both lift through the IR arm in
/// [`expo_ir::Lowerer::lower_expr_to_operand`]; this shim is kept
/// alive by Stub-nested parents (closures, list literals, match
/// operands) plus the empty-and-no-else degenerate case (a no-op
/// matching legacy semantics).
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
    let result_ty = resolved_type.cloned().unwrap_or(Type::Unknown);
    lift_construct(
        compiler,
        function,
        result_ty.clone(),
        |lowerer, builder, open| {
            lowerer.lower_cond(builder, open, arms, else_body.as_deref(), result_ty)
        },
    )
}

/// AST-level emitter for `if cond ... [else ...] end`. Both arms
/// (with and without `else`) now lift through the IR arm in
/// [`expo_ir::Lowerer::lower_expr_to_operand`]; this shim is kept
/// alive by Stub-nested parents (closures, list literals, match
/// operands).
pub fn compile_if<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[Statement],
    else_body: &Option<Vec<Statement>>,
    resolved_type: Option<&Type>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let result_ty = resolved_type.cloned().unwrap_or(Type::Unknown);
    match else_body {
        None => lift_construct(compiler, function, Type::Unit, |lowerer, builder, open| {
            lowerer.lower_if_no_else(builder, open, condition, then_body)
        }),
        Some(else_stmts) => lift_construct(
            compiler,
            function,
            result_ty.clone(),
            |lowerer, builder, open| {
                lowerer.lower_if_else(builder, open, condition, then_body, else_stmts, result_ty)
            },
        ),
    }
}

/// AST-level emitter for `unless cond ... end`. Lifted through the
/// IR arm in [`expo_ir::Lowerer::lower_expr_to_operand`]; this shim
/// is kept alive by Stub-nested parents (closures, list literals,
/// match operands).
pub fn compile_unless<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    lift_construct(compiler, function, Type::Unit, |lowerer, builder, open| {
        lowerer.lower_unless(builder, open, condition, body)
    })
}

/// Compiles a ternary expression (`condition ? then_expr : else_expr`).
pub fn compile_ternary<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    resolved_type: &Type,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let result_ty = resolved_type.clone();
    lift_construct(
        compiler,
        function,
        result_ty.clone(),
        |lowerer, builder, open| {
            lowerer.lower_ternary(builder, open, condition, then_expr, else_expr, result_ty)
        },
    )
}

/// Drive a control-flow construct's recursive lowering at the
/// current LLVM builder position: create a fresh
/// [`expo_ir::CFGBuilder`], add the entry block, run the lift
/// closure, walk the produced blocks via
/// [`super::walk_function_blocks`], and materialize the merge
/// operand (when present) into a [`TypedValue`].
fn lift_construct<'ctx, F>(
    compiler: &mut Compiler<'ctx>,
    function: FunctionValue<'ctx>,
    result_ty: Type,
    lift: F,
) -> ExprResult<'ctx>
where
    F: FnOnce(
        &mut expo_ir::Lowerer<'_>,
        &mut CFGBuilder,
        expo_ir::IRBlockId,
    ) -> Result<(Option<expo_ir::IRBlockId>, IROperand), String>,
{
    let mut builder = CFGBuilder::new();
    let entry = compiler.fn_lower.next_block_id();
    builder.add_block(entry, "construct_entry");
    let (_open, operand) = {
        let mut lowerer = compiler.lowerer();
        lift(&mut lowerer, &mut builder, entry)?
    };
    let (blocks, closed) = builder.into_blocks_with_closed();
    let result = match &operand {
        IROperand::Unit => walk_function_blocks(compiler, &blocks, &closed, function, None)?
            .map(|_| unreachable!("Unit operand returned a value")),
        op => walk_function_blocks(compiler, &blocks, &closed, function, Some(op))?,
    };
    Ok(result.map(|v| TypedValue::new(v, result_ty)))
}
