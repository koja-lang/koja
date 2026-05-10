//! `CString.to_string(self) -> String` — copy the `len` bytes of the
//! `CString { ptr, len }` struct payload into a fresh `[i64
//! bit_length][len bytes]` Expo string block. Caller retains
//! ownership of `self`; the produced `String` is a fresh owned heap
//! allocation, freed by the surrounding drop pipeline at end of
//! scope.

use expo_alpha_ir::IRFunction;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::declare_malloc_extern;

const STRING_HEADER_BYTES: u64 = 8;

pub(super) fn matches_id(id: &str) -> bool {
    id == "CString.to_string"
}

pub(super) fn emit_to_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    _id: &str,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let header_size = i64_ty.const_int(STRING_HEADER_BYTES, false);

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
                .map_err(|e| {
                    inkwell_err(
                        format_args!("build_extract_value for `{}`", function.symbol),
                        e,
                    )
                })?
                .into_pointer_value();
            let len = ctx
                .builder
                .build_extract_value(s, 1, "cs_len")
                .map_err(|e| {
                    inkwell_err(
                        format_args!("build_extract_value for `{}`", function.symbol),
                        e,
                    )
                })?
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

    let total = ctx
        .builder
        .build_int_add(header_size, byte_len, "total")
        .map_err(|e| inkwell_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let base_ptr: PointerValue<'ctx> = ctx
        .builder
        .build_call(malloc, &[total.into()], "base_ptr")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call malloc for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "malloc returned no value for `{}`",
                function.symbol
            ))
        })?
        .into_pointer_value();

    let bit_len: IntValue<'ctx> = ctx
        .builder
        .build_int_mul(byte_len, i64_ty.const_int(8, false), "bit_len")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(base_ptr, bit_len)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;

    let payload_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, base_ptr, &[header_size], "payload_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[payload_ptr.into(), c_ptr.into(), byte_len.into()],
            "",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call memcpy for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_return(Some(&payload_ptr))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}
