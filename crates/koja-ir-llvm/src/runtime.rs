//! Runtime-symbol declarations for [`crate::intrinsics`] (which
//! calls them from compiler-synthesized `@intrinsic` bodies) and
//! [`crate::main_wrapper`]'s spawn / main-done hand-off.
//!
//! Each runtime helper lives in `koja-runtime-posix/src/intrinsics.rs`; this
//! module owns the LLVM-side declarations so the callers stamp
//! exactly one `module.get_function` lookup per symbol.

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, FunctionType};
use inkwell::values::{FunctionValue, GlobalValue};
use inkwell::{AddressSpace, ThreadLocalMode};

use crate::ctx::EmitContext;

pub(crate) const CLOSURE_DEEP_COPY_SYMBOL: &str = "koja_closure_deep_copy";
pub(crate) const CLOSURE_RC_DEC_SYMBOL: &str = "koja_closure_rc_dec";
pub(crate) const HEAP_DEEP_COPY_SYMBOL: &str = "koja_heap_deep_copy";
pub(crate) const CONCAT_BITS_SYMBOL: &str = "__koja_concat_bits";
pub(crate) const FORMAT_BOOL_SYMBOL: &str = "koja_format_bool";
pub(crate) const FORMAT_F32_SYMBOL: &str = "koja_format_f32";
pub(crate) const FORMAT_F64_SYMBOL: &str = "koja_format_f64";
pub(crate) const FORMAT_I64_SYMBOL: &str = "koja_format_i64";
pub(crate) const FORMAT_U64_SYMBOL: &str = "koja_format_u64";
pub(crate) const FLOAT_PARSE_SYMBOL: &str = "koja_float_parse";
pub(crate) const FREE_SYMBOL: &str = "koja_free";
pub(crate) const INT_PARSE_SYMBOL: &str = "koja_int_parse";
pub(crate) const LAST_ERROR_SYMBOL: &str = "koja_last_error";
pub(crate) const MALLOC_SYMBOL: &str = "koja_alloc";
pub(crate) const BINARY_SLICE_SYMBOL: &str = "koja_binary_slice";
pub(crate) const MEMSET_SYMBOL: &str = "memset";
pub(crate) const RC_DEC_SYMBOL: &str = "koja_rc_dec";
pub(crate) const RC_INC_SYMBOL: &str = "koja_rc_inc";
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
// `koja-runtime-posix/src/scheduler.rs`. Backend-side declare helpers
// live below the existing `declare_*_extern` family.
pub(crate) const RT_BUILD_ARGV_SYMBOL: &str = "koja_rt_build_argv";
pub(crate) const RT_CALL_RECEIVE_SYMBOL: &str = "koja_rt_call_receive";
pub(crate) const RT_CALL_TOKEN_SYMBOL: &str = "koja_rt_call_token";
pub(crate) const RT_KILL_SYMBOL: &str = "koja_rt_kill";
pub(crate) const RT_MAIN_DONE_SYMBOL: &str = "koja_rt_main_done";
pub(crate) const RT_PROCESS_ALIVE_SYMBOL: &str = "koja_rt_is_process_alive";
pub(crate) const RT_PROCESS_EXIT_SYMBOL: &str = "koja_rt_process_exit";
pub(crate) const RT_RECEIVE_SYMBOL: &str = "koja_rt_receive";
pub(crate) const RT_RECEIVE_TIMEOUT_SYMBOL: &str = "koja_rt_receive_timeout";
pub(crate) const RT_REPLY_SYMBOL: &str = "koja_rt_reply";
pub(crate) const RT_SELF_SYMBOL: &str = "koja_rt_self";
pub(crate) const RT_SEND_AFTER_SYMBOL: &str = "koja_rt_send_after";
pub(crate) const RT_SEND_LIFECYCLE_SYMBOL: &str = "koja_rt_send_lifecycle";
pub(crate) const RT_SET_PRIORITY_SYMBOL: &str = "koja_rt_set_priority";
pub(crate) const RT_SEND_SYMBOL: &str = "koja_rt_send";
pub(crate) const RT_SPAWN_SYMBOL: &str = "koja_rt_spawn";
pub(crate) const RT_YIELD_CHECK_SYMBOL: &str = "koja_rt_yield_check";
pub(crate) const RT_REDUCTIONS_COUNTER_SYMBOL: &str = "koja_reductions_left";

