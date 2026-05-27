//! Runtime-symbol declarations for [`crate::intrinsics`] (which
//! calls them from compiler-synthesized `@intrinsic` bodies) and
//! [`crate::main_wrapper`]'s spawn / main-done hand-off.
//!
//! Each runtime helper lives in `koja-runtime/src/intrinsics.rs`; this
//! module owns the LLVM-side declarations so the callers stamp
//! exactly one `module.get_function` lookup per symbol.

use inkwell::AddressSpace;
use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;

pub(crate) const CONCAT_BITS_SYMBOL: &str = "__koja_concat_bits";
pub(crate) const FORMAT_BOOL_SYMBOL: &str = "koja_format_bool";
pub(crate) const FORMAT_F32_SYMBOL: &str = "koja_format_f32";
pub(crate) const FORMAT_F64_SYMBOL: &str = "koja_format_f64";
pub(crate) const FORMAT_I64_SYMBOL: &str = "koja_format_i64";
pub(crate) const FORMAT_U64_SYMBOL: &str = "koja_format_u64";
pub(crate) const FLOAT_PARSE_SYMBOL: &str = "koja_float_parse";
pub(crate) const FREE_SYMBOL: &str = "free";
pub(crate) const INT_PARSE_SYMBOL: &str = "koja_int_parse";
pub(crate) const LAST_ERROR_SYMBOL: &str = "koja_last_error";
pub(crate) const MALLOC_SYMBOL: &str = "malloc";
pub(crate) const MEMSET_SYMBOL: &str = "memset";
pub(crate) const REALLOC_SYMBOL: &str = "realloc";
pub(crate) const UTF8_VALIDATE_SYMBOL: &str = "koja_utf8_validate";
pub(crate) const PACK_BITS_SYMBOL: &str = "__koja_pack_bits";
pub(crate) const PANIC_SYMBOL: &str = "__koja_panic";
pub(crate) const PRINT_STRING_SYMBOL: &str = "__koja_print_string";
pub(crate) const SOCKET_RECV_FROM_SYMBOL: &str = "koja_socket_recv_from";
pub(crate) const SOCKET_RESOLVE_SYMBOL: &str = "koja_socket_resolve";
pub(crate) const STRCMP_SYMBOL: &str = "strcmp";
pub(crate) const STRING_GET_SYMBOL: &str = "koja_string_get";
pub(crate) const STRING_LENGTH_SYMBOL: &str = "koja_string_length";
pub(crate) const STRING_SLICE_SYMBOL: &str = "koja_string_slice";

// `koja_rt_*` mailbox / scheduler symbols defined in
// `koja-runtime/src/scheduler.rs`. Backend-side declare helpers
// live below the existing `declare_*_extern` family.
pub(crate) const RT_BUILD_ARGV_SYMBOL: &str = "koja_rt_build_argv";
pub(crate) const RT_KILL_SYMBOL: &str = "koja_rt_kill";
pub(crate) const RT_MAIN_DONE_SYMBOL: &str = "koja_rt_main_done";
pub(crate) const RT_PROCESS_ALIVE_SYMBOL: &str = "koja_rt_is_process_alive";
pub(crate) const RT_RECEIVE_SYMBOL: &str = "koja_rt_receive";
pub(crate) const RT_RECEIVE_TIMEOUT_SYMBOL: &str = "koja_rt_receive_timeout";
pub(crate) const RT_SELF_SYMBOL: &str = "koja_rt_self";
pub(crate) const RT_SEND_AFTER_SYMBOL: &str = "koja_rt_send_after";
pub(crate) const RT_SEND_LIFECYCLE_SYMBOL: &str = "koja_rt_send_lifecycle";
pub(crate) const RT_SEND_SYMBOL: &str = "koja_rt_send";
pub(crate) const RT_SPAWN_SYMBOL: &str = "koja_rt_spawn";

/// Get the existing declaration for `symbol` or stamp a fresh
/// `void(arg_type)` external one. Idempotent so callers can declare
/// the same printer from multiple emit sites without duplicating.
/// Today's only caller is the `Global.print(s: String)` intrinsic
/// body in [`crate::intrinsics::print`].
pub(crate) fn declare_runtime_printer<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    let signature = ctx.context.void_type().fn_type(&[argument_type], false);
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

