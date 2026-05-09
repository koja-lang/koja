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

use std::collections::HashSet;

use expo_alpha_ir::{IRBasicBlock, IRBlockId, IRTerminator, IRType};
use inkwell::AddressSpace;
use inkwell::module::Linkage;
use inkwell::values::{BasicValueEnum, IntValue};

use crate::ctx::EmitContext;
use crate::emit::{self, ValueMap, inkwell_err};
use crate::error::LlvmError;
use crate::function::declare_blocks;
use crate::runtime::{
    PRINT_BINARY_SYMBOL, PRINT_BITS_SYMBOL, PRINT_BOOL_SYMBOL, PRINT_F32_SYMBOL, PRINT_F64_SYMBOL,
    PRINT_INT_SYMBOL, PRINT_STRING_SYMBOL, declare_runtime_printer,
};

const APP_NAME_SYMBOL: &str = "__expo_app_name";
const ENTRY_SYMBOL: &str = "main";

/// Emit `__expo_app_name` as a null-terminated C-string constant.
/// The `expo-runtime` panic handler reads it for backtrace labels
/// (declared there as `extern static [c_char; 0]`); every
/// alpha-compiled binary defines it so the runtime archive links
/// cleanly regardless of codegen-unit partitioning.
pub(crate) fn emit_app_name_global(ctx: &EmitContext<'_>, app_name: &str) {
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
/// `IRType::Unit` trailings (e.g. a script body whose last
/// expression is `print("hello")`) skip the auto-print call entirely
/// — Unit has no value to print — and just emit `ret i64 0`. The
/// non-Unit path looks up the trailing value and dispatches through
/// [`emit_print_call`].
///
/// Empty bodies are illegal (sealed IR guarantees at least one
/// block), and the final IR block must end in `Return`. The seal
/// pass admits other terminators for non-trailing blocks; only the
/// entry function's last block carries the auto-print scaffolding.
pub(crate) fn emit_as_main<'ctx>(
    ctx: &EmitContext<'ctx>,
    blocks: &[IRBasicBlock],
    return_type: &IRType,
) -> Result<(), LlvmError> {
    let i64_type = ctx.context.i64_type();
    let signature = i64_type.fn_type(&[], false);
    let function = ctx
        .module
        .add_function(ENTRY_SYMBOL, signature, Some(Linkage::External));
    // The script-mode body is its own function from a slot-identity
    // perspective; flush any stragglers from a prior compile or
    // helper so `LocalDecl` registers cleanly here.
    ctx.reset_locals();
    let block_map = declare_blocks(ctx, function, blocks);
    let reachable = emit::reachable_blocks(blocks);
    let return_block_id = find_return_block(blocks, &reachable)?;

    let mut values: ValueMap<'ctx> = ValueMap::new();
    let phi_map = emit::declare_block_param_phis(ctx, blocks, &block_map, &mut values)?;
    for block in blocks {
        if !reachable.contains(&block.id) {
            // Same boundary stand-in as `define_function`: blocks the
            // CFG can't reach get `unreachable` so we never try to
            // materialize their (impossible-to-reach) value reads.
            emit::emit_unreachable_terminator(ctx, block.id, &block_map)?;
            continue;
        }
        let llvm_block = block_map[&block.id];
        ctx.builder.position_at_end(llvm_block);
        if block.id == return_block_id {
            let (next_values, terminator) =
                emit::emit_instructions(ctx, block, std::mem::take(&mut values))?;
            values = next_values;
            emit_main_return(ctx, return_type, terminator, &values)?;
        } else {
            emit::emit_block(ctx, block, &block_map, &phi_map, &mut values)?;
        }
    }
    Ok(())
}

/// Auto-print + `ret i64 0` synthesis for the trailing block of
/// `main`. `Unit` trailings skip the print call; everything else
/// looks up the trailing value and routes through
/// [`emit_print_call`]. Both paths finish with `ret i64 0` per the
/// host-runtime contract.
fn emit_main_return<'ctx>(
    ctx: &EmitContext<'ctx>,
    return_type: &IRType,
    terminator: &IRTerminator,
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let i64_type = ctx.context.i64_type();
    if !matches!(return_type, IRType::Unit) {
        let body_value = match terminator {
            IRTerminator::Return { value: Some(id) } => emit::lookup(values, *id)?,
            IRTerminator::Return { value: None } => {
                return Err(LlvmError::Codegen(format!(
                    "main return block has no value but its return type is `{return_type:?}`",
                )));
            }
            other => unreachable!("main return-block must terminate in Return; got {other:?}"),
        };
        emit_print_call(ctx, return_type, body_value)?;
    }
    ctx.builder
        .build_return(Some(&i64_type.const_int(0, false)))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return for main", e))
}