/// Get the existing declaration for `symbol`, or stamp a fresh external one
/// with `signature`. Idempotent, so emit sites can declare the same symbol
/// from multiple places without producing a duplicate.
fn declare_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    signature: FunctionType<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

/// Declare (or look up) a `void(argument_type)` extern. Today's only caller
/// is the `Global.print(s: String)` intrinsic body in
/// [`crate::intrinsics::print`].
pub(crate) fn declare_runtime_printer<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    let signature = ctx.context.void_type().fn_type(&[argument_type], false);
    declare_extern(ctx, symbol, signature)
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
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[argument_type], false);
    declare_extern(ctx, symbol, signature)
}

/// Declare (or look up) the `koja_free` extern — the runtime
/// allocator funnel's free (a sizeless libc-`free` passthrough; see
/// `koja-runtime-posix/src/mem.rs`). The drop emitter calls this once per
/// heap-typed slot at function exit. Signature is `void(i8*)`; the
/// heap-block pointers are computed by adjusting the SSA payload
/// pointer (`payload - 8`) before the call so the funnel sees the
/// allocator's block base.
pub(crate) fn declare_free_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, FREE_SYMBOL, signature)
}

/// Declare (or look up) the `koja_rc_inc` extern — the runtime's
/// refcount increment for an rc-managed leaf heap block. Signature:
/// `void koja_rc_inc(i8* base)`, where `base` is the block base (the
/// `i64 rc` word, `payload - HEADER_BYTES`). Immortal (rodata) blocks
/// carry a negative sentinel rc and are skipped by the runtime. The
/// `Clone` emitter calls this once per acquired heap-leaf value.
pub(crate) fn declare_rc_inc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, RC_INC_SYMBOL, signature)
}

/// Declare (or look up) the `koja_rc_dec` extern — the runtime's
/// refcount decrement for an rc-managed leaf heap block, freeing the
/// block when the count hits zero. Signature: `void koja_rc_dec(i8*
/// base)` (block base, as for [`declare_rc_inc_extern`]). The drop
/// emitter calls this once per heap-leaf slot at scope exit / per
/// discarded heap-leaf value.
pub(crate) fn declare_rc_dec_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, RC_DEC_SYMBOL, signature)
}

/// Declare (or look up) the `koja_closure_rc_dec` extern — the
/// runtime's refcount decrement for a closure env block. Signature:
/// `void koja_closure_rc_dec(i8* env)`, where `env` is the env block
/// base (the `i64 rc` word). At zero it runs the env header's
/// capture-release glue (if non-null) and frees the block; null /
/// immortal envs are no-ops. The closure `Drop` emitter calls this
/// once per closure-typed slot at scope exit / per discarded closure
/// value.
pub(crate) fn declare_closure_rc_dec_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, CLOSURE_RC_DEC_SYMBOL, signature)
}

/// Declare (or look up) the `koja_heap_deep_copy` extern — the
/// runtime's deep copy for an rc-managed leaf heap block. Signature:
/// `i8* koja_heap_deep_copy(i8* payload)`; returns a fresh payload
/// pointer with `rc = 1` and the bytes copied (immortal rodata
/// blocks are shared as-is, null returns null). The `DeepCopy`
/// emitter calls this once per heap-leaf value crossing a process
/// boundary.
pub(crate) fn declare_heap_deep_copy_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, HEAP_DEEP_COPY_SYMBOL, signature)
}

