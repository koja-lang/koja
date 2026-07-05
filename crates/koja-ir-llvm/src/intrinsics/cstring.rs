//! `CString.to_string(self) -> String`: copy the `len` bytes of the
//! `CString { ptr, len }` struct payload into a fresh
//! `[i64 rc][i64 bit_length][len bytes][NUL]` Koja string block
//! (`rc = 1`, trailing NUL for libc compat, `String.length`/equality
//! rely on the terminator). Caller retains ownership of `self`. The
//! produced `String` is a fresh owned heap allocation, freed by the
//! surrounding drop pipeline at end of scope.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_alloc_size, init_heap_block};
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::declare_malloc_extern;

pub(super) fn emit_to_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let i64_ty = ctx.context.i64_type();

    let cs_val = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "CString.to_string missing `self` param on `{}`",
            function.symbol,
        ))
    })?;
    let (c_ptr, byte_len) = match cs_val {
        BasicValueEnum::StructValue(s) => {
            let ptr = ctx
                .builder
                .build_extract_value(s, 0, "cs_ptr")
                .or_ice()?
                .into_pointer_value();
            let len = ctx
                .builder
                .build_extract_value(s, 1, "cs_len")
                .or_ice()?
                .into_int_value();
            (ptr, len)
        }
        other => {
            return Err(LlvmError::Codegen(format!(
                "CString.to_string expected struct receiver on `{}`, got `{other:?}`",
                function.symbol,
            )));
        }
    };

    let total = block_alloc_size(ctx, byte_len, true, "total")?;
    let malloc = declare_malloc_extern(ctx);
    let base_ptr: PointerValue<'ctx> = ctx
        .call_basic(malloc, &[total.into()], "base_ptr")?
        .into_pointer_value();

    let bit_len: IntValue<'ctx> = ctx
        .builder
        .build_int_mul(byte_len, i64_ty.const_int(8, false), "bit_len")
        .or_ice()?;
    let payload_ptr = init_heap_block(ctx, base_ptr, bit_len, "cstring_str")?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[payload_ptr.into(), c_ptr.into(), byte_len.into()],
            "",
        )
        .or_ice()?;
    let nul_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(ctx.context.i8_type(), payload_ptr, &[byte_len], "nul_ptr")
            .or_ice()?
    };
    ctx.builder
        .build_store(nul_ptr, ctx.context.i8_type().const_zero())
        .or_ice()?;
    ctx.builder
        .build_return(Some(&payload_ptr))
        .or_ice()
        .map(|_| ())
}
