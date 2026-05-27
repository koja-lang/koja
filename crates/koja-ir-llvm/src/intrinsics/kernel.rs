//! `Kernel.panic(message: String)` — abort the process with a
//! diagnostic. v1 routed panic through the runtime's symbolicated
//! stack-trace helper; we keep the same surface (calls
//! `__koja_panic` with the message) and falls back to libc
//! `abort` if the helper isn't linked. Either way the body ends in
//! `unreachable` so LLVM treats the call as divergent — paired with
//! the IR-level `Statement::Expr` Never-detection that caps the
//! enclosing block with `IRTerminator::Unreachable`, the typed
//! Never return is preserved end to end.

use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::declare_panic_extern;

pub(super) fn emit_panic<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let message = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Kernel.panic missing `message` param on `{}`",
            function.symbol,
        ))
    })?;
    let panic = declare_panic_extern(ctx);
    ctx.builder
        .build_call(panic, &[message.into()], "")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call panic for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder.build_unreachable().map(|_| ()).map_err(|e| {
        inkwell_err(
            format_args!("build_unreachable for `{}`", function.symbol),
            e,
        )
    })
}
