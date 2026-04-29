//! Loop compilation: `while`, infinite `loop`, and `for` (desugared at
//! emission into an indexed `while` loop over an `Enumeration` impl).
//!
//! Slice 6 lifted all three constructs onto the
//! [`expo_ir::Lowerer`] + `emit_*_unified` pipeline that the
//! conditional walkers and `match` walker already use:
//!
//! - `compile_while` / `compile_loop` / `compile_for` are thin shims
//!   that lower the AST construct into an
//!   [`expo_ir::resolved::loops::IRWhile`] /
//!   [`expo_ir::resolved::loops::IRLoop`] /
//!   [`expo_ir::resolved::loops::IRFor`] and dispatch through the
//!   shared [`super::execute_instructions`] +
//!   [`super::emit_terminator`] machinery.
//! - Bodies remain AST `Vec<Statement>` stubs walked via
//!   [`super::compile_body_as_value`] (statement-level lowering is
//!   Phase 4g territory).
//! - `break` continues to use the codegen-side `loop_exit_stack`
//!   (push/pop bracketing the body walk in each emit walker) until
//!   Phase 4g lifts it into the IR.
//!
//! `for` keeps the iterator-protocol desugaring at the codegen seam
//! (`length()` / `get()` / `Option` unwrap / pattern bind) because the
//! desugaring needs the LLVM type registry; the lowerer only mints
//! the structural block / value ids. Same precedent as
//! [`expo_ir::values::IRInstruction::PatternBinaryMatch`] from Slice
//! 5b.

use std::collections::HashMap;

use expo_ast::ast::{Expr, Pattern, Statement};
use expo_ir::IRBlockId;
use expo_ir::identity::FunctionIdentifier;
use expo_ir::lower::loops::resolve_enumerable_info;
use expo_ir::resolved::loops::{IRFor, IRLoop, IRWhile, ResolvedEnumerable};
use expo_ir::values::IRValueId;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult};
use crate::drop::Ownership;
use crate::expr::compile_expr;
use crate::generics::monomorphize_impl_method;
use crate::stmt::compile_statement;
use crate::types::to_llvm_type;

use super::instructions::execute_instructions;
use super::terminator::emit_terminator;

/// Compiles an infinite `loop` block. Only exits via `break`. Lowers
/// to an [`IRLoop`] via [`expo_ir::Lowerer::lower_loop`] and walks
/// via [`emit_loop_unified`].
pub fn compile_loop<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let ir = compiler.lowerer().lower_loop(body);
    emit_loop_unified(compiler, &ir, function)
}

/// Compiles a `while` loop. Condition is re-evaluated each iteration.
/// Lowers to an [`IRWhile`] via [`expo_ir::Lowerer::lower_while`] and
/// walks via [`emit_while_unified`].
pub fn compile_while<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let ir = compiler.lowerer().lower_while(condition, body);
    emit_while_unified(compiler, &ir, function)
}

/// Compiles a `for` loop by desugaring (at emit time) into an indexed
/// `while`-style loop over an `Enumeration` impl. Lowers to an
/// [`IRFor`] via [`expo_ir::Lowerer::lower_for`] and walks via
/// [`emit_for_unified`].
pub fn compile_for<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    iterable: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let ir = compiler.lowerer().lower_for(iterable, pattern, body);
    emit_for_unified(compiler, &ir, function)
}

/// Walks an [`IRLoop`] into LLVM IR. Allocates LLVM blocks for
/// `body_block` / `exit_block`, branches into the body, walks the
/// AST-stub body via [`compile_statement`], and dispatches the
/// declared back-edge `body_terminator` only when the body has not
/// already self-terminated. `exit_block` is pushed onto
/// `loop_exit_stack` for AST `break` resolution.
fn emit_loop_unified<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRLoop,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let block_map = build_loop_block_map(compiler, ir, function);
    let body_bb = block_map[&ir.body_block];
    let exit_bb = block_map[&ir.exit_block];

    compiler
        .builder
        .build_unconditional_branch(body_bb)
        .unwrap();

    compiler.builder.position_at_end(body_bb);
    compiler.fn_state.loop_exit_stack.push(exit_bb);
    walk_loop_body(compiler, &ir.body_stmts, function)?;
    if !compiler.current_block_terminated() {
        let value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
        emit_terminator(
            compiler,
            &ir.body_terminator,
            &block_map,
            &value_map,
            function,
        )?;
    }
    compiler.fn_state.loop_exit_stack.pop();

    compiler.builder.position_at_end(exit_bb);
    Ok(None)
}

