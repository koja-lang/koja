//! Loop compilation: infinite loops, while loops, and for loops (desugared
//! into indexed while loops over Enumeration types).

use crate::drop::Ownership;
use expo_ast::ast::{Expr, Pattern, Statement};
use expo_ir::lower::loops::resolve_enumerable_info;
use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::{Compiler, ExprResult};
use crate::expr::compile_expr;
use crate::generics::monomorphize_impl_method;
use crate::stmt::compile_statement;
use crate::types::to_llvm_type;

use super::coerce_to_bool;

/// Compiles an infinite `loop` block. Only exits via `break`.
pub fn compile_loop<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let loop_header = compiler.context.append_basic_block(function, "loop_header");
    let loop_body = compiler.context.append_basic_block(function, "loop_body");
    let loop_exit = compiler.context.append_basic_block(function, "loop_exit");

    compiler
        .builder
        .build_unconditional_branch(loop_header)
        .unwrap();

    compiler.builder.position_at_end(loop_header);
    compiler
        .builder
        .build_unconditional_branch(loop_body)
        .unwrap();

    compiler.builder.position_at_end(loop_body);
    compiler.fn_state.loop_exit_stack.push(loop_exit);

    for stmt in body {
        if compiler.current_block_terminated() {
            break;
        }
        compile_statement(compiler, stmt, function)?;
    }

    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(loop_header)
            .unwrap();
    }

    compiler.fn_state.loop_exit_stack.pop();
    compiler.builder.position_at_end(loop_exit);

    Ok(None)
}

/// Compiles a `while` loop. Condition is re-evaluated each iteration.
pub fn compile_while<'ctx>(
    compiler: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let while_header = compiler
        .context
        .append_basic_block(function, "while_header");
    let while_body = compiler.context.append_basic_block(function, "while_body");
    let while_exit = compiler.context.append_basic_block(function, "while_exit");

    compiler
        .builder
        .build_unconditional_branch(while_header)
        .unwrap();

    compiler.builder.position_at_end(while_header);
    let cond_val = compile_expr(compiler, condition, function)?
        .ok_or("while condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(compiler, cond_val, "while condition")?;
    compiler
        .builder
        .build_conditional_branch(cond_int, while_body, while_exit)
        .unwrap();

    compiler.builder.position_at_end(while_body);
    compiler.fn_state.loop_exit_stack.push(while_exit);

    for stmt in body {
        if compiler.current_block_terminated() {
            break;
        }
        compile_statement(compiler, stmt, function)?;
    }

    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(while_header)
            .unwrap();
    }

    compiler.fn_state.loop_exit_stack.pop();
    compiler.builder.position_at_end(while_exit);

    Ok(None)
}

/// Compiles a `for` loop by desugaring into an indexed while loop:
///   idx = 0; len = iterable.length(); while idx < len { elem = iterable.get(idx); body; idx += 1 }
pub fn compile_for<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    iterable: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let iter_tv =
        compile_expr(compiler, iterable, function)?.ok_or("for iterable produced no value")?;
    let iter_val = iter_tv.value;

    let iter_ty = iter_tv.expo_type;
    let iter_llvm_ty = iter_val.get_type();

    let iter_alloca = compiler
        .builder
        .build_alloca(iter_llvm_ty, "for_iter")
        .unwrap();
    compiler.builder.build_store(iter_alloca, iter_val).unwrap();
    compiler.fn_state.variables.insert(
        "__for_iter".to_string(),
        (iter_alloca, iter_ty.clone(), Ownership::Unowned),
    );

    let resolved = resolve_enumerable_info(&compiler.lower_ctx(), &iter_ty)?;
    let elem_llvm_ty = to_llvm_type(&resolved.elem_type, compiler.context, &compiler.llvm_types)
        .ok_or("cannot resolve element LLVM type")?;

    monomorphize_impl_method(compiler, &resolved.base, "length", &resolved.type_args, &[])?;
    monomorphize_impl_method(compiler, &resolved.base, "get", &resolved.type_args, &[])?;

    let length_fn_name = format!("{}_length", resolved.mangled_type);
    let get_fn_name = format!("{}_get", resolved.mangled_type);

    let length_fn = *compiler
        .functions
        .get(&length_fn_name)
        .ok_or_else(|| format!("no function `{length_fn_name}`"))?;
    let get_fn = *compiler
        .functions
        .get(&get_fn_name)
        .ok_or_else(|| format!("no function `{get_fn_name}`"))?;

    let i64_ty = compiler.context.i64_type();

    let iter_loaded = compiler
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_load")
        .unwrap();
    let len_val = compiler
        .call(length_fn, &[iter_loaded.into()], "len")
        .ok_or("length() returned void")?
        .into_int_value();

    let idx_alloca = compiler.builder.build_alloca(i64_ty, "for_idx").unwrap();
    compiler
        .builder
        .build_store(idx_alloca, i64_ty.const_int(0, false))
        .unwrap();

    let header_bb = compiler.context.append_basic_block(function, "for_header");
    let body_bb = compiler.context.append_basic_block(function, "for_body");
    let exit_bb = compiler.context.append_basic_block(function, "for_exit");

    compiler
        .builder
        .build_unconditional_branch(header_bb)
        .unwrap();

    compiler.builder.position_at_end(header_bb);
    let idx = compiler
        .builder
        .build_load(i64_ty, idx_alloca, "idx")
        .unwrap()
        .into_int_value();
    let cond = compiler
        .builder
        .build_int_compare(IntPredicate::ULT, idx, len_val, "for_cond")
        .unwrap();
    compiler
        .builder
        .build_conditional_branch(cond, body_bb, exit_bb)
        .unwrap();

    compiler.builder.position_at_end(body_bb);
    compiler.fn_state.loop_exit_stack.push(exit_bb);

    let iter_for_get = compiler
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_get")
        .unwrap();
    let idx_for_get = compiler
        .builder
        .build_load(i64_ty, idx_alloca, "idx_get")
        .unwrap();
    let option_val = compiler
        .call(get_fn, &[iter_for_get.into(), idx_for_get.into()], "elem")
        .ok_or("get() returned void")?;
    let elem_val = compiler
        .builder
        .build_extract_value(option_val.into_struct_value(), 1, "payload")
        .unwrap();

    if let Pattern::Binding { name, .. } = pattern {
        let alloca = compiler.builder.build_alloca(elem_llvm_ty, name).unwrap();
        compiler.builder.build_store(alloca, elem_val).unwrap();
        compiler.fn_state.variables.insert(
            name.clone(),
            (alloca, resolved.elem_type.clone(), Ownership::Unowned),
        );
    }

    for stmt in body {
        if compiler.current_block_terminated() {
            break;
        }
        compile_statement(compiler, stmt, function)?;
    }

    if !compiler.current_block_terminated() {
        let cur_idx = compiler
            .builder
            .build_load(i64_ty, idx_alloca, "cur_idx")
            .unwrap()
            .into_int_value();
        let next_idx = compiler
            .builder
            .build_int_add(cur_idx, i64_ty.const_int(1, false), "next_idx")
            .unwrap();
        compiler.builder.build_store(idx_alloca, next_idx).unwrap();
        compiler
            .builder
            .build_unconditional_branch(header_bb)
            .unwrap();
    }

    compiler.fn_state.loop_exit_stack.pop();
    compiler.fn_state.variables.remove("__for_iter");
    compiler.builder.position_at_end(exit_bb);

    Ok(None)
}
