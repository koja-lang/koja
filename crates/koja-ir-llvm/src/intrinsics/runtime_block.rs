use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::heap_payload;

pub(super) fn emit_adopt_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    ctx.builder
        .build_return(Some(&payload))
        .or_ice()
        .map(|_| ())
}
