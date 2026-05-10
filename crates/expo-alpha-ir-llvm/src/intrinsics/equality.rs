//! `Equality.eq` family — one impl per primitive integer / bool
//! type from `kernel.expo` (`Bool`, `Int`, `Int8`, `Int16`, `Int32`,
//! `UInt8`, `UInt16`, `UInt32`, `UInt64`). Every cell collapses to
//! the same LLVM shape: `icmp eq` on two same-width integers.
//! Float and string equality belong to other families that do not
//! share this dispatch shape (no `Equality for Float`/`String` impls
//! today).

use expo_alpha_ir::IRFunction;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

const TYPES: &[&str] = &[
    "Bool", "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
];

pub(super) fn matches_id(id: &str) -> bool {
    TYPES.iter().any(|ty| id == format!("{ty}.eq"))
}

pub(super) fn emit_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    _id: &str,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let lhs = nth_int(function, llvm_function, 0, "self")?;
    let rhs = nth_int(function, llvm_function, 1, "other")?;
    let cmp = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, lhs, rhs, "eq")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_return(Some(&cmp))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn nth_int<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "missing param `{name}` (#{index}) on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::IntValue(v) => Ok(v),
        other => Err(LlvmError::Codegen(format!(
            "expected integer for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}
