//! `CPtr<T>` family — raw, manually-managed C pointers. Each call
//! site monomorphizes to a separate intrinsic body via the receiver
//! pinning (`Global.CPtr_$UInt8$.alloc` and `Global.CPtr_$Float32$.alloc`
//! emit distinct functions); the dispatch id stays the bare
//! `CPtr.<method>` since [`crate::intrinsics::emitter_for`] cannot see
//! the type args otherwise. The pointee `IRType` lives on
//! `params[0].ty` for instance methods and on `return_type` for
//! `alloc`/`null`; [`pointee`] picks the right slot.
//!
//! Mirrors v1 [`expo_codegen::intrinsics::cptr`] one-to-one, ported
//! to the alpha emit context. Bodies are inline LLVM IR — `null`
//! returns `null`, `alloc` calls `malloc(count * sizeof(T))`, `free`
//! calls libc `free`, `offset` issues a typed GEP, `read` / `write`
//! load / store at the typed pointer, `null?` compares against
//! `null`, `to_string` is a zero-cost reinterpret (the pointer
//! already points to a valid Expo string payload), `to_binary`
//! malloc's a length-prefixed block and memcpy's `len` bytes from
//! the source pointer.

use expo_alpha_ir::{CPtrMethod, IRFunction, IRType};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::module::Linkage;
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::ir_basic_type;

/// Bit-length header prepended to every Expo `Binary` / `String`
/// payload — stored as `i64` immediately before the payload pointer.
/// Matches v1's `STRING_HEADER_BYTES` so eval / native produce
/// byte-identical heap layouts.
const STRING_HEADER_BYTES: u64 = 8;

pub(super) fn emit_cptr<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: CPtrMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    match method {
        CPtrMethod::Alloc => emit_alloc(ctx, function, llvm_function),
        CPtrMethod::Free => emit_free(ctx, function, llvm_function),
        CPtrMethod::Null => emit_null(ctx, function),
        CPtrMethod::NullQ => emit_null_check(ctx, function, llvm_function),
        CPtrMethod::Offset => emit_offset(ctx, function, llvm_function),
        CPtrMethod::Read => emit_read(ctx, function, llvm_function),
        CPtrMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
        CPtrMethod::ToString => emit_to_string(ctx, function, llvm_function),
        CPtrMethod::Write => emit_write(ctx, function, llvm_function),
    }
}

/// Resolve the pointee `T` for a `CPtr<T>` intrinsic. `alloc` /
/// `null` carry it on the return type; every other method receives
/// `self: CPtr<T>` as `params[0]`. Falls through to a codegen error
/// if neither slot is a `CPtr`.
fn pointee(method: CPtrMethod, function: &IRFunction) -> Result<&IRType, LlvmError> {
    let candidate = match method {
        CPtrMethod::Alloc | CPtrMethod::Null => &function.return_type,
        _ => &function.params[0].ty,
    };
    match candidate {
        IRType::CPtr(inner) => Ok(inner),
        other => Err(LlvmError::Codegen(format!(
            "CPtr.{method:?} expected a `CPtr<T>` slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}

fn emit_null<'ctx>(ctx: &EmitContext<'ctx>, function: &IRFunction) -> Result<(), LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_return(Some(&ptr_ty.const_null()))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_alloc<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let inner = pointee(CPtrMethod::Alloc, function)?;
    let basic = ir_basic_type(ctx, inner)?;
    let element_size = basic.size_of().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "CPtr.alloc cannot compute size of pointee `{inner:?}` (symbol `{}`)",
            function.symbol,
        ))
    })?;
    let count = nth_int(function, llvm_function, 0, "count")?;
    let total = ctx
        .builder
        .build_int_mul(count, element_size, "alloc_bytes")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let raw = ctx
        .builder
        .build_call(malloc, &[total.into()], "alloc_ptr")
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
                function.symbol,
            ))
        })?;
    ctx.builder
        .build_return(Some(&raw))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_free<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[self_ptr.into()], "")
        .map_err(|e| inkwell_err(format_args!("build_call free for `{}`", function.symbol), e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_offset<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let inner = pointee(CPtrMethod::Offset, function)?;
    let element_ty = ir_basic_type(ctx, inner)?;
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let n = nth_int(function, llvm_function, 1, "n")?;
    let gep = unsafe {
        ctx.builder
            .build_gep(element_ty, self_ptr, &[n], "offset_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_return(Some(&gep))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_read<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let inner = pointee(CPtrMethod::Read, function)?;
    let element_ty = ir_basic_type(ctx, inner)?;
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let val = ctx
        .builder
        .build_load(element_ty, self_ptr, "read_val")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?;
    ctx.builder
        .build_return(Some(&val))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_write<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let value = llvm_function.get_nth_param(1).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "CPtr.write missing `value` param on `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_store(self_ptr, value)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_null_check<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let cmp = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, self_ptr, ptr_ty.const_null(), "is_null")
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

/// `to_string(self): self` — the alpha runtime requires the pointer
/// to already point at a valid length-prefixed Expo string payload
/// (same layout as `String`); this method is therefore a zero-cost
/// reinterpret.
fn emit_to_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    ctx.builder
        .build_return(Some(&self_ptr))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

/// `to_binary(self, len): Binary` — malloc a `[i64 bit_len][len bytes]`
/// block and `memcpy` `len` bytes from the source pointer. Returns a
/// pointer to the payload (`base + 8`) per the alpha `Binary` ABI.
/// Caller retains ownership of `self`; the produced `Binary` is a
/// fresh owned heap allocation.
fn emit_to_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let header_size = i64_ty.const_int(STRING_HEADER_BYTES, false);

    let src_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let byte_len = nth_int(function, llvm_function, 1, "len")?;

    let total = ctx
        .builder
        .build_int_add(header_size, byte_len, "total")
        .map_err(|e| inkwell_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let base_ptr = ctx
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
                function.symbol,
            ))
        })?
        .into_pointer_value();

    // Header is `byte_len * 8` (bit length) at offset 0.
    let bit_len = ctx
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
            &[payload_ptr.into(), src_ptr.into(), byte_len.into()],
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

fn nth_pointer<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "missing param `{name}` (#{index}) on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::PointerValue(p) => Ok(p),
        other => Err(LlvmError::Codegen(format!(
            "expected pointer for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
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

/// Declare (or look up) the libc `memcpy` extern. Used by
/// [`emit_to_binary`] and CString conversions to copy raw bytes into
/// a freshly-allocated payload block. Signature:
/// `i8* memcpy(i8* dst, i8* src, i64 n)`.
pub(super) fn declare_memcpy_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function("memcpy") {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function("memcpy", signature, Some(Linkage::External))
}