/// Declare (or look up) the `koja_closure_deep_copy` extern — the
/// runtime's deep copy for a closure env block. Signature:
/// `i8* koja_closure_deep_copy(i8* env)`; dispatches through the env
/// header's `copy_fn` glue and returns a fresh env base with `rc = 1`
/// and every heap-managed capture recursively copied (null envs
/// return null). The `DeepCopy` emitter calls this once per closure
/// value crossing a process boundary, then rebuilds the fat pointer
/// around the fresh env.
pub(crate) fn declare_closure_deep_copy_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, CLOSURE_DEEP_COPY_SYMBOL, signature)
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
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    declare_extern(ctx, INT_PARSE_SYMBOL, signature)
}

/// Declare (or look up) the `koja_float_parse` runtime helper.
/// Signature: `i64 koja_float_parse(i8* input_payload, f64* out)`.
/// Same return convention as [`declare_int_parse_extern`]; the
/// `Float.parse` intrinsic emitter follows the same skeleton.
pub(crate) fn declare_float_parse_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    declare_extern(ctx, FLOAT_PARSE_SYMBOL, signature)
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
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[], false);
    declare_extern(ctx, LAST_ERROR_SYMBOL, signature)
}

/// Declare (or look up) the `koja_alloc` extern — the runtime
/// allocator funnel's alloc (a libc-`malloc` passthrough that aborts
/// on OOM; see `koja-runtime-posix/src/mem.rs`). The concat /
/// binary-construct emitters call this for the heap block base.
/// Signature: `i8* koja_alloc(i64)` (the runtime targets 64-bit hosts;
/// the argument type matches `size_t` on those targets).
pub(crate) fn declare_malloc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[i64_ty.into()], false);
    declare_extern(ctx, MALLOC_SYMBOL, signature)
}

/// Declare (or look up) the `koja_utf8_validate` runtime helper.
/// Signature: `i64 koja_utf8_validate(i8* ptr, i64 len)`. Returns
/// `1` if `[ptr..ptr+len)` is valid UTF-8, `0` otherwise. Called
/// from `Binary.to_string` to gate the heap-clone path; the same
/// helper backs v1's `Binary_to_string` intrinsic.
pub(crate) fn declare_utf8_validate_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, UTF8_VALIDATE_SYMBOL, signature)
}

/// Declare (or look up) the libc `memset` extern. The hashtable
/// `new` emitter calls this to zero-clear the freshly-malloc'd
/// `states` buffer (so every slot starts as `EMPTY`). Signature:
/// `i8* memset(i8* dst, i32 value, i64 n)`.
pub(crate) fn declare_memset_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let i32_ty = ctx.context.i32_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i32_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, MEMSET_SYMBOL, signature)
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
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[i32_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, SOCKET_RECV_FROM_SYMBOL, signature)
}

/// Declare (or look up) the `koja_socket_resolve` runtime helper.
/// Signature: `i8* koja_socket_resolve(i8* hostname_payload)`.
/// Wraps `getaddrinfo` and returns a heap-allocated
/// `[i64 count, *u8 ip0, *u8 ip1, ...]` buffer (or null on error).
/// The `Socket.resolve` intrinsic emitter copies the trailing
/// pointer array into a fresh `List<IPAddress>` element buffer.
pub(crate) fn declare_socket_resolve_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, SOCKET_RESOLVE_SYMBOL, signature)
}

/// Declare (or look up) the `__koja_concat_bits` runtime
/// helper. Signature: `i8* __koja_concat_bits(i8* lhs_payload,
/// i8* rhs_payload)`. Reads bit-lengths from each operand's `-8`
/// header, allocates a new `[i64 bit_length][ceil((L+R)/8) bytes]`
/// block, and bit-shifts rhs to land at the lhs trailing partial
/// byte. Sub-byte alignment is far cleaner in Rust than LLVM IR.
pub(crate) fn declare_concat_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    declare_extern(ctx, CONCAT_BITS_SYMBOL, signature)
}

