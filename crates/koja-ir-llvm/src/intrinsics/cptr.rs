//! `CPtr<T>` family: raw, manually-managed C pointers. Each call
//! site monomorphizes to a separate intrinsic body via the receiver
//! pinning (`Global.CPtr_$UInt8$.alloc` and `Global.CPtr_$Float32$.alloc`
//! emit distinct functions). The dispatch id stays the bare
//! `CPtr.<method>` since [`crate::intrinsics::emit_intrinsic_body`]
//! cannot see the type args otherwise. The pointee `IRType` lives on
//! `params[0].ty` for instance methods and on `return_type` for
//! `alloc`/`null`. [`pointee`] picks the right slot.
//!
//! Bodies are inline LLVM IR: `address` is a `ptrtoint`, `alloc`
//! calls `malloc(count * sizeof(T))`, `borrow` returns a Binary's
//! payload pointer as-is, `copy` mallocs an owned byte copy, `free`
//! calls libc `free`, `null` returns `null`, `null?` compares
//! against `null`, `offset` issues a typed GEP, `read` / `write`
//! load / store at the typed pointer (a float `read` traps on NaN or
//! inf), and `to_binary` copies `len` bytes into a managed Binary.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};
use koja_ir::panics::CPTR_READ_NON_FINITE_MESSAGE;
use koja_ir::{CPtrMethod, IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_alloc_size, init_heap_block, load_bit_length};
use crate::emit::ops::{emit_fault_guard, emit_finite_guard};
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::{nth_int, nth_param, nth_pointer};
use crate::runtime::{declare_free_extern, declare_malloc_extern, declare_memcpy_extern};
use crate::types::ir_basic_type;

pub(super) fn emit_cptr<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: CPtrMethod,
) -> Result<(), LlvmError> {
    match method {
        CPtrMethod::Address => emit_address(ctx, function, llvm_function),
        CPtrMethod::Alloc => emit_alloc(ctx, function, llvm_function),
        CPtrMethod::Borrow => emit_borrow(ctx, function, llvm_function),
        CPtrMethod::Copy => emit_copy(ctx, function, llvm_function),
        CPtrMethod::Free => emit_free(ctx, function, llvm_function),
        CPtrMethod::Null => emit_null(ctx),
        CPtrMethod::NullQ => emit_null_check(ctx, function, llvm_function),
        CPtrMethod::Offset => emit_offset(ctx, function, llvm_function),
        CPtrMethod::Read => emit_read(ctx, function, llvm_function),
        CPtrMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
        CPtrMethod::Write => emit_write(ctx, function, llvm_function),
    }
}

/// Resolve the pointee `T` for a `CPtr<T>` intrinsic. `alloc` /
/// `null` carry it on the return type. Every other method receives
/// `self: CPtr<T>` as `params[0]`. Panics if neither slot is a
/// `CPtr`.
fn pointee(method: CPtrMethod, function: &IRFunction) -> Result<&IRType, LlvmError> {
    let candidate = match method {
        CPtrMethod::Alloc | CPtrMethod::Null => &function.return_type,
        _ => &function.params[0].ty,
    };
    match candidate {
        IRType::CPtr(inner) => Ok(inner),
        other => panic!(
            "CPtr.{method:?} expected a `CPtr<T>` slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ),
    }
}

/// `address(self) -> Int`: the pointer value as an integer bit
/// pattern (`ptrtoint`). A null pointer yields 0.
fn emit_address<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let address = ctx
        .builder
        .build_ptr_to_int(self_ptr, ctx.context.i64_type(), "address")
        .or_ice()?;
    ctx.builder
        .build_return(Some(&address))
        .or_ice()
        .map(|_| ())
}

fn emit_null<'ctx>(ctx: &EmitContext<'ctx>) -> Result<(), LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_return(Some(&ptr_ty.const_null()))
        .or_ice()
        .map(|_| ())
}

fn emit_alloc<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let inner = pointee(CPtrMethod::Alloc, function)?;
    let basic = ir_basic_type(ctx, inner)?;
    let element_size = basic.size_of().unwrap_or_else(|| {
        panic!(
            "CPtr.alloc cannot compute size of pointee `{inner:?}` (symbol `{}`)",
            function.symbol,
        )
    });
    let count = nth_int(function, llvm_function, 0, "count")?;
    guard_nonnegative(ctx, count, "CPtr.alloc count cannot be negative")?;
    let total = ctx
        .builder
        .build_int_mul(count, element_size, "alloc_bytes")
        .or_ice()?;
    let is_empty = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            total,
            total.get_type().const_zero(),
            "is_empty",
        )
        .or_ice()?;
    let empty_block = ctx.context.append_basic_block(llvm_function, "empty");
    let allocate_block = ctx.context.append_basic_block(llvm_function, "allocate");
    ctx.builder
        .build_conditional_branch(is_empty, empty_block, allocate_block)
        .or_ice()?;

    ctx.builder.position_at_end(empty_block);
    let null = ctx.context.ptr_type(AddressSpace::default()).const_null();
    ctx.builder.build_return(Some(&null)).or_ice()?;

    ctx.builder.position_at_end(allocate_block);
    let malloc = declare_malloc_extern(ctx);
    let raw = ctx.call_basic(malloc, &[total.into()], "alloc_ptr")?;
    ctx.builder.build_return(Some(&raw)).or_ice().map(|_| ())
}

