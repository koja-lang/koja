//! `@intrinsic` methods on `Socket` from
//! [`koja/lib/net/src/net.koja`]:
//!
//! * `Socket.recv_from(self, count: Int) -> Result<Pair<Binary, SocketAddress>, String>`:
//!   datagram receive. Suspends until the fd is readable.
//! * `Socket.resolve(hostname: String) -> Result<List<IPAddress>, String>`:
//!   synchronous `getaddrinfo` shim.
//!
//! Both bodies follow the same skeleton: call the runtime helper,
//! branch on the null sentinel, build either `Result.Err` from
//! `koja_last_error()` or `Result.Ok` from the runtime's buffer.
//! Built against the [`layout`]-driven struct lookups
//! ([`Layouts::struct_type`] / [`Layouts::struct_field_ir_type`] /
//! [`Layouts::enum_variant_payload`]) and [`build_enum_value`] for
//! the `Result.Ok` / `Result.Err` construction. The marshaling-in-LLVM
//! shape is a pragmatic stopgap for the `Net` test surface. The two
//! intrinsics can be hoisted back into stdlib Koja with thinner
//! runtime helpers once the surface stabilizes.
//!
//! [`layout`]: crate::layout
//! [`Layouts::struct_type`]: crate::layout::Layouts::struct_type
//! [`Layouts::struct_field_ir_type`]: crate::layout::Layouts::struct_field_ir_type
//! [`Layouts::enum_variant_payload`]: crate::layout::Layouts::enum_variant_payload

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicType, IntType};
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
use crate::types::list_value_type;

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
        SocketMethod::RecvFrom => emit_recv_from(ctx, function, llvm_function),
        SocketMethod::Resolve => emit_resolve(ctx, function, llvm_function),
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
    let ip_symbol = resolve_list_element_symbol(ctx, result_symbol, function)?;
    let ip_struct = ctx.layouts.struct_type(ip_symbol.mangled());
    let ip_size = ctx
        .layouts
        .target_data
        .get_abi_size(&ip_struct.as_basic_type_enum());

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();

    let hostname = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Socket.resolve missing `hostname` param on `{}`",
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
        .build_int_mul(count, i64_ty.const_int(ip_size, false), "alloc_sz")
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
    let pair_symbol = resolve_pair_symbol(ctx, result_symbol, function)?;
    let sa_symbol = resolve_struct_field_symbol(ctx, &pair_symbol, 1, function)?;
    let ip_symbol = resolve_struct_field_symbol(ctx, &sa_symbol, 0, function)?;

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());

    let self_struct = llvm_function
        .get_nth_param(0)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "Socket.recv_from missing `self` param on `{}`",
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
            "Socket.recv_from missing `count` param on `{}`",
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

    let ip_struct = ctx.layouts.struct_type(ip_symbol.mangled());
    let ip_val = build_insert(
        ctx,
        ip_struct.get_undef().into(),
        ip_bin_ptr,
        0,
        "ip_with_bytes",
    )?
    .into_struct_value();

    let sa_struct = ctx.layouts.struct_type(sa_symbol.mangled());
    let sa_val = build_insert(
        ctx,
        sa_struct.get_undef().into(),
        ip_val.into(),
        0,
        "sa_with_ip",
    )?
    .into_struct_value();
    let sa_val =
        build_insert(ctx, sa_val.into(), recv_port, 1, "sa_with_port")?.into_struct_value();

    let pair_struct = ctx.layouts.struct_type(pair_symbol.mangled());
    let pair_val = build_insert(
        ctx,
        pair_struct.get_undef().into(),
        data_ptr,
        0,
        "pair_with_data",
    )?
    .into_struct_value();
    let pair_val =
        build_insert(ctx, pair_val.into(), sa_val.into(), 1, "pair_with_addr")?.into_struct_value();

    let ok = build_enum_value(ctx, result_symbol, RESULT_OK_TAG, &[pair_val.into()])?;
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

/// Walk `Result<List<IPAddress>, _>` and pull out the `IRSymbol`
/// of the `IPAddress` struct. The intrinsic emitter needs the
/// symbol to ABI-size the element and lay out the `List` buffer.
fn resolve_list_element_symbol(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
) -> Result<IRSymbol, LlvmError> {
    let ok_field = single_ok_payload(ctx, result_symbol, function, "Socket.resolve")?;
    let inner = match ok_field {
        IRType::List(inner) => *inner,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Socket.resolve Ok payload expected to be List<_>, got `{other:?}`",
            )));
        }
    };
    match inner {
        IRType::Struct(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "Socket.resolve Ok payload `List<T>` element expected to be a Struct, got `{other:?}`",
        ))),
    }
}

/// Walk `Result<Pair<Binary, SocketAddress>, _>` and pull out the
/// `IRSymbol` of the `Pair` struct. The intrinsic emitter then
/// recursively walks the pair's fields to reach `SocketAddress`
/// and `IPAddress`.
fn resolve_pair_symbol(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    function: &IRFunction,
) -> Result<IRSymbol, LlvmError> {
    let ok_field = single_ok_payload(ctx, result_symbol, function, "Socket.recv_from")?;
    match ok_field {
        IRType::Struct(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "Socket.recv_from Ok payload expected to be a Struct (Pair), got `{other:?}`",
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

/// `IRSymbol` of the struct at `index` inside `struct_symbol`. The
/// `Pair` / `SocketAddress` walk needs this to reach the inner
/// `SocketAddress` and `IPAddress` types without hardcoding their
/// identifier strings.
fn resolve_struct_field_symbol(
    ctx: &EmitContext<'_>,
    struct_symbol: &IRSymbol,
    index: usize,
    function: &IRFunction,
) -> Result<IRSymbol, LlvmError> {
    match ctx.layouts.struct_field_ir_type(struct_symbol, index) {
        IRType::Struct(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "field {index} of struct `{struct_symbol}` expected to be a Struct, got `{other:?}` \
             (symbol `{}`)",
            function.symbol,
        ))),
    }
}
