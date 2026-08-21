//! `@intrinsic` methods on `Socket` from
//! [`koja/lib/net/src/net.koja`]:
//!
//! * `Socket.recv_from_raw(self, count: Int) -> Result<(Binary, Binary, Int), String>`:
//!   raw datagram receive. Suspends until the fd is readable.
//! * `Socket.resolve_raw(hostname: String) -> Result<List<Binary>, String>`:
//!   raw synchronous `getaddrinfo` shim.
//!
//! Both bodies follow the same skeleton: call the runtime helper,
//! branch on the null sentinel, build either `Result.Err` from
//! `koja_last_error()` or `Result.Ok` from the runtime's buffer.
//! The backend marshals only runtime ABI values. Ordinary Koja code
//! constructs `IPAddress` and `SocketAddress`, so their layouts never
//! become part of this boundary.
//!
//! [`layout`]: crate::layout
//! [`Layouts::enum_variant_payload`]: crate::layout::Layouts::enum_variant_payload

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicType, BasicTypeEnum, IntType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantPayload, IRVariantTag, SocketMethod};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{
    declare_free_extern, declare_last_error_extern, declare_malloc_extern,
    declare_socket_recv_from_extern, declare_socket_resolve_extern,
};
use crate::types::{ir_basic_type, list_value_type};

/// `enum Result<T, E>` variant tag for `Ok(T)`. Lifted from
/// `koja/lib/global/src/kernel.koja`'s declaration order.
const RESULT_OK_TAG: IRVariantTag = IRVariantTag(0);
/// `enum Result<T, E>` variant tag for `Err(E)`.
const RESULT_ERR_TAG: IRVariantTag = IRVariantTag(1);

/// Byte count of the `i64 count` header the runtime writes at the
/// front of the `koja_socket_resolve` buffer. The IP-pointer array
/// starts immediately after this header.
const RESOLVE_HEADER_BYTES: u64 = 8;
/// Offset of `*u8 ip_bin` inside the runtime's
/// `koja_socket_recv_from` `[*u8 data, *u8 ip_bin, i64 port]` triple.
const RECV_FROM_IP_OFFSET: u64 = 8;
/// Offset of `i64 port` inside the same triple.
const RECV_FROM_PORT_OFFSET: u64 = 16;

pub(super) fn emit_socket<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: SocketMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    match method {
        SocketMethod::LastError => emit_last_error(ctx),
        SocketMethod::RecvFromRaw => emit_recv_from(ctx, function, llvm_function),
        SocketMethod::ResolveRaw => emit_resolve(ctx, function, llvm_function),
    }
}

fn emit_last_error(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let last_error = declare_last_error_extern(ctx);
    let message = ctx.call_basic(last_error, &[], "last_error")?;
    ctx.builder
        .build_return(Some(&message))
        .or_ice()
        .map(|_| ())
}

fn emit_resolve<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let result_symbol = expect_enum_symbol(&function.return_type, function)?;
    validate_resolve_payload(ctx, result_symbol, function)?;

    let binary_size = binary_pointer_size(ctx, function, "Socket.resolve_raw")?;

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();

    let hostname = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Socket.resolve_raw missing `hostname` param on `{}`",
            function.symbol,
        ))
    })?;

    let resolve_fn = declare_socket_resolve_extern(ctx);
    let result_ptr = ctx
        .call_basic(resolve_fn, &[hostname.into()], "resolve_buf")?
        .into_pointer_value();

    let (ok_bb, err_bb) = branch_on_null(ctx, llvm_function, result_ptr)?;

    ctx.builder.position_at_end(err_bb);
    let err = build_err(ctx, result_symbol)?;
    ret(ctx, err)?;

    ctx.builder.position_at_end(ok_bb);
    let count = build_load_int(ctx, i64_ty, result_ptr, "count")?;
    let alloc_size = ctx
        .builder
        .build_int_mul(count, i64_ty.const_int(binary_size, false), "alloc_sz")
        .or_ice()?;

    let malloc = declare_malloc_extern(ctx);
    let list_buf = ctx
        .call_basic(malloc, &[alloc_size.into()], "list_buf")?
        .into_pointer_value();

    let payload_start = build_gep_offset(
        ctx,
        i8_ty,
        result_ptr,
        i64_ty.const_int(RESOLVE_HEADER_BYTES, false),
        "payload_start",
    )?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[list_buf.into(), payload_start.into(), alloc_size.into()],
            "cpy",
        )
        .or_ice()?;

    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[result_ptr.into()], "free_buf")
        .or_ice()?;

    let list_val = build_list_struct(ctx, list_buf, count, count)?;
    let ok = build_enum_value(ctx, result_symbol, RESULT_OK_TAG, &[list_val.into()])?;
    ret(ctx, ok)
}

