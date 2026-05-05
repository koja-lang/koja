//! Synthesize the host `i64 main()` entry point that wraps the
//! body's value in a runtime-printer call before returning 0.
//!
//! **Everything in this file is temporary scaffolding.** When
//! `IO.puts` lands, the auto-print wrapper goes away and the entry
//! function emits as a normal helper through
//! [`crate::function::define_function`]. The
//! [`__expo_app_name`](APP_NAME_SYMBOL) global also lives here
//! because it's the same kind of "runtime convention" plumbing —
//! emitted on every alpha-compiled binary so the runtime archive's
//! panic handler links cleanly regardless of cgu partitioning.
//!
//! See [`expo-runtime/src/alpha.rs`](../../expo-runtime/src/alpha.rs)
//! for the runtime side of these conventions.

use expo_alpha_ir::{IRBasicBlock, IRBlockId, IRTerminator, IRType};
use inkwell::AddressSpace;
use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::ctx::EmitCtx;
use crate::emit::{self, ValueMap};
use crate::error::LlvmError;
use crate::function::declare_blocks;

const APP_NAME_SYMBOL: &str = "__expo_app_name";
const ENTRY_SYMBOL: &str = "main";
const PRINT_BOOL_SYMBOL: &str = "__expo_alpha_print_bool";
const PRINT_F32_SYMBOL: &str = "__expo_alpha_print_f32";
const PRINT_F64_SYMBOL: &str = "__expo_alpha_print_f64";
const PRINT_INT_SYMBOL: &str = "__expo_alpha_print_i64";
const PRINT_STRING_SYMBOL: &str = "__expo_alpha_print_string";

/// Emit `__expo_app_name` as a null-terminated C-string constant.
/// The `expo-runtime` panic handler reads it for backtrace labels
/// (declared there as `extern static [c_char; 0]`); every
/// alpha-compiled binary defines it so the runtime archive links
/// cleanly regardless of codegen-unit partitioning.
pub(crate) fn emit_app_name_global(ctx: &EmitCtx<'_>, app_name: &str) {
    let value = ctx.context.const_string(app_name.as_bytes(), true);
    let global = ctx
        .module
        .add_global(value.get_type(), None, APP_NAME_SYMBOL);
    global.set_initializer(&value);
    global.set_constant(true);
}

/// Emit `blocks` as the host `main` function: declare `i64 main()`,
/// pre-create one inkwell `BasicBlock` per IR block, walk every IR
/// block's instructions in order, and intercept the
/// trailing-block's `Return` so we can insert the auto-print call
/// before `ret i64 0`. Branch / cond-branch terminators are lowered
/// to `br` instructions verbatim.
///
/// Empty bodies are illegal (sealed IR guarantees at least one
/// block), and the final IR block must end in `Return`. The seal
/// pass admits other terminators for non-trailing blocks; only the
/// entry function's last block carries the auto-print scaffolding.
pub(crate) fn emit_as_main<'ctx>(
    ctx: &EmitCtx<'ctx>,
    blocks: &[IRBasicBlock],
    return_type: &IRType,
) -> Result<(), LlvmError> {
    let i64_type = ctx.context.i64_type();
    let signature = i64_type.fn_type(&[], false);
    let function = ctx
        .module
        .add_function(ENTRY_SYMBOL, signature, Some(Linkage::External));
    let block_map = declare_blocks(ctx, function, blocks);
    let return_block_id = find_return_block(blocks)?;

    let mut values: ValueMap<'ctx> = ValueMap::new();
    for block in blocks {
        let llvm_block = block_map[&block.id];
        ctx.builder.position_at_end(llvm_block);
        if block.id == return_block_id {
            let (next_values, terminator) =
                emit::emit_instructions(ctx, block, std::mem::take(&mut values))?;
            values = next_values;
            let body_value = match terminator {
                IRTerminator::Return { value: Some(id) } => emit::lookup(&values, *id)?,
                IRTerminator::Return { value: None } => {
                    return Err(LlvmError::Codegen(
                        "alpha LLVM does not yet emit Unit-returning `main`".to_string(),
                    ));
                }
                other => {
                    unreachable!("main return-block must terminate in Return; got {other:?}")
                }
            };
            emit_print_call(ctx, return_type, body_value)?;
            ctx.builder
                .build_return(Some(&i64_type.const_int(0, false)))
                .map(|_| ())
                .map_err(|e| {
                    LlvmError::Codegen(format!("inkwell rejected build_return for main: {e}"))
                })?;
        } else {
            emit::emit_block(ctx, block, &block_map, &mut values)?;
        }
    }
    Ok(())
}