/// Declare (or look up) the `__koja_pack_bits` runtime helper.
/// Signature: `void __koja_pack_bits(i8* payload, i64 value,
/// i8 width, i64 bit_offset)`. Packs `width` bits of `value` into
/// `payload` MSB-first starting at `bit_offset`. The binary-literal
/// emitter calls this for sub-byte segment widths.
pub(crate) fn declare_pack_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let signature = ctx.context.void_type().fn_type(
        &[ptr_ty.into(), i64_ty.into(), i8_ty.into(), i64_ty.into()],
        false,
    );
    declare_extern(ctx, PACK_BITS_SYMBOL, signature)
}

/// Declare (or look up) the libc `strcmp` extern.
/// Signature: `i32 strcmp(i8* lhs, i8* rhs)`. Used by `String.eq`
/// and the generic `==` lowering on `String` operands. Both
/// arguments point to NUL-terminated payload bytes.
pub(crate) fn declare_strcmp_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let signature = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    declare_extern(ctx, STRCMP_SYMBOL, signature)
}

/// Declare (or look up) the `koja_string_get` runtime helper.
/// Signature: `i8* koja_string_get(i8* payload, i64 index)`. Returns
/// a freshly-allocated payload for the codepoint at `index`, or
/// `null` when out-of-bounds — the `String.get` emitter branches
/// on the null to mint `None` vs `Some`.
pub(crate) fn declare_string_get_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, STRING_GET_SYMBOL, signature)
}

/// Declare (or look up) the `koja_string_length` runtime helper.
/// Signature: `i64 koja_string_length(i8* payload)`. Walks the
/// payload as UTF-8 and returns the Unicode codepoint count.
pub(crate) fn declare_string_length_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, STRING_LENGTH_SYMBOL, signature)
}

/// Declare (or look up) the `koja_string_slice` runtime helper.
/// Signature: `i8* koja_string_slice(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated payload covering the inclusive
/// codepoint range `[start, stop]`; out-of-bounds endpoints clamp to
/// the string boundaries.
pub(crate) fn declare_string_slice_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, STRING_SLICE_SYMBOL, signature)
}

/// Declare (or look up) the `koja_binary_slice` runtime helper.
/// Signature: `i8* koja_binary_slice(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated `Binary` payload covering the inclusive
/// byte range `[start, stop]`; out-of-bounds endpoints clamp to the
/// binary boundaries.
pub(crate) fn declare_binary_slice_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, BINARY_SLICE_SYMBOL, signature)
}

/// Declare (or look up) the `__koja_panic` runtime helper.
/// Signature: `void __koja_panic(i8* message_payload)`. The
/// `Kernel.panic` intrinsic body calls this with the `String`
/// payload pointer (i.e. 8 bytes past the v1 length header) and
/// trails the call with `unreachable`. The runtime side prints
/// `panic: <message>` to stderr and aborts.
pub(crate) fn declare_panic_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    declare_extern(ctx, PANIC_SYMBOL, signature)
}

// `koja_rt_*` mailbox / scheduler externs ----------------------------------