fn emit_recv_from<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let result_symbol = expect_enum_symbol(&function.return_type, function)?;
    let received_type = resolve_recv_from_payload(ctx, result_symbol, function)?;
    binary_pointer_size(ctx, function, "Socket.recv_from_raw")?;

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let self_struct = llvm_function
        .get_nth_param(0)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "Socket.recv_from_raw missing `self` param on `{}`",
                function.symbol,
            ))
        })?
        .into_struct_value();
    let fd_struct = ctx
        .builder
        .build_extract_value(self_struct, 0, "fd_struct")
        .or_ice()?
        .into_struct_value();
    let fd = ctx
        .builder
        .build_extract_value(fd_struct, 0, "fd")
        .or_ice()?
        .into_int_value();
    let count_val = llvm_function.get_nth_param(1).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Socket.recv_from_raw missing `count` param on `{}`",
            function.symbol,
        ))
    })?;

    let recv_fn = declare_socket_recv_from_extern(ctx);
    let result_ptr = ctx
        .call_basic(recv_fn, &[fd.into(), count_val.into()], "recv_buf")?
        .into_pointer_value();

    let (ok_bb, err_bb) = branch_on_null(ctx, llvm_function, result_ptr)?;

    ctx.builder.position_at_end(err_bb);
    let err = build_err(ctx, result_symbol)?;
    ret(ctx, err)?;

    ctx.builder.position_at_end(ok_bb);
    let data_ptr = ctx
        .builder
        .build_load(ptr_ty, result_ptr, "data_ptr")
        .or_ice()?;
    let ip_field_ptr = build_gep_offset(
        ctx,
        i8_ty,
        result_ptr,
        i64_ty.const_int(RECV_FROM_IP_OFFSET, false),
        "ip_field",
    )?;
    let ip_bin_ptr = ctx
        .builder
        .build_load(ptr_ty, ip_field_ptr, "ip_bin")
        .or_ice()?;
    let port_field_ptr = build_gep_offset(
        ctx,
        i8_ty,
        result_ptr,
        i64_ty.const_int(RECV_FROM_PORT_OFFSET, false),
        "port_field",
    )?;
    let recv_port = ctx
        .builder
        .build_load(i64_ty, port_field_ptr, "port")
        .or_ice()?;

    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[result_ptr.into()], "free_buf")
        .or_ice()?;

    let tuple_struct = ir_basic_type(ctx, &received_type)?.into_struct_type();
    let received = build_insert(
        ctx,
        tuple_struct.get_undef().into(),
        data_ptr,
        0,
        "tuple_with_data",
    )?
    .into_struct_value();
    let received =
        build_insert(ctx, received.into(), ip_bin_ptr, 1, "tuple_with_ip")?.into_struct_value();
    let received =
        build_insert(ctx, received.into(), recv_port, 2, "tuple_with_port")?.into_struct_value();

    let ok = build_enum_value(ctx, result_symbol, RESULT_OK_TAG, &[received.into()])?;
    ret(ctx, ok)
}

/// Build `Result.Err(koja_last_error())`. The runtime helper
/// returns a freshly-allocated Koja string payload pointer, which
/// is exactly the LLVM-level representation of an `IRType::String`,
/// so we can feed it straight into the `Err` payload slot without
/// any further marshaling.
fn build_err<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let last_error = declare_last_error_extern(ctx);
    let err_msg = ctx.call_basic(last_error, &[], "err_msg")?;
    build_enum_value(ctx, result_symbol, RESULT_ERR_TAG, &[err_msg])
}

/// Append `ok` / `err` blocks to `llvm_function` and conditional-
/// branch on `ptr == null`. The runtime helpers use null as the
/// error sentinel. The err branch reads `koja_last_error()`, the ok
/// branch unpacks the heap buffer.
fn branch_on_null<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    ptr: PointerValue<'ctx>,
) -> Result<(BasicBlock<'ctx>, BasicBlock<'ctx>), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let ptr_int = ctx
        .builder
        .build_ptr_to_int(ptr, i64_ty, "ptr_int")
        .or_ice()?;
    let null_int = ctx
        .builder
        .build_ptr_to_int(ptr_ty.const_null(), i64_ty, "null_int")
        .or_ice()?;
    let is_null = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, ptr_int, null_int, "is_null")
        .or_ice()?;
    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let err_bb = ctx.context.append_basic_block(llvm_function, "err");
    ctx.builder
        .build_conditional_branch(is_null, err_bb, ok_bb)
        .or_ice()?;
    Ok((ok_bb, err_bb))
}

/// `{ buf, len, cap }` `List<T>` SSA value. Both `len` and `cap`
/// hold `count` here because the resolve buffer is sized exactly
/// to its element count, so there's no growth headroom to mark.
fn build_list_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    buf: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let list_ty = list_value_type(ctx);
    let with_buf = build_insert(ctx, list_ty.get_undef().into(), buf.into(), 0, "with_buf")?
        .into_struct_value();
    let with_len =
        build_insert(ctx, with_buf.into(), len.into(), 1, "with_len")?.into_struct_value();
    let with_cap =
        build_insert(ctx, with_len.into(), cap.into(), 2, "with_cap")?.into_struct_value();
    Ok(with_cap)
}

