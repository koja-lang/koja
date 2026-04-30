//! Loop compilation: `while`, infinite `loop`, and `for` (desugared at
//! emission into an indexed `while` loop over an `Enumeration` impl).
//!
//! Slice 3: `compile_while` and `compile_loop` collapse to thin shims
//! over [`expo_ir::Lowerer::lower_while`] /
//! [`expo_ir::Lowerer::lower_loop`] threaded through the recursive
//! [`expo_ir::CFGBuilder`] surface and walked via
//! [`super::walk_function_blocks`]. `compile_for` keeps the legacy
//! AST-driven iterator-protocol desugaring at the codegen seam --
//! the IR-side `lower_for` Stubs the entire `for` AST today, and the
//! desugar relocates into the elaboration pass when [`expo_ir::values::IRInstruction::ForLoopStub`]
//! lands.

use expo_ast::ast::{Expr, Pattern, Statement};
use expo_ir::identity::FunctionIdentifier;
use expo_ir::lower::loops::resolve_enumerable_info;
use expo_ir::resolved::loops::ResolvedEnumerable;
use expo_ir::{CFGBuilder, IRBlockId, IROperand};
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult};
use crate::drop::Ownership;
use crate::expr::compile_expr;
use crate::generics::monomorphize_impl_method;
use crate::types::to_llvm_type;

use super::register_block;
use super::walk_function_blocks;

/// Compiles an infinite `loop` block. Only exits via `break`.
pub fn compile_loop<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    lift_loop(compiler, function, |lowerer, builder, open| {
        lowerer.lower_loop(builder, open, body)
    })
}

/// Compiles a `while` loop.
pub fn compile_while<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    lift_loop(compiler, function, |lowerer, builder, open| {
        lowerer.lower_while(builder, open, condition, body)
    })
}

fn lift_loop<'ctx, F>(
    compiler: &mut Compiler<'ctx>,
    function: FunctionValue<'ctx>,
    lift: F,
) -> ExprResult<'ctx>
where
    F: FnOnce(
        &mut expo_ir::Lowerer<'_>,
        &mut CFGBuilder,
        IRBlockId,
    ) -> Result<(Option<IRBlockId>, IROperand), String>,
{
    let mut builder = CFGBuilder::new();
    let entry = compiler.fn_lower.next_block_id();
    builder.add_block(entry, "loop_entry");
    let (_open, _) = {
        let mut lowerer = compiler.lowerer();
        // The loop lowering pushed `loop_exit` for break resolution.
        // Walk runs Stub-deferred sub-expressions which may contain
        // breaks; pop after the walk.
        lift(&mut lowerer, &mut builder, entry)?
    };
    let (blocks, closed) = builder.into_blocks_with_closed();
    walk_function_blocks(compiler, &blocks, &closed, function, None)?;
    compiler.fn_lower.pop_loop_exit();
    Ok(None)
}

/// Compiles a `for` loop by desugaring (at emission time) into an
/// indexed `while`-style loop over an `Enumeration` impl. The IR-side
/// lowering Stubs the entire `for` AST; this codegen-side shim does
/// the actual iterator-protocol desugar (planned to relocate into
/// the elaboration pass once [`expo_ir::values::IRInstruction::ForLoopStub`]
/// is reintroduced in a follow-up slice).
pub fn compile_for<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    iterable: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let setup = build_for_loop_setup(compiler, iterable, function)?;

    let header_id = compiler.fn_lower.next_block_id();
    let body_id = compiler.fn_lower.next_block_id();
    let exit_id = compiler.fn_lower.next_block_id();

    let header_bb = register_block(compiler, function, header_id, "for_header");
    let body_bb = register_block(compiler, function, body_id, "for_body");
    let exit_bb = register_block(compiler, function, exit_id, "for_exit");

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
    compiler.fn_lower.push_loop_exit(exit_id);
    emit_for_iteration(compiler, pattern, body, &setup, header_bb, function)?;
    compiler.fn_lower.pop_loop_exit();
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
    pattern: &Pattern,
    body: &[Statement],
    setup: &ForLoopSetup<'ctx>,
    header_bb: BasicBlock<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let elem_val = build_for_element_load(compiler, setup);
    bind_for_pattern(compiler, pattern, elem_val, setup);

    // Lower the body via the IR pipeline, then walk the resulting
    // blocks at the current LLVM builder position. Body may contain
    // statements like `break` / `return`; the surrounding
    // `push_loop_exit` already published the exit id.
    let (body_blocks, body_closed, body_open) = lower_for_body(compiler, body)?;
    walk_function_blocks(compiler, &body_blocks, &body_closed, function, None)?;
    let _ = body_open;

    if !compiler.current_block_terminated() {
        emit_for_back_edge(compiler, setup, header_bb);
    }
    Ok(())
}

/// Lowered `for`-body fragment: the IR blocks, the set of blocks
/// whose terminator was explicitly set by lowering, and the final
/// open block id (or `None` if all paths terminated).
type LoweredForBody = (
    Vec<expo_ir::IRBasicBlock>,
    std::collections::HashSet<IRBlockId>,
    Option<IRBlockId>,
);

/// Lower a `for` loop body into IR blocks via a fresh
/// [`CFGBuilder`].
fn lower_for_body<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
) -> Result<LoweredForBody, String> {
    let mut builder = CFGBuilder::new();
    let entry = compiler.fn_lower.next_block_id();
    builder.add_block(entry, "for_body_entry");
    let exit = {
        let mut lowerer = compiler.lowerer();
        lowerer.lower_statements(&mut builder, entry, body)?
    };
    let (blocks, closed) = builder.into_blocks_with_closed();
    Ok((blocks, closed, exit))
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
    iterable: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<ForLoopSetup<'ctx>, String> {
    let iter_tv =
        compile_expr(compiler, iterable, function)?.ok_or("for iterable produced no value")?;
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

#[allow(dead_code)] // Reserved for the elaboration-pass relocation.
fn _unused_for_compat<'ctx>(_: &mut Compiler<'ctx>) -> ExprResult<'ctx> {
    Ok(None)
}