/// Walks an [`IRWhile`] into LLVM IR. Allocates LLVM blocks for
/// `header_block` / `body_block` / `exit_block`, branches into the
/// header, runs `header_instructions` through
/// [`execute_instructions`] and dispatches `header_terminator`
/// (`CondBranch { cond, then: body, otherwise: exit }`) via
/// [`emit_terminator`]. The body walks as in [`emit_loop_unified`]
/// with `exit_block` pushed onto `loop_exit_stack` for AST `break`.
fn emit_while_unified<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRWhile,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let block_map = build_while_block_map(compiler, ir, function);
    let header_bb = block_map[&ir.header_block];
    let body_bb = block_map[&ir.body_block];
    let exit_bb = block_map[&ir.exit_block];

    compiler
        .builder
        .build_unconditional_branch(header_bb)
        .unwrap();

    compiler.builder.position_at_end(header_bb);
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    execute_instructions(
        compiler,
        &ir.header_instructions,
        function,
        None,
        &mut value_map,
    )?;
    emit_terminator(
        compiler,
        &ir.header_terminator,
        &block_map,
        &value_map,
        function,
    )?;

    compiler.builder.position_at_end(body_bb);
    compiler.fn_state.loop_exit_stack.push(exit_bb);
    walk_loop_body(compiler, &ir.body_stmts, function)?;
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
    compiler.fn_state.loop_exit_stack.pop();

    compiler.builder.position_at_end(exit_bb);
    Ok(None)
}

/// Walks an [`IRFor`] into LLVM IR. Desugars `for binding in iterable`
/// into an indexed loop over an `Enumeration` impl: compiles the
/// iterable into a stack alloca, monomorphizes `length` / `get` and
/// looks up their LLVM symbols, allocates the index slot, then runs
/// the standard header (`idx < len` cond branch) / body
/// (`elem = get(idx); bind; body_stmts; idx += 1`) / exit skeleton
/// through the same block-map + value-map plumbing the other loop
/// walkers use.
fn emit_for_unified<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRFor,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let setup = build_for_loop_setup(compiler, ir, function)?;
    let block_map = build_for_block_map(compiler, ir, function);
    let header_bb = block_map[&ir.header_block];
    let body_bb = block_map[&ir.body_block];
    let exit_bb = block_map[&ir.exit_block];

    compiler
        .builder
        .build_unconditional_branch(header_bb)
        .unwrap();

    compiler.builder.position_at_end(header_bb);
    let cond = build_for_header_check(compiler, &setup);
    compiler
        .builder
        .build_conditional_branch(cond, body_bb, exit_bb)
        .unwrap();

    compiler.builder.position_at_end(body_bb);
    compiler.fn_state.loop_exit_stack.push(exit_bb);
    emit_for_iteration(compiler, ir, &setup, header_bb, function)?;
    compiler.fn_state.loop_exit_stack.pop();
    compiler.fn_state.variables.remove("__for_iter");

    compiler.builder.position_at_end(exit_bb);
    Ok(None)
}

/// Per-iteration scaffolding for a `for` loop: load the element via
/// `get(idx)`, bind it via the `binding_pattern`, walk the body, and
/// (when the body has not self-terminated) increment the index and
/// branch back to `header_bb`.
fn emit_for_iteration<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRFor,
    setup: &ForLoopSetup<'ctx>,
    header_bb: BasicBlock<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let elem_val = build_for_element_load(compiler, setup);
    bind_for_pattern(compiler, &ir.binding_pattern, elem_val, setup);

    walk_loop_body(compiler, &ir.body_stmts, function)?;

    if !compiler.current_block_terminated() {
        emit_for_back_edge(compiler, setup, header_bb);
    }
    Ok(())
}

/// Allocate LLVM basic blocks for an [`IRLoop`] and return the
/// id->block map.
fn build_loop_block_map<'ctx>(
    compiler: &Compiler<'ctx>,
    ir: &IRLoop,
    function: FunctionValue<'ctx>,
) -> HashMap<IRBlockId, BasicBlock<'ctx>> {
    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(
        ir.body_block,
        compiler.context.append_basic_block(function, "loop_body"),
    );
    block_map.insert(
        ir.exit_block,
        compiler.context.append_basic_block(function, "loop_exit"),
    );
    block_map
}