/// Declare (or look up) one of the `koja_format_*` runtime helpers
/// used by `Debug.format` primitive intrinsics. Signature:
/// `i8* koja_format_<ty>(<argument_type> value)`. Each helper
/// formats `value` into a freshly-allocated length-prefixed Koja
/// string and returns the payload pointer (8 bytes past the
/// `i64 bit_length` header).
pub(crate) fn declare_runtime_format<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[argument_type], false);
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `free` extern. The drop emitter
/// calls this once per heap-typed slot at function exit. Signature
/// is `void(i8*)`; the heap-block pointers are computed by
/// adjusting the SSA payload pointer (`payload - 8`) before the
/// call so `free` sees the allocator's block base.
pub(crate) fn declare_free_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(FREE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(FREE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_int_parse` runtime helper.
/// Signature: `i64 koja_int_parse(i8* input_payload, i64* out)`.
/// Parses the input as a base-10 i64, writes the result to `*out`,
/// and returns `1` on success / `0` on failure (leaving `*out`
/// untouched). The `Int.parse` intrinsic emitter allocates a
/// stack slot for `out`, branches on the return code, and wraps
/// the parsed value (or a literal `"invalid integer"`) into
/// `Result<Int, String>`.
pub(crate) fn declare_int_parse_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(INT_PARSE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(INT_PARSE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_float_parse` runtime helper.
/// Signature: `i64 koja_float_parse(i8* input_payload, f64* out)`.
/// Same return convention as [`declare_int_parse_extern`]; the
/// `Float.parse` intrinsic emitter follows the same skeleton.
pub(crate) fn declare_float_parse_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(FLOAT_PARSE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(FLOAT_PARSE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_last_error` runtime helper.
/// Signature: `i8* koja_last_error()`. Returns a freshly-allocated
/// Koja string payload (8 bytes past its `i64 bit_length` header)
/// describing the last I/O error set via `set_last_error` on the
/// calling thread; falls back to `"unknown error"` when no error
/// is set. The socket intrinsics (`Socket.recv_from`,
/// `Socket.resolve`) wrap this pointer directly into the
/// `Result.Err(String)` branch.
pub(crate) fn declare_last_error_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(LAST_ERROR_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[], false);
    ctx.module
        .add_function(LAST_ERROR_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `malloc` extern. The concat /
/// binary-construct emitters call this for the heap block base.
/// Signature: `i8* malloc(i64)` (the runtime targets 64-bit hosts; the
/// argument type matches `size_t` on those targets).
pub(crate) fn declare_malloc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(MALLOC_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[i64_ty.into()], false);
    ctx.module
        .add_function(MALLOC_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_utf8_validate` runtime helper.
/// Signature: `i64 koja_utf8_validate(i8* ptr, i64 len)`. Returns
/// `1` if `[ptr..ptr+len)` is valid UTF-8, `0` otherwise. Called
/// from `Binary.to_string` to gate the heap-clone path; the same
/// helper backs v1's `Binary_to_string` intrinsic.
pub(crate) fn declare_utf8_validate_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(UTF8_VALIDATE_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(UTF8_VALIDATE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `memset` extern. The hashtable
/// `new` emitter calls this to zero-clear the freshly-malloc'd
/// `states` buffer (so every slot starts as `EMPTY`). Signature:
/// `i8* memset(i8* dst, i32 value, i64 n)`.
pub(crate) fn declare_memset_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(MEMSET_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let i32_ty = ctx.context.i32_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i32_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(MEMSET_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_socket_recv_from` runtime
/// helper. Signature: `i8* koja_socket_recv_from(i32 fd, i64 count)`.
/// Suspends the calling process until the fd is readable, then
/// receives one datagram and returns a heap-allocated
/// `[*u8 data, *u8 ip_bin, i64 port]` triple (or null on error).
/// The `Socket.recv_from` intrinsic emitter marshals the triple
/// into a `Pair<String, SocketAddress>` SSA value.
pub(crate) fn declare_socket_recv_from_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(SOCKET_RECV_FROM_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[i32_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(SOCKET_RECV_FROM_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_socket_resolve` runtime helper.
/// Signature: `i8* koja_socket_resolve(i8* hostname_payload)`.
/// Wraps `getaddrinfo` and returns a heap-allocated
/// `[i64 count, *u8 ip0, *u8 ip1, ...]` buffer (or null on error).
/// The `Socket.resolve` intrinsic emitter copies the trailing
/// pointer array into a fresh `List<IPAddress>` element buffer.
pub(crate) fn declare_socket_resolve_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(SOCKET_RESOLVE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(SOCKET_RESOLVE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `realloc` extern. The list
/// `append` / `concat` emitters call this when the buffer needs to
/// grow. Signature: `i8* realloc(i8* ptr, i64 new_size)`.
pub(crate) fn declare_realloc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(REALLOC_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(REALLOC_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `__koja_concat_bits` runtime
/// helper. Signature: `i8* __koja_concat_bits(i8* lhs_payload,
/// i8* rhs_payload)`. Reads bit-lengths from each operand's `-8`
/// header, allocates a new `[i64 bit_length][ceil((L+R)/8) bytes]`
/// block, and bit-shifts rhs to land at the lhs trailing partial
/// byte. Sub-byte alignment is far cleaner in Rust than LLVM IR.
pub(crate) fn declare_concat_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(CONCAT_BITS_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(CONCAT_BITS_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `__koja_pack_bits` runtime helper.
/// Signature: `void __koja_pack_bits(i8* payload, i64 value,
/// i8 width, i64 bit_offset)`. Packs `width` bits of `value` into
/// `payload` MSB-first starting at `bit_offset`. The binary-literal
/// emitter calls this for sub-byte segment widths.
pub(crate) fn declare_pack_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(PACK_BITS_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let signature = ctx.context.void_type().fn_type(
        &[ptr_ty.into(), i64_ty.into(), i8_ty.into(), i64_ty.into()],
        false,
    );
    ctx.module
        .add_function(PACK_BITS_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `strcmp` extern.
/// Signature: `i32 strcmp(i8* lhs, i8* rhs)`. Used by `String.eq`
/// and the generic `==` lowering on `String` operands. Both
/// arguments point to NUL-terminated payload bytes.
pub(crate) fn declare_strcmp_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRCMP_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let signature = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(STRCMP_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_string_get` runtime helper.
/// Signature: `i8* koja_string_get(i8* payload, i64 index)`. Returns
/// a freshly-allocated payload for the codepoint at `index`, or
/// `null` when out-of-bounds — the `String.get` emitter branches
/// on the null to mint `None` vs `Some`.
pub(crate) fn declare_string_get_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRING_GET_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(STRING_GET_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_string_length` runtime helper.
/// Signature: `i64 koja_string_length(i8* payload)`. Walks the
/// payload as UTF-8 and returns the Unicode codepoint count.
pub(crate) fn declare_string_length_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRING_LENGTH_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(STRING_LENGTH_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `koja_string_slice` runtime helper.
/// Signature: `i8* koja_string_slice(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated payload covering the inclusive
/// codepoint range `[start, stop]`; out-of-bounds endpoints clamp to
/// the string boundaries.
pub(crate) fn declare_string_slice_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRING_SLICE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(STRING_SLICE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `__koja_panic` runtime helper.
/// Signature: `void __koja_panic(i8* message_payload)`. The
/// `Kernel.panic` intrinsic body calls this with the `String`
/// payload pointer (i.e. 8 bytes past the v1 length header) and
/// trails the call with `unreachable`. The runtime side prints
/// `panic: <message>` to stderr and aborts.
pub(crate) fn declare_panic_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(PANIC_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(PANIC_SYMBOL, signature, Some(Linkage::External))
}

// `koja_rt_*` mailbox / scheduler externs ----------------------------------

/// Declare (or look up) `koja_rt_spawn`. Signature:
/// `i64 koja_rt_spawn(void (*fn)(i8*), i8* state_ptr, i64 state_len)`.
/// Returns the new process's pid (1-indexed).
pub(crate) fn declare_rt_spawn_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_SPAWN_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(RT_SPAWN_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_receive`. Signature:
/// `i8* koja_rt_receive()`. Returns a tagged envelope buffer
/// (tag at offset 0; payload starts at offset 8). Blocks until a
/// message arrives.
pub(crate) fn declare_rt_receive_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_RECEIVE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[], false);
    ctx.module
        .add_function(RT_RECEIVE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_receive_timeout`. Signature:
/// `i8* koja_rt_receive_timeout(i64 timeout_ms)`. Like
/// [`declare_rt_receive_extern`] but returns null on timeout.
pub(crate) fn declare_rt_receive_timeout_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_RECEIVE_TIMEOUT_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[i64_ty.into()], false);
    ctx.module.add_function(
        RT_RECEIVE_TIMEOUT_SYMBOL,
        signature,
        Some(Linkage::External),
    )
}

/// Declare (or look up) `koja_rt_self`. Signature:
/// `i64 koja_rt_self()`. Returns the current process's pid.
pub(crate) fn declare_rt_self_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_SELF_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[], false);
    ctx.module
        .add_function(RT_SELF_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_send`. Signature:
/// `void koja_rt_send(i64 pid, i8* msg_ptr, i64 msg_len)`. Copies
/// `msg_len` bytes into the target's mailbox; the runtime tags the
/// payload with `tag=0` (business message) before delivery.
pub(crate) fn declare_rt_send_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_SEND_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ctx
        .context
        .void_type()
        .fn_type(&[i64_ty.into(), ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(RT_SEND_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_send_lifecycle`. Signature:
/// `void koja_rt_send_lifecycle(i64 pid, i64 variant)`. Variant
/// indices follow the `Lifecycle` enum: 0=Shutdown, 1=Interrupt,
/// 2=Reload. Inserted at the front of the mailbox for priority
/// delivery.
pub(crate) fn declare_rt_send_lifecycle_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_SEND_LIFECYCLE_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let signature = ctx
        .context
        .void_type()
        .fn_type(&[i64_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(RT_SEND_LIFECYCLE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_send_after`. Signature:
/// `void koja_rt_send_after(i64 pid, i8* msg_ptr, i64 msg_len, i64 delay_ms)`.
/// Copies the message immediately; delivery happens when the timer
/// fires.
pub(crate) fn declare_rt_send_after_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_SEND_AFTER_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(
        &[i64_ty.into(), ptr_ty.into(), i64_ty.into(), i64_ty.into()],
        false,
    );
    ctx.module
        .add_function(RT_SEND_AFTER_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_kill`. Signature:
/// `void koja_rt_kill(i64 pid)`. Marks the target process Dead
/// without giving it a chance to run cleanup.
pub(crate) fn declare_rt_kill_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_KILL_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(&[i64_ty.into()], false);
    ctx.module
        .add_function(RT_KILL_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_is_process_alive`. Signature:
/// `i64 koja_rt_is_process_alive(i64 pid)`. Returns 1 when the
/// target process is alive, 0 otherwise (including out-of-range
/// pids). The `Ref.alive?` emitter trims the result down to `i1`
/// before handing it back as a `Bool`.
pub(crate) fn declare_rt_is_process_alive_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_PROCESS_ALIVE_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[i64_ty.into()], false);
    ctx.module
        .add_function(RT_PROCESS_ALIVE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_main_done`. Signature:
/// `void koja_rt_main_done()`. Called by the auto-print wrapper
/// after `main` returns; boots the I/O reactor and worker pool,
/// then runs the scheduling loop until the main process (PID 1)
/// dies. Without this call, spawned processes never execute.
pub(crate) fn declare_rt_main_done_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_MAIN_DONE_SYMBOL) {
        return existing;
    }
    let signature = ctx.context.void_type().fn_type(&[], false);
    ctx.module
        .add_function(RT_MAIN_DONE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) `koja_rt_build_argv`. Signature:
/// `void koja_rt_build_argv(i32 argc, i8** argv, i8* out)`. Builds a
/// `List<String>` from C `argc`/`argv` (skipping `argv[0]`) and
/// writes it into `*out`. Used by the process-entry `main`
/// trampoline when the entry state's `Process<C, ..>` impl picks
/// `C = List<String>`.
pub(crate) fn declare_rt_build_argv_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(RT_BUILD_ARGV_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let signature = ctx
        .context
        .void_type()
        .fn_type(&[i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(RT_BUILD_ARGV_SYMBOL, signature, Some(Linkage::External))
}