/// The [`IRBlockId`] of the unique *reachable* block ending in
/// `Return`. The auto-print wrapper around `main` patches in
/// `ret i64 0` after executing the body, so we need to know which
/// IR block carries the body's value before walking. Today's slice
/// produces exactly one reachable `Return`-terminated block per
/// function (the merge block of an `if` / `unless` falls through to
/// it via `Branch`); divergent if/else's may synthesize an
/// unreachable merge whose `Return` reads an unmaterialized
/// `BlockParam` — those don't count and are filtered out via
/// `reachable`. A missing or duplicate reachable `Return` is a
/// lowering bug we surface as a codegen error.
fn find_return_block(
    blocks: &[IRBasicBlock],
    reachable: &HashSet<IRBlockId>,
) -> Result<IRBlockId, LlvmError> {
    let mut found: Option<IRBlockId> = None;
    for block in blocks {
        if !reachable.contains(&block.id) {
            continue;
        }
        if matches!(block.terminator, IRTerminator::Return { .. }) {
            if found.is_some() {
                return Err(LlvmError::Codegen(
                    "alpha LLVM expects exactly one reachable Return-terminated block in `main`"
                        .to_string(),
                ));
            }
            found = Some(block.id);
        }
    }
    found.ok_or_else(|| {
        LlvmError::Codegen(
            "alpha LLVM expects at least one reachable Return-terminated block in `main`"
                .to_string(),
        )
    })
}

/// Pick the runtime printer for `return_type` and emit the call.
/// Integer / `Bool` widths extend to `i64` (sign- or zero-extended
/// per signedness); `Float32` / `Float64` flow as native f32 / f64;
/// `String` flows the payload pointer through unchanged (runtime
/// reads the v1 header at `ptr - 8` for byte length).
fn emit_print_call<'ctx>(
    ctx: &EmitContext<'ctx>,
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
        IRType::Binary => (
            PRINT_BINARY_SYMBOL,
            ctx.context.ptr_type(AddressSpace::default()).into(),
            body_value.into_pointer_value().into(),
        ),
        IRType::Bits => (
            PRINT_BITS_SYMBOL,
            ctx.context.ptr_type(AddressSpace::default()).into(),
            body_value.into_pointer_value().into(),
        ),
        IRType::String => (
            PRINT_STRING_SYMBOL,
            ctx.context.ptr_type(AddressSpace::default()).into(),
            body_value.into_pointer_value().into(),
        ),
        IRType::CPtr(pointee) => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not auto-print `CPtr<{pointee:?}>` return values \
                 (FFI pointers are opaque at the Expo level); call sites that need a \
                 print must project a primitive scalar before the trailing expression",
            )));
        }
        IRType::Enum(symbol) => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet auto-print enum values (return type \
                 `Enum({symbol})`); follow-up slice — until then, return a primitive \
                 (e.g. project a tag-discriminated `Bool` / `Int64`) from the trailing \
                 expression",
            )));
        }
        IRType::Struct(symbol) => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet auto-print struct values (return type \
                 `Struct({symbol})`); project a primitive field with `value.field` or \
                 wrap the call site so the trailing expression is a primitive",
            )));
        }
        IRType::Unit => {
            return Err(LlvmError::Codegen(
                "emit_print_call invoked with `IRType::Unit` — the Unit-typed trailing path \
                 in emit_as_main should have skipped this call (compiler bug)"
                    .to_string(),
            ));
        }
        IRType::Function { .. } => {
            return Err(LlvmError::Codegen(
                "alpha LLVM does not yet auto-print closure values; bind the closure to a \
                 local before the trailing expression"
                    .to_string(),
            ));
        }
    };
    let printer = declare_runtime_printer(ctx, printer_symbol, argument_type);
    ctx.builder
        .build_call(printer, &[argument], "")
        .map(|_| ())
        .map_err(|e| inkwell_err("print call", e))
}

fn sext_to_i64<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_int_s_extend(value, ctx.context.i64_type(), "print_arg")
        .map_err(|e| inkwell_err("sext for print arg", e))
}

fn zext_to_i64<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_int_z_extend(value, ctx.context.i64_type(), "print_arg")
        .map_err(|e| inkwell_err("zext for print arg", e))
}