/// Allocate LLVM basic blocks for an [`IRWhile`] and return the
/// id->block map.
fn build_while_block_map<'ctx>(
    compiler: &Compiler<'ctx>,
    ir: &IRWhile,
    function: FunctionValue<'ctx>,
) -> HashMap<IRBlockId, BasicBlock<'ctx>> {
    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(
        ir.header_block,
        compiler
            .context
            .append_basic_block(function, "while_header"),
    );
    block_map.insert(
        ir.body_block,
        compiler.context.append_basic_block(function, "while_body"),
    );
    block_map.insert(
        ir.exit_block,
        compiler.context.append_basic_block(function, "while_exit"),
    );
    block_map
}

/// Allocate LLVM basic blocks for an [`IRFor`] and return the
/// id->block map.
fn build_for_block_map<'ctx>(
    compiler: &Compiler<'ctx>,
    ir: &IRFor,
    function: FunctionValue<'ctx>,
) -> HashMap<IRBlockId, BasicBlock<'ctx>> {
    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    block_map.insert(
        ir.header_block,
        compiler.context.append_basic_block(function, "for_header"),
    );
    block_map.insert(
        ir.body_block,
        compiler.context.append_basic_block(function, "for_body"),
    );
    block_map.insert(
        ir.exit_block,
        compiler.context.append_basic_block(function, "for_exit"),
    );
    block_map
}

/// Walk a loop body's AST statements, stopping early if the current
/// block self-terminates. Mirrors the per-statement walk that lived
/// in the legacy `compile_loop` / `compile_while` / `compile_for`
/// bodies; pulled out because all three walkers share it.
fn walk_loop_body<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    for stmt in body {
        if compiler.current_block_terminated() {
            break;
        }
        compile_statement(compiler, stmt, function)?;
    }
    Ok(())
}

/// Pre-loop scaffolding for a `for` loop: compiles the iterable into
/// a stack alloca, registers a hidden `__for_iter` binding so drop
/// analysis tracks it, monomorphizes the impl's `length` / `get`
/// methods and looks up their LLVM symbols, computes the LLVM type of
/// one element, allocates the index slot, and snapshots the iterable
/// length (`length()` is called once before the header). Returns
/// everything the per-iteration emission needs.
struct ForLoopSetup<'ctx> {
    elem_llvm_ty: BasicTypeEnum<'ctx>,
    elem_type: expo_typecheck::types::Type,
    get_fn: FunctionValue<'ctx>,
    idx_alloca: PointerValue<'ctx>,
    iter_alloca: PointerValue<'ctx>,
    iter_llvm_ty: BasicTypeEnum<'ctx>,
    len_val: IntValue<'ctx>,
}

fn build_for_loop_setup<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRFor,
    function: FunctionValue<'ctx>,
) -> Result<ForLoopSetup<'ctx>, String> {
    let iter_tv =
        compile_expr(compiler, &ir.iterable, function)?.ok_or("for iterable produced no value")?;
    let iter_ty = iter_tv.expo_type;
    let iter_llvm_ty = iter_tv.value.get_type();

    let iter_alloca = compiler
        .builder
        .build_alloca(iter_llvm_ty, "for_iter")
        .unwrap();
    compiler
        .builder
        .build_store(iter_alloca, iter_tv.value)
        .unwrap();
    compiler.fn_state.variables.insert(
        "__for_iter".to_string(),
        (iter_alloca, iter_ty.clone(), Ownership::Unowned),
    );

    let resolved = resolve_enumerable_info(&compiler.lower_ctx(), &iter_ty)?;
    let elem_llvm_ty = to_llvm_type(&resolved.elem_type, compiler.context, &compiler.llvm_types)
        .ok_or("cannot resolve element LLVM type")?;
    let (length_fn, get_fn) = resolve_for_impl_methods(compiler, &resolved)?;

    let iter_loaded = compiler
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_load")
        .unwrap();
    let len_val = compiler
        .call(length_fn, &[iter_loaded.into()], "len")
        .ok_or("length() returned void")?
        .into_int_value();

    let i64_ty = compiler.context.i64_type();
    let idx_alloca = compiler.builder.build_alloca(i64_ty, "for_idx").unwrap();
    compiler
        .builder
        .build_store(idx_alloca, i64_ty.const_int(0, false))
        .unwrap();

    Ok(ForLoopSetup {
        elem_llvm_ty,
        elem_type: resolved.elem_type,
        get_fn,
        idx_alloca,
        iter_alloca,
        iter_llvm_ty,
        len_val,
    })
}

