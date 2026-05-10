//! `Int.parse(input: String) -> Result<Int, String>` and
//! `Float.parse(input: String) -> Result<Float, String>`. Both
//! delegate to runtime helpers (`__expo_alpha_int_parse`,
//! `__expo_alpha_float_parse`) that produce the boxed Result via
//! the runtime crate's pre-allocated layout. Calling convention
//! mirrors the Result enum's outer-blob shape: the helper writes
//! into a caller-allocated outer-blob slot and returns it by value.
//!
//! Today the body just unreachable-traps so reachable callers fail
//! loudly until the runtime helpers land. Surfacing as an explicit
//! "feature gap" instead of a silent miscompile keeps the eager-
//! lower-stdlib path honest.

use expo_alpha_ir::IRFunction;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

pub(super) fn matches_id(id: &str) -> bool {
    matches!(id, "Int.parse" | "Float.parse")
}

pub(super) fn emit_parse<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    _id: &str,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    ctx.builder.build_unreachable().map(|_| ()).map_err(|e| {
        inkwell_err(
            format_args!("build_unreachable for `{}`", function.symbol),
            e,
        )
    })
}