/// The [`IRBlockId`] of the unique block ending in `Return`. The
/// auto-print wrapper around `main` patches in `ret i64 0` after
/// executing the body, so we need to know which IR block carries
/// the body's value before walking. Today's slice produces exactly
/// one `Return`-terminated block per function (the merge block of
/// an `if` / `unless` falls through to it via `Branch`), so a
/// missing or duplicate `Return` is a lowering bug we surface as a
/// codegen error.
fn find_return_block(blocks: &[IRBasicBlock]) -> Result<IRBlockId, LlvmError> {
    let mut found: Option<IRBlockId> = None;
    for block in blocks {
        if matches!(block.terminator, IRTerminator::Return { .. }) {
            if found.is_some() {
                return Err(LlvmError::Codegen(
                    "alpha LLVM expects exactly one Return-terminated block in `main`".to_string(),
                ));
            }
            found = Some(block.id);
        }
    }
    found.ok_or_else(|| {
        LlvmError::Codegen(
            "alpha LLVM expects at least one Return-terminated block in `main`".to_string(),
        )
    })
}

/// Pick the runtime printer for `return_type` and emit the call.
/// Integer / `Bool` widths extend to `i64` (sign- or zero-extended
/// per signedness); `Float32` / `Float64` flow as native f32 / f64;
/// `String` flows the payload pointer through unchanged (runtime
/// reads the v1 header at `ptr - 8` for byte length).
fn emit_print_call<'ctx>(
    ctx: &EmitCtx<'ctx>,
    return_type: &IRType,
    body_value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let (printer_symbol, argument_type, argument) = match return_type {
        IRType::Bool => {
            let int = body_value.into_int_value();
            let extended = zext_to_i64(ctx, int)?;
            (
                PRINT_BOOL_SYMBOL,
                ctx.context.i64_type().into(),
                extended.into(),
            )
        }
        IRType::Float32 => (
            PRINT_F32_SYMBOL,
            ctx.context.f32_type().into(),
            body_value.into_float_value().into(),
        ),
        IRType::Float64 => (
            PRINT_F64_SYMBOL,
            ctx.context.f64_type().into(),
            body_value.into_float_value().into(),
        ),
        IRType::Int8 | IRType::Int16 | IRType::Int32 => {
            let int = body_value.into_int_value();
            let extended = sext_to_i64(ctx, int)?;
            (
                PRINT_INT_SYMBOL,
                ctx.context.i64_type().into(),
                extended.into(),
            )
        }
        IRType::Int64 | IRType::UInt64 => (
            PRINT_INT_SYMBOL,
            ctx.context.i64_type().into(),
            body_value.into_int_value().into(),
        ),
        IRType::UInt8 | IRType::UInt16 | IRType::UInt32 => {
            let int = body_value.into_int_value();
            let extended = zext_to_i64(ctx, int)?;
            (
                PRINT_INT_SYMBOL,
                ctx.context.i64_type().into(),
                extended.into(),
            )
        }
        IRType::String => (
            PRINT_STRING_SYMBOL,
            ctx.context.ptr_type(AddressSpace::default()).into(),
            body_value.into_pointer_value().into(),
        ),
        IRType::Unit => {
            return Err(LlvmError::Codegen(
                "alpha LLVM does not yet emit Unit-typed main bodies".to_string(),
            ));
        }
    };
    let printer = declare_runtime_printer(ctx, printer_symbol, argument_type);
    ctx.builder
        .build_call(printer, &[argument], "")
        .map(|_| ())
        .map_err(|e| LlvmError::Codegen(format!("inkwell rejected print call: {e}")))
}

fn declare_runtime_printer<'ctx>(
    ctx: &EmitCtx<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    let signature = ctx.context.void_type().fn_type(&[argument_type], false);
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

fn sext_to_i64<'ctx>(
    ctx: &EmitCtx<'ctx>,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_int_s_extend(value, ctx.context.i64_type(), "print_arg")
        .map_err(|e| LlvmError::Codegen(format!("inkwell rejected sext for print arg: {e}")))
}

fn zext_to_i64<'ctx>(
    ctx: &EmitCtx<'ctx>,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_int_z_extend(value, ctx.context.i64_type(), "print_arg")
        .map_err(|e| LlvmError::Codegen(format!("inkwell rejected zext for print arg: {e}")))
}
