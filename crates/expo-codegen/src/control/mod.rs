//! Control flow compilation: if/else, cond, match, ternary, while loops,
//! for loops, and infinite loops with break support.

mod conditionals;
mod instructions;
mod loops;
mod patterns;
mod terminator;

use expo_ast::ast::Statement;

pub use conditionals::{compile_cond, compile_if, compile_ternary, compile_unless};
pub(crate) use instructions::{execute_instructions, maybe_typed_value};
pub use loops::{compile_for, compile_loop, compile_while};
pub use patterns::compile_match;
pub(crate) use patterns::{compile_pattern, get_payload_ptr, match_values};
pub(crate) use terminator::emit_terminator;

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::compiler::{Compiler, TypedValue};
use crate::expr::compile_expr;
use crate::stmt::compile_statement;

/// Compiles a statement list and returns the value of the last expression.
/// Non-expression statements produce no value; only a trailing `Expr` is captured.
pub(crate) fn compile_body_as_value<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<TypedValue<'ctx>>, String> {
    let mut val: Option<TypedValue> = None;
    for (i, stmt) in body.iter().enumerate() {
        if compiler.current_block_terminated() {
            break;
        }
        if i == body.len() - 1
            && let Statement::Expr(expr) = stmt
        {
            val = compile_expr(compiler, expr, function)?;
            continue;
        }
        let was_tail = compiler.fn_lower.save_tail();
        compile_statement(compiler, stmt, function)?;
        compiler.fn_lower.restore_tail(was_tail);
    }
    Ok(val)
}

/// Converts an integer value to a 1-bit bool. Already-boolean values pass
/// through; wider ints are compared != 0.
pub(super) fn coerce_to_bool<'ctx>(
    compiler: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<IntValue<'ctx>, String> {
    if !val.is_int_value() {
        return Err(format!("{label} must be a boolean"));
    }

    let iv = val.into_int_value();
    if iv.get_type().get_bit_width() == 1 {
        Ok(iv)
    } else {
        Ok(compiler
            .builder
            .build_int_compare(IntPredicate::NE, iv, iv.get_type().const_zero(), label)
            .unwrap())
    }
}