/// `borrow(bytes: Binary) -> CPtr<UInt8>`: zero-cost view. `Binary`
/// lowers to its payload pointer, which is already byte-addressable,
/// so return the argument as-is. The typecheck position check keeps
/// the result from outliving the statement (and thus the source).
fn emit_borrow<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = nth_pointer(function, llvm_function, 0, "bytes")?;
    ctx.builder
        .build_return(Some(&payload))
        .or_ice()
        .map(|_| ())
}

/// `copy(bytes: Binary) -> CPtr<UInt8>`: malloc `byte_size` bytes and
/// memcpy the payload. The result is a bare C buffer (no Binary
/// header, no NUL) the caller owns. Empty input returns null,
/// mirroring `alloc(0)` and the eval backend.
fn emit_copy<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let payload = nth_pointer(function, llvm_function, 0, "bytes")?;
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .or_ice()?;
    let is_empty = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            byte_count,
            i64_ty.const_zero(),
            "is_empty",
        )
        .or_ice()?;
    let empty_block = ctx.context.append_basic_block(llvm_function, "empty");
    let allocate_block = ctx.context.append_basic_block(llvm_function, "allocate");
    ctx.builder
        .build_conditional_branch(is_empty, empty_block, allocate_block)
        .or_ice()?;

    ctx.builder.position_at_end(empty_block);
    let null = ctx.context.ptr_type(AddressSpace::default()).const_null();
    ctx.builder.build_return(Some(&null)).or_ice()?;

    ctx.builder.position_at_end(allocate_block);
    let malloc = declare_malloc_extern(ctx);
    let raw = ctx.call_basic(malloc, &[byte_count.into()], "copy_ptr")?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(memcpy, &[raw.into(), payload.into(), byte_count.into()], "")
        .or_ice()?;
    ctx.builder.build_return(Some(&raw)).or_ice().map(|_| ())
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
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
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
    // SAFETY: `CPtr` is the raw pointer surface. Staying inside the
    // allocation is the Koja caller's contract, not this emitter's.
    let gep = unsafe {
        ctx.builder
            .build_gep(element_ty, self_ptr, &[n], "offset_ptr")
            .or_ice()?
    };
    ctx.builder.build_return(Some(&gep)).or_ice().map(|_| ())
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
        .or_ice()?;
    // Foreign memory is the other boundary (with extern returns) where
    // NaN or inf could enter a finite-only float type.
    if let BasicValueEnum::FloatValue(float) = val {
        emit_finite_guard(ctx, float, CPTR_READ_NON_FINITE_MESSAGE)?;
    }
    ctx.builder.build_return(Some(&val)).or_ice().map(|_| ())
}

fn emit_write<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let value = nth_param(function, llvm_function, 1, "value")?;
    ctx.builder.build_store(self_ptr, value).or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
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
        .or_ice()?;
    ctx.builder.build_return(Some(&cmp)).or_ice().map(|_| ())
}

/// `to_binary(self, len): Binary`: malloc a heap block and `memcpy`
/// `len` bytes from the source pointer. Returns the payload pointer
/// (`base + HEADER_BYTES`) per the `Binary` ABI. Caller retains
/// ownership of `self`. The produced `Binary` is a fresh owned heap
/// allocation.
fn emit_to_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();

    let src_ptr = nth_pointer(function, llvm_function, 0, "self")?;
    let byte_len = nth_int(function, llvm_function, 1, "len")?;
    guard_nonnegative(ctx, byte_len, "CPtr.to_binary length cannot be negative")?;

    let total = block_alloc_size(ctx, byte_len, false, "total")?;
    let malloc = declare_malloc_extern(ctx);
    let base_ptr = ctx
        .call_basic(malloc, &[total.into()], "base_ptr")?
        .into_pointer_value();

    let bit_len = ctx
        .builder
        .build_int_mul(byte_len, i64_ty.const_int(8, false), "bit_len")
        .or_ice()?;
    let payload_ptr = init_heap_block(ctx, base_ptr, bit_len, "cptr_str")?;
    let has_bytes = ctx
        .builder
        .build_int_compare(
            IntPredicate::NE,
            byte_len,
            byte_len.get_type().const_zero(),
            "has_bytes",
        )
        .or_ice()?;
    let copy_source = ctx
        .builder
        .build_select(has_bytes, src_ptr, payload_ptr, "copy_source")
        .or_ice()?
        .into_pointer_value();
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[payload_ptr.into(), copy_source.into(), byte_len.into()],
            "",
        )
        .or_ice()?;
    ctx.builder
        .build_return(Some(&payload_ptr))
        .or_ice()
        .map(|_| ())
}

fn guard_nonnegative<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: IntValue<'ctx>,
    message: &str,
) -> Result<(), LlvmError> {
    let negative = ctx
        .builder
        .build_int_compare(
            IntPredicate::SLT,
            value,
            value.get_type().const_zero(),
            "negative",
        )
        .or_ice()?;
    emit_fault_guard(ctx, negative, message, "negative")
}
