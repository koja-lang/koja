//! `Kernel.panic(message: String)` aborts the process with a
//! diagnostic. The body calls the runtime's `__koja_panic` with the
//! message and ends in `unreachable` so LLVM treats the call as
//! divergent. Paired with the IR-level `Statement::Expr`
//! Never-detection that caps the enclosing block with
//! `IRTerminator::Unreachable`, the typed Never return is preserved
//! end to end.

use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::nth_param;
use crate::runtime::declare_panic_extern;

pub(super) fn emit_panic<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let message = nth_param(function, llvm_function, 0, "message");
    let panic = declare_panic_extern(ctx);
    ctx.builder
        .build_call(panic, &[message.into()], "")
        .or_ice()?;
    ctx.builder.build_unreachable().or_ice().map(|_| ())
}
