//! `@intrinsic` methods on `Socket` from
//! `koja/lib/net/src/net.koja`:
//!
//! * `Socket.recv_from_raw(self, count: Int) -> Result<(Binary, Binary, Int), Int>`
//!   is the raw datagram receive. It suspends until the fd is readable.
//! * `Socket.resolve_raw(hostname: String) -> Result<List<Binary>, Int>`
//!   is the raw synchronous `getaddrinfo` shim.
//!
//! Both bodies follow the same skeleton. They call the runtime helper
//! with an out slot for the buffer, branch on a negative status, and
//! build either `Result.Err` from the status or `Result.Ok` from the
//! runtime's buffer. The status is the failure value of the call.
//! The backend marshals only runtime ABI values. Ordinary Koja code
//! constructs `IPAddress` and `SocketAddress`, so their layouts never
//! become part of this boundary.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicType, BasicTypeEnum, IntType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantPayload, SocketMethod};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::result;
use crate::intrinsics::util::{
    build_list_struct, expect_enum_symbol, extract_int, nth_param, nth_struct, ret,
};
use crate::runtime::{
    declare_free_extern, declare_malloc_extern, declare_memcpy_extern,
    declare_socket_recv_from_extern, declare_socket_resolve_extern,
};
use crate::types::ir_basic_type;

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
    match method {
        SocketMethod::RecvFromRaw => emit_recv_from(ctx, function, llvm_function),
        SocketMethod::ResolveRaw => emit_resolve(ctx, function, llvm_function),
    }
}

fn emit_resolve<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let result_symbol = expect_enum_symbol(&function.return_type, function, "Socket.resolve_raw");
    validate_resolve_payload(ctx, result_symbol, function);

    let binary_size = binary_pointer_size(ctx, function, "Socket.resolve_raw")?;

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let hostname = nth_param(function, llvm_function, 0, "hostname");

    let out_slot = ctx.build_entry_alloca(ptr_ty, "resolve_out");
    let resolve_fn = declare_socket_resolve_extern(ctx);
    let status = ctx
        .call_basic(
            resolve_fn,
            &[hostname.into(), out_slot.into()],
            "resolve_status",
        )?
        .into_int_value();

    let (ok_bb, err_bb) = branch_on_failure(ctx, llvm_function, status)?;

    ctx.builder.position_at_end(err_bb);
    let err = build_err(ctx, result_symbol, status)?;
    ret(ctx, err)?;

    ctx.builder.position_at_end(ok_bb);
    let result_ptr = ctx
        .builder
        .build_load(ptr_ty, out_slot, "resolve_buf")
        .or_ice()?
        .into_pointer_value();
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
    let ok = build_enum_value(
        ctx,
        result_symbol,
        result::ok_tag(ctx, result_symbol),
        &[list_val.into()],
    )?;
    ret(ctx, ok)
}

fn emit_recv_from<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let result_symbol = expect_enum_symbol(&function.return_type, function, "Socket.recv_from_raw");
    let received_type = resolve_recv_from_payload(ctx, result_symbol, function);
    binary_pointer_size(ctx, function, "Socket.recv_from_raw")?;

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let self_struct = nth_struct(function, llvm_function, 0, "self");
    let fd_struct = ctx
        .builder
        .build_extract_value(self_struct, 0, "fd_struct")
        .or_ice()?
        .into_struct_value();
    let fd = extract_int(ctx, fd_struct, 0, "fd")?;
    let count_val = nth_param(function, llvm_function, 1, "count");

    let out_slot = ctx.build_entry_alloca(ptr_ty, "recv_out");
    let recv_fn = declare_socket_recv_from_extern(ctx);
    let status = ctx
        .call_basic(
            recv_fn,
            &[fd.into(), count_val.into(), out_slot.into()],
            "recv_status",
        )?
        .into_int_value();

    let (ok_bb, err_bb) = branch_on_failure(ctx, llvm_function, status)?;

    ctx.builder.position_at_end(err_bb);
    let err = build_err(ctx, result_symbol, status)?;
    ret(ctx, err)?;

    ctx.builder.position_at_end(ok_bb);
    let result_ptr = ctx
        .builder
        .build_load(ptr_ty, out_slot, "recv_buf")
        .or_ice()?
        .into_pointer_value();
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

    let ok = build_enum_value(
        ctx,
        result_symbol,
        result::ok_tag(ctx, result_symbol),
        &[received.into()],
    )?;
    ret(ctx, ok)
}

