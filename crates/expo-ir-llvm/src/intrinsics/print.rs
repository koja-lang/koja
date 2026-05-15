//! `@intrinsic Global.print(s: String) -> Unit` — synthesize a body
//! that calls into the existing `__expo_alpha_print_string` runtime
//! helper. Same target the auto-print scaffolding uses for
//! `IRType::String` trailings, so the runtime contract (read v1
//! header at `ptr - 8`, write payload + trailing newline) is shared
//! between user-level `print(...)` calls and the temporary
//! auto-print wrapper.

use expo_alpha_ir::IRFunction;
use inkwell::AddressSpace;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::{PRINT_STRING_SYMBOL, declare_runtime_printer};

pub(super) fn emit_global_print<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let printer = declare_runtime_printer(
        ctx,
        PRINT_STRING_SYMBOL,
        ctx.context.ptr_type(AddressSpace::default()).into(),
    );
    let payload = llvm_function.get_nth_param(0).unwrap_or_else(|| {
        panic!(
            "intrinsic `{}` declared without a `String` payload param — \
             signature/IR drift",
            function.symbol,
        )
    });
    ctx.builder
        .build_call(printer, &[payload.into()], "")
        .map_err(|e| inkwell_err(format_args!("build_call for `{}`", function.symbol), e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}