/// Monomorphize the iterable's `length` / `get` impl methods (the
/// emitter's standard route to materializing an `Enumeration` impl
/// for a concrete iterable type) and look up their LLVM symbols.
fn resolve_for_impl_methods<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedEnumerable,
) -> Result<(FunctionValue<'ctx>, FunctionValue<'ctx>), String> {
    monomorphize_impl_method(compiler, &resolved.base, "length", &resolved.type_args, &[])?;
    monomorphize_impl_method(compiler, &resolved.base, "get", &resolved.type_args, &[])?;

    let length_fn_name = format!("{}_length", resolved.mangled_type);
    let get_fn_name = format!("{}_get", resolved.mangled_type);
    let length_fn = *compiler
        .functions
        .get(&FunctionIdentifier::new(&length_fn_name))
        .ok_or_else(|| format!("no function `{length_fn_name}`"))?;
    let get_fn = *compiler
        .functions
        .get(&FunctionIdentifier::new(&get_fn_name))
        .ok_or_else(|| format!("no function `{get_fn_name}`"))?;
    Ok((length_fn, get_fn))
}

/// Build the header's `idx < len` comparison. Materializes the
/// current index, compares it against the snapshotted length, and
/// returns the resulting i1 for the surrounding cond branch.
fn build_for_header_check<'ctx>(
    compiler: &mut Compiler<'ctx>,
    setup: &ForLoopSetup<'ctx>,
) -> IntValue<'ctx> {
    let i64_ty = compiler.context.i64_type();
    let idx = compiler
        .builder
        .build_load(i64_ty, setup.idx_alloca, "idx")
        .unwrap()
        .into_int_value();
    compiler
        .builder
        .build_int_compare(IntPredicate::ULT, idx, setup.len_val, "for_cond")
        .unwrap()
}

/// Per-iteration element load: reload the iterable, reload the
/// index, call `get(iterable, idx)`, and extract the `Some` payload
/// (struct field 1 of `Option`).
fn build_for_element_load<'ctx>(
    compiler: &mut Compiler<'ctx>,
    setup: &ForLoopSetup<'ctx>,
) -> BasicValueEnum<'ctx> {
    let i64_ty = compiler.context.i64_type();
    let iter_for_get = compiler
        .builder
        .build_load(setup.iter_llvm_ty, setup.iter_alloca, "iter_get")
        .unwrap();
    let idx_for_get = compiler
        .builder
        .build_load(i64_ty, setup.idx_alloca, "idx_get")
        .unwrap();
    let option_val = compiler
        .call(
            setup.get_fn,
            &[iter_for_get.into(), idx_for_get.into()],
            "elem",
        )
        .expect("get() returned void");
    compiler
        .builder
        .build_extract_value(option_val.into_struct_value(), 1, "payload")
        .unwrap()
}

/// Bind one iteration's element to the `for` loop's binding pattern.
/// Today only [`Pattern::Binding`] is supported (matches the legacy
/// `compile_for` behavior); other patterns silently no-op (typecheck
/// rejects most cases).
fn bind_for_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    elem_val: BasicValueEnum<'ctx>,
    setup: &ForLoopSetup<'ctx>,
) {
    if let Pattern::Binding { name, .. } = pattern {
        let alloca = compiler
            .builder
            .build_alloca(setup.elem_llvm_ty, name)
            .unwrap();
        compiler.builder.build_store(alloca, elem_val).unwrap();
        compiler.fn_state.variables.insert(
            name.clone(),
            (alloca, setup.elem_type.clone(), Ownership::Unowned),
        );
    }
}

/// Per-iteration back edge: increment the index and branch back to
/// `header_bb`. Emitted only when the body has not already
/// self-terminated.
fn emit_for_back_edge<'ctx>(
    compiler: &mut Compiler<'ctx>,
    setup: &ForLoopSetup<'ctx>,
    header_bb: BasicBlock<'ctx>,
) {
    let i64_ty = compiler.context.i64_type();
    let cur_idx = compiler
        .builder
        .build_load(i64_ty, setup.idx_alloca, "cur_idx")
        .unwrap()
        .into_int_value();
    let next_idx = compiler
        .builder
        .build_int_add(cur_idx, i64_ty.const_int(1, false), "next_idx")
        .unwrap();
    compiler
        .builder
        .build_store(setup.idx_alloca, next_idx)
        .unwrap();
    compiler
        .builder
        .build_unconditional_branch(header_bb)
        .unwrap();
}