/// Build `Result.Err(status)`. The status is the negative `i64` the
/// runtime helper returned, which is the LLVM-level representation of
/// an `IRType::Int64`, so it goes into the `Err` payload slot as is.
fn build_err<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    status: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    build_enum_value(
        ctx,
        result_symbol,
        result::err_tag(ctx, result_symbol),
        &[status.into()],
    )
}

/// Append `ok` / `err` blocks to `llvm_function` and conditional-
/// branch on `status < 0`. A negative status is the failure value of
/// the runtime call. The err branch returns it, and the ok branch
/// unpacks the heap buffer the helper wrote through its out slot.
fn branch_on_failure<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    status: IntValue<'ctx>,
) -> Result<(BasicBlock<'ctx>, BasicBlock<'ctx>), LlvmError> {
    let zero = ctx.context.i64_type().const_zero();
    let failed = ctx
        .builder
        .build_int_compare(IntPredicate::SLT, status, zero, "failed")
        .or_ice()?;
    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let err_bb = ctx.context.append_basic_block(llvm_function, "err");
    ctx.builder
        .build_conditional_branch(failed, err_bb, ok_bb)
        .or_ice()?;
    Ok((ok_bb, err_bb))
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
    // SAFETY: callers pass field offsets of the runtime result
    // structs, which the runtime allocated in full.
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

fn binary_pointer_size(
    ctx: &EmitContext<'_>,
    function: &IRFunction,
    intrinsic_label: &str,
) -> Result<u64, LlvmError> {
    let binary_ty = ir_basic_type(ctx, &IRType::Binary)?;
    let BasicTypeEnum::PointerType(binary_ptr_ty) = binary_ty else {
        panic!(
            "{intrinsic_label} on `{}` requires Binary to use the runtime pointer ABI",
            function.symbol,
        );
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
    assert!(
        binary_size == runtime_pointer_size,
        "{intrinsic_label} on `{}` requires Binary to match the runtime pointer ABI \
         ({binary_size} bytes != {runtime_pointer_size} bytes)",
        function.symbol,
    );

    Ok(binary_size)
}

/// Require the raw DNS result shape that matches the runtime pointer
/// array. Domain address construction stays in ordinary Koja code.
fn validate_resolve_payload(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
) {
    let ok_field = single_ok_payload(ctx, result_symbol, function, "Socket.resolve_raw");
    let inner = match ok_field {
        IRType::List(inner) => *inner,
        other => {
            panic!("Socket.resolve_raw Ok payload expected to be List<Binary>, got `{other:?}`")
        }
    };
    match inner {
        IRType::Binary => (),
        other => panic!(
            "Socket.resolve_raw Ok payload expected to be List<Binary>, got `List<{other:?}>`",
        ),
    }
}

/// Walk `Result<(Binary, Binary, Int), _>` and return the raw tuple
/// type used by the runtime buffer.
fn resolve_recv_from_payload(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
) -> IRType {
    let ok_field = single_ok_payload(ctx, result_symbol, function, "Socket.recv_from_raw");
    match ok_field {
        IRType::Tuple(elements) => {
            let [IRType::Binary, IRType::Binary, IRType::Int64] = elements.as_slice() else {
                panic!(
                    "Socket.recv_from_raw Ok payload expected `(Binary, Binary, Int)`, \
                     got `{elements:?}`",
                );
            };
            IRType::Tuple(elements)
        }
        other => panic!("Socket.recv_from_raw Ok payload expected a Tuple, got `{other:?}`"),
    }
}

/// Single-payload `Ok` extractor shared by both intrinsics. The
/// IR seal pins `Result.Ok` to exactly one field, so a shape
/// violation panics.
fn single_ok_payload(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
    intrinsic_label: &str,
) -> IRType {
    let payload = ctx
        .layouts
        .enum_variant_payload(result_symbol, result::ok_tag(ctx, result_symbol));
    match payload {
        IRVariantPayload::Tuple(types) if types.len() == 1 => types.into_iter().next().unwrap(),
        IRVariantPayload::Struct(fields) if fields.len() == 1 => {
            fields.into_iter().next().unwrap().ir_type
        }
        other => panic!(
            "{intrinsic_label} on `{}` Ok variant has unexpected payload `{other:?}` \
             (expected single-field, IR seal invariant violation)",
            function.symbol,
        ),
    }
}