fn build_insert<'ctx>(
    ctx: &EmitContext<'ctx>,
    aggregate: BasicValueEnum<'ctx>,
    value: BasicValueEnum<'ctx>,
    index: u32,
    name: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let aggregate = aggregate.into_struct_value();
    ctx.builder
        .build_insert_value(aggregate, value, index, name)
        .or_ice()
        .map(|v| v.into_struct_value().into())
}

fn build_gep_offset<'ctx>(
    ctx: &EmitContext<'ctx>,
    elem_ty: IntType<'ctx>,
    base: PointerValue<'ctx>,
    offset: IntValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    unsafe {
        ctx.builder
            .build_gep(elem_ty, base, &[offset], name)
            .or_ice()
    }
}

fn build_load_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: IntType<'ctx>,
    ptr: PointerValue<'ctx>,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_load(ty, ptr, name)
        .or_ice()
        .map(|v| v.into_int_value())
}

fn ret<'ctx>(ctx: &EmitContext<'ctx>, value: BasicValueEnum<'ctx>) -> Result<(), LlvmError> {
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "Socket intrinsic on `{}` expected an enum-typed return, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn binary_pointer_size(
    ctx: &EmitContext<'_>,
    function: &IRFunction,
    intrinsic_label: &str,
) -> Result<u64, LlvmError> {
    let binary_ty = ir_basic_type(ctx, &IRType::Binary)?;
    let BasicTypeEnum::PointerType(binary_ptr_ty) = binary_ty else {
        return Err(LlvmError::Codegen(format!(
            "{intrinsic_label} on `{}` requires Binary to use the runtime pointer ABI",
            function.symbol,
        )));
    };
    let binary_size = ctx
        .layouts
        .target_data
        .get_abi_size(&binary_ptr_ty.as_basic_type_enum());
    let runtime_pointer_size = ctx.layouts.target_data.get_abi_size(
        &ctx.context
            .ptr_type(AddressSpace::default())
            .as_basic_type_enum(),
    );
    if binary_size != runtime_pointer_size {
        return Err(LlvmError::Codegen(format!(
            "{intrinsic_label} on `{}` requires Binary to match the runtime pointer ABI \
             ({binary_size} bytes != {runtime_pointer_size} bytes)",
            function.symbol,
        )));
    }

    Ok(binary_size)
}

/// Require the raw DNS result shape that matches the runtime pointer
/// array. Domain address construction stays in ordinary Koja code.
fn validate_resolve_payload(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
) -> Result<(), LlvmError> {
    let ok_field = single_ok_payload(ctx, result_symbol, function, "Socket.resolve_raw")?;
    let inner = match ok_field {
        IRType::List(inner) => *inner,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Socket.resolve_raw Ok payload expected to be List<Binary>, got `{other:?}`",
            )));
        }
    };
    match inner {
        IRType::Binary => Ok(()),
        other => Err(LlvmError::Codegen(format!(
            "Socket.resolve_raw Ok payload expected to be List<Binary>, got `List<{other:?}>`",
        ))),
    }
}

/// Walk `Result<(Binary, Binary, Int), _>` and return the raw tuple
/// type used by the runtime buffer.
fn resolve_recv_from_payload(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
) -> Result<IRType, LlvmError> {
    let ok_field = single_ok_payload(ctx, result_symbol, function, "Socket.recv_from_raw")?;
    match ok_field {
        IRType::Tuple(elements) => {
            let [IRType::Binary, IRType::Binary, IRType::Int64] = elements.as_slice() else {
                return Err(LlvmError::Codegen(format!(
                    "Socket.recv_from_raw Ok payload expected `(Binary, Binary, Int)`, \
                     got `{elements:?}`",
                )));
            };
            Ok(IRType::Tuple(elements))
        }
        other => Err(LlvmError::Codegen(format!(
            "Socket.recv_from_raw Ok payload expected a Tuple, got `{other:?}`",
        ))),
    }
}

/// Single-payload `Ok` extractor shared by both intrinsics. The
/// IR seal pins `Result.Ok` to exactly one field. Surfaces a
/// codegen error (not a panic) on shape violations so the failure
/// mode is symmetric with the rest of the file.
fn single_ok_payload(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
    intrinsic_label: &str,
) -> Result<IRType, LlvmError> {
    let payload = ctx
        .layouts
        .enum_variant_payload(result_symbol, RESULT_OK_TAG);
    match payload {
        IRVariantPayload::Tuple(types) if types.len() == 1 => Ok(types.into_iter().next().unwrap()),
        IRVariantPayload::Struct(fields) if fields.len() == 1 => {
            Ok(fields.into_iter().next().unwrap().ir_type)
        }
        other => Err(LlvmError::Codegen(format!(
            "{intrinsic_label} on `{}` Ok variant has unexpected payload `{other:?}` \
             (expected single-field, IR seal invariant violation)",
            function.symbol,
        ))),
    }
}