/// Declare (or look up) `koja_rt_spawn`. Signature:
/// `i64 koja_rt_spawn(void (*fn)(i8*), i8* state_ptr, i64 state_len,
/// void(i8*)* drop_glue)`. Returns the new process's pid. The runtime
/// copies the config bytes and owns them: `drop_glue` (null when the
/// config owns no nested heap) runs over the copy when the process's
/// resources are reclaimed.
pub(crate) fn declare_rt_spawn_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(
        &[ptr_ty.into(), ptr_ty.into(), i64_ty.into(), ptr_ty.into()],
        false,
    );
    declare_extern(ctx, RT_SPAWN_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_receive`. Signature:
/// `i64 koja_rt_receive(i8* out, i64 out_cap)`. Copies the next
/// message's payload (header stripped) into the `out` slot, clamped to
/// `out_cap` bytes, frees the transport buffer, and returns the wire
/// tag; blocks until a message arrives. Returns `-1` on an empty wake.
pub(crate) fn declare_rt_receive_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, RT_RECEIVE_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_receive_timeout`. Signature:
/// `i64 koja_rt_receive_timeout(i8* out, i64 out_cap, i64 timeout_ms)`.
/// Like [`declare_rt_receive_extern`] but returns `-1` on timeout.
pub(crate) fn declare_rt_receive_timeout_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, RT_RECEIVE_TIMEOUT_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_self`. Signature:
/// `i64 koja_rt_self()`. Returns the current process's pid.
pub(crate) fn declare_rt_self_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[], false);
    declare_extern(ctx, RT_SELF_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_send`. Signature:
/// `void koja_rt_send(i64 pid, i8* msg_ptr, i64 msg_len, void(i8*)*
/// drop_glue)`. Copies `msg_len` bytes into the target's mailbox; the
/// runtime tags the payload with `tag=0` (business message) before
/// delivery. `drop_glue` (null when the payload owns no nested heap)
/// releases the payload's nested heap if the envelope is discarded
/// undelivered.
pub(crate) fn declare_rt_send_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(
        &[i64_ty.into(), ptr_ty.into(), i64_ty.into(), ptr_ty.into()],
        false,
    );
    declare_extern(ctx, RT_SEND_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_send_lifecycle`. Signature:
/// `void koja_rt_send_lifecycle(i64 pid, i64 variant)`. Variant
/// indices follow the `Lifecycle` enum: 0=Shutdown, 1=Interrupt,
/// 2=Reload. Routed to the target's system queue, which `receive`
/// drains before business traffic.
pub(crate) fn declare_rt_send_lifecycle_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = ctx
        .context
        .void_type()
        .fn_type(&[i64_ty.into(), i64_ty.into()], false);
    declare_extern(ctx, RT_SEND_LIFECYCLE_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_process_exit`. Signature:
/// `void koja_rt_process_exit(i64 reason)` — records the terminating
/// process's exit reason (0=Normal, 1=Shutdown, ...) on its control block.
/// Emitted in the process-body tail from the process's own `StopReason`.
pub(crate) fn declare_rt_process_exit_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(&[i64_ty.into()], false);
    declare_extern(ctx, RT_PROCESS_EXIT_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_set_priority`. Signature:
/// `void koja_rt_set_priority(i64 level)` — sets the current process's
/// scheduling weight (0=Low, 1=Normal, 2=High). Called once per process
/// body, right after `start` succeeds.
pub(crate) fn declare_rt_set_priority_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(&[i64_ty.into()], false);
    declare_extern(ctx, RT_SET_PRIORITY_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_yield_check`. Signature:
/// `void koja_rt_yield_check()` — the slow path of a cooperative preemption
/// point, called inline only once the reduction budget is exhausted, which
/// re-queues the process and switches back to its worker.
pub(crate) fn declare_rt_yield_check_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let signature = ctx.context.void_type().fn_type(&[], false);
    declare_extern(ctx, RT_YIELD_CHECK_SYMBOL, signature)
}

/// Declare (or look up) `koja_reductions_left`, the per-worker reduction
/// budget defined as a thread-local in `koja-runtime-posix/src/reductions.c`.
/// Compiled `YieldCheck`s decrement it inline; the runtime seeds it on each
/// resume. Initial-exec because it is resolved within the final executable.
pub(crate) fn reductions_counter_global<'ctx>(ctx: &EmitContext<'ctx>) -> GlobalValue<'ctx> {
    if let Some(existing) = ctx.module.get_global(RT_REDUCTIONS_COUNTER_SYMBOL) {
        return existing;
    }
    let global = ctx.module.add_global(
        ctx.context.i32_type(),
        Some(AddressSpace::default()),
        RT_REDUCTIONS_COUNTER_SYMBOL,
    );
    global.set_thread_local(true);
    global.set_thread_local_mode(Some(ThreadLocalMode::InitialExecTLSModel));
    global.set_linkage(Linkage::External);
    global
}

