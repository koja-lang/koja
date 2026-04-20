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
    c: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let loop_header = c.context.append_basic_block(function, "loop_header");
    let loop_body = c.context.append_basic_block(function, "loop_body");
    let loop_exit = c.context.append_basic_block(function, "loop_exit");

    c.builder.build_unconditional_branch(loop_header).unwrap();

    c.builder.position_at_end(loop_header);
    c.builder.build_unconditional_branch(loop_body).unwrap();

    c.builder.position_at_end(loop_body);
    c.fn_state.loop_exit_stack.push(loop_exit);

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(loop_header).unwrap();
    }

    c.fn_state.loop_exit_stack.pop();
    c.builder.position_at_end(loop_exit);

    Ok(None)
}

/// Compiles a `while` loop. Condition is re-evaluated each iteration.
pub fn compile_while<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let while_header = c.context.append_basic_block(function, "while_header");
    let while_body = c.context.append_basic_block(function, "while_body");
    let while_exit = c.context.append_basic_block(function, "while_exit");

    c.builder.build_unconditional_branch(while_header).unwrap();

    c.builder.position_at_end(while_header);
    let cond_val = compile_expr(c, condition, function)?
        .ok_or("while condition produced no value")?
        .value;
    let cond_int = coerce_to_bool(c, cond_val, "while condition")?;
    c.builder
        .build_conditional_branch(cond_int, while_body, while_exit)
        .unwrap();

    c.builder.position_at_end(while_body);
    c.fn_state.loop_exit_stack.push(while_exit);

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(while_header).unwrap();
    }

    c.fn_state.loop_exit_stack.pop();
    c.builder.position_at_end(while_exit);

    Ok(None)
}

/// Compiles a `for` loop by desugaring into an indexed while loop:
///   idx = 0; len = iterable.length(); while idx < len { elem = iterable.get(idx); body; idx += 1 }
pub fn compile_for<'ctx>(
    c: &mut Compiler<'ctx>,
    pattern: &Pattern,
    iterable: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let iter_tv = compile_expr(c, iterable, function)?.ok_or("for iterable produced no value")?;
    let iter_val = iter_tv.value;

    let iter_ty = iter_tv.expo_type;
    let iter_llvm_ty = iter_val.get_type();

    let iter_alloca = c.builder.build_alloca(iter_llvm_ty, "for_iter").unwrap();
    c.builder.build_store(iter_alloca, iter_val).unwrap();
    c.fn_state.variables.insert(
        "__for_iter".to_string(),
        (iter_alloca, iter_ty.clone(), Ownership::Unowned),
    );

    let resolved = resolve_enumerable_info(&c.lower_ctx(), &iter_ty)?;
    let elem_llvm_ty = to_llvm_type(&resolved.elem_type, c.context, &c.llvm_types)
        .ok_or("cannot resolve element LLVM type")?;

    monomorphize_impl_method(c, &resolved.base, "length", &resolved.type_args, &[])?;
    monomorphize_impl_method(c, &resolved.base, "get", &resolved.type_args, &[])?;

    let length_fn_name = format!("{}_length", resolved.mangled_type);
    let get_fn_name = format!("{}_get", resolved.mangled_type);

    let length_fn = *c
        .functions
        .get(&length_fn_name)
        .ok_or_else(|| format!("no function `{length_fn_name}`"))?;
    let get_fn = *c
        .functions
        .get(&get_fn_name)
        .ok_or_else(|| format!("no function `{get_fn_name}`"))?;

    let i64_ty = c.context.i64_type();

    let iter_loaded = c
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_load")
        .unwrap();
    let len_val = c
        .call(length_fn, &[iter_loaded.into()], "len")
        .ok_or("length() returned void")?
        .into_int_value();

    let idx_alloca = c.builder.build_alloca(i64_ty, "for_idx").unwrap();
    c.builder
        .build_store(idx_alloca, i64_ty.const_int(0, false))
        .unwrap();

    let header_bb = c.context.append_basic_block(function, "for_header");
    let body_bb = c.context.append_basic_block(function, "for_body");
    let exit_bb = c.context.append_basic_block(function, "for_exit");

    c.builder.build_unconditional_branch(header_bb).unwrap();

    c.builder.position_at_end(header_bb);
    let idx = c
        .builder
        .build_load(i64_ty, idx_alloca, "idx")
        .unwrap()
        .into_int_value();
    let cond = c
        .builder
        .build_int_compare(IntPredicate::ULT, idx, len_val, "for_cond")
        .unwrap();
    c.builder
        .build_conditional_branch(cond, body_bb, exit_bb)
        .unwrap();

    c.builder.position_at_end(body_bb);
    c.fn_state.loop_exit_stack.push(exit_bb);

    let iter_for_get = c
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_get")
        .unwrap();
    let idx_for_get = c.builder.build_load(i64_ty, idx_alloca, "idx_get").unwrap();
    let option_val = c
        .call(get_fn, &[iter_for_get.into(), idx_for_get.into()], "elem")
        .ok_or("get() returned void")?;
    let elem_val = c
        .builder
        .build_extract_value(option_val.into_struct_value(), 1, "payload")
        .unwrap();

    if let Pattern::Binding { name, .. } = pattern {
        let alloca = c.builder.build_alloca(elem_llvm_ty, name).unwrap();
        c.builder.build_store(alloca, elem_val).unwrap();
        c.fn_state.variables.insert(
            name.clone(),
            (alloca, resolved.elem_type.clone(), Ownership::Unowned),
        );
    }

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        let cur_idx = c
            .builder
            .build_load(i64_ty, idx_alloca, "cur_idx")
            .unwrap()
            .into_int_value();
        let next_idx = c
            .builder
            .build_int_add(cur_idx, i64_ty.const_int(1, false), "next_idx")
            .unwrap();
        c.builder.build_store(idx_alloca, next_idx).unwrap();
        c.builder.build_unconditional_branch(header_bb).unwrap();
    }

    c.fn_state.loop_exit_stack.pop();
    c.fn_state.variables.remove("__for_iter");
    c.builder.position_at_end(exit_bb);

    Ok(None)
}