/// Declare (or look up) `koja_rt_send_after`. Signature:
/// `void koja_rt_send_after(i64 pid, i8* msg_ptr, i64 msg_len, i64
/// delay_ms, void(i8*)* drop_glue)`. Copies the message immediately;
/// delivery happens when the timer fires. `drop_glue` (null when the
/// payload owns no nested heap) rides the timer onto the fired
/// envelope so an undeliverable fire releases the nested heap.
pub(crate) fn declare_rt_send_after_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(
        &[
            i64_ty.into(),
            ptr_ty.into(),
            i64_ty.into(),
            i64_ty.into(),
            ptr_ty.into(),
        ],
        false,
    );
    declare_extern(ctx, RT_SEND_AFTER_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_reply`. Signature:
/// `void koja_rt_reply(i64 pid, i64 token, i8* msg_ptr, i64 msg_len,
/// void(i8*)* drop_glue)`. Like `koja_rt_send` but the envelope is
/// routed to the caller's one-shot reply slot, where
/// `koja_rt_call_receive` correlates it by `token`; it never enters
/// the receive queues.
pub(crate) fn declare_rt_reply_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(
        &[
            i64_ty.into(),
            i64_ty.into(),
            ptr_ty.into(),
            i64_ty.into(),
            ptr_ty.into(),
        ],
        false,
    );
    declare_extern(ctx, RT_REPLY_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_call_token`. Signature:
/// `i64 koja_rt_call_token()`. Mints a fresh correlation token for a
/// `Ref.call`; the caller stamps it into the outgoing `ReplyTo` and
/// waits for it via `koja_rt_call_receive`.
pub(crate) fn declare_rt_call_token_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[], false);
    declare_extern(ctx, RT_CALL_TOKEN_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_call_receive`. Signature:
/// `i64 koja_rt_call_receive(i64 token, i8* out, i64 out_cap, i64
/// timeout_ms)`. Blocks until the reply correlated with `token`
/// arrives, copies its payload into `out`, and returns `0`; returns
/// `-1` on timeout. Stale replies (token mismatch) are discarded by
/// the runtime.
pub(crate) fn declare_rt_call_receive_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(
        &[i64_ty.into(), ptr_ty.into(), i64_ty.into(), i64_ty.into()],
        false,
    );
    declare_extern(ctx, RT_CALL_RECEIVE_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_kill`. Signature:
/// `void koja_rt_kill(i64 pid)`. Marks the target process Dead
/// without giving it a chance to run cleanup.
pub(crate) fn declare_rt_kill_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = ctx.context.void_type().fn_type(&[i64_ty.into()], false);
    declare_extern(ctx, RT_KILL_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_is_process_alive`. Signature:
/// `i64 koja_rt_is_process_alive(i64 pid)`. Returns 1 when the
/// target process is alive, 0 otherwise (including out-of-range
/// pids). The `Ref.alive?` emitter trims the result down to `i1`
/// before handing it back as a `Bool`.
pub(crate) fn declare_rt_is_process_alive_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[i64_ty.into()], false);
    declare_extern(ctx, RT_PROCESS_ALIVE_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_main_done`. Signature:
/// `void koja_rt_main_done()`. Called by the auto-print wrapper
/// after `main` returns; boots the I/O reactor and worker pool,
/// then runs the scheduling loop until the main process (PID 1)
/// dies. Without this call, spawned processes never execute.
pub(crate) fn declare_rt_main_done_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let signature = ctx.context.void_type().fn_type(&[], false);
    declare_extern(ctx, RT_MAIN_DONE_SYMBOL, signature)
}

/// Declare (or look up) `koja_rt_build_argv`. Signature:
/// `void koja_rt_build_argv(i32 argc, i8** argv, i8* out)`. Builds a
/// `List<String>` from C `argc`/`argv` (skipping `argv[0]`) and
/// writes it into `*out`. Used by the process-entry `main`
/// trampoline when the entry state's `Process<C, ..>` impl picks
/// `C = List<String>`.
pub(crate) fn declare_rt_build_argv_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let signature = ctx
        .context
        .void_type()
        .fn_type(&[i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
    declare_extern(ctx, RT_BUILD_ARGV_SYMBOL, signature)
}
