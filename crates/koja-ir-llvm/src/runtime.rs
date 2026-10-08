//! Runtime-symbol declarations for [`crate::intrinsics`] (which
//! calls them from compiler-synthesized `@intrinsic` bodies) and
//! [`crate::main_wrapper`]'s spawn / main-done hand-off.
//!
//! The scalar helpers live in `koja-runtime-posix/src/intrinsics.rs`
//! and the `koja_rt_*` mailbox / scheduler symbols in
//! `koja-runtime-posix/src/scheduler.rs`. This module owns the
//! LLVM-side declarations so the callers stamp exactly one
//! `module.get_function` lookup per symbol. Every `declare_*_extern`
//! is one [`declare_with`] call that spells the signature in [`Ty`]
//! shapes.

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, FunctionType};
use inkwell::values::{FunctionValue, GlobalValue};
use inkwell::{AddressSpace, ThreadLocalMode};

use crate::ctx::EmitContext;

pub(crate) const BINARY_FIND_SYMBOL: &str = "koja_binary_find";
pub(crate) const BINARY_SLICE_SYMBOL: &str = "koja_binary_slice";
pub(crate) const CLOSURE_DEEP_COPY_SYMBOL: &str = "koja_closure_deep_copy";
pub(crate) const CLOSURE_RC_DEC_SYMBOL: &str = "koja_closure_rc_dec";
pub(crate) const CONCAT_BITS_SYMBOL: &str = "__koja_concat_bits";
pub(crate) const CONCAT_BYTES_OWNED_SYMBOL: &str = "__koja_concat_bytes_owned";
pub(crate) const FLOAT_PARSE_SYMBOL: &str = "koja_float_parse";
pub(crate) const FORMAT_BOOL_SYMBOL: &str = "koja_format_bool";
pub(crate) const FORMAT_F32_SYMBOL: &str = "koja_format_f32";
pub(crate) const FORMAT_F64_SYMBOL: &str = "koja_format_f64";
pub(crate) const FORMAT_I64_SYMBOL: &str = "koja_format_i64";
pub(crate) const FORMAT_U64_SYMBOL: &str = "koja_format_u64";
pub(crate) const FREE_SYMBOL: &str = "koja_free";
pub(crate) const HEAP_DEEP_COPY_SYMBOL: &str = "koja_heap_deep_copy";
pub(crate) const INT_PARSE_SYMBOL: &str = "koja_int_parse";
pub(crate) const LAST_ERROR_SYMBOL: &str = "koja_last_error";
pub(crate) const MALLOC_SYMBOL: &str = "koja_alloc";
pub(crate) const MEMCMP_SYMBOL: &str = "memcmp";
pub(crate) const MEMCPY_SYMBOL: &str = "memcpy";
pub(crate) const MEMSET_SYMBOL: &str = "memset";
pub(crate) const PACK_BITS_SYMBOL: &str = "__koja_pack_bits";
pub(crate) const PANIC_SYMBOL: &str = "__koja_panic";
pub(crate) const RC_DEC_SYMBOL: &str = "koja_rc_dec";
pub(crate) const RC_INC_SYMBOL: &str = "koja_rc_inc";
pub(crate) const RT_BUILD_ARGV_SYMBOL: &str = "koja_rt_build_argv";
pub(crate) const RT_CALL_RECEIVE_SYMBOL: &str = "koja_rt_call_receive";
pub(crate) const RT_CALL_TOKEN_SYMBOL: &str = "koja_rt_call_token";
pub(crate) const RT_CONTEXT_GET_SYMBOL: &str = "koja_rt_context_get";
pub(crate) const RT_DEMONITOR_SYMBOL: &str = "koja_rt_demonitor";
pub(crate) const RT_EXPORT_DROPPED_SYMBOL: &str = "koja_rt_export_dropped";
pub(crate) const RT_EXPORT_POP_SYMBOL: &str = "koja_rt_export_pop";
pub(crate) const RT_EXPORT_PUSH_SYMBOL: &str = "koja_rt_export_push";
pub(crate) const RT_KILL_SYMBOL: &str = "koja_rt_kill";
pub(crate) const RT_LOG_CONFIG_SYMBOL: &str = "koja_rt_log_config";
pub(crate) const RT_LOG_CONFIGURE_SYMBOL: &str = "koja_rt_log_configure";
pub(crate) const RT_LOG_ENTER_SYMBOL: &str = "koja_rt_log_enter";
pub(crate) const RT_LOG_LEAVE_SYMBOL: &str = "koja_rt_log_leave";
pub(crate) const RT_LOG_LEVEL_SYMBOL: &str = "koja_rt_log_level";
pub(crate) const RT_MAIN_DONE_SYMBOL: &str = "koja_rt_main_done";
pub(crate) const RT_MONITOR_SYMBOL: &str = "koja_rt_monitor";
pub(crate) const RT_PARENT_SYMBOL: &str = "koja_rt_parent";
pub(crate) const RT_PROCESS_ALIVE_SYMBOL: &str = "koja_rt_is_process_alive";
pub(crate) const RT_PROCESS_EXIT_SYMBOL: &str = "koja_rt_process_exit";
pub(crate) const RT_RECEIVE_SYMBOL: &str = "koja_rt_receive";
pub(crate) const RT_RECEIVE_TIMEOUT_SYMBOL: &str = "koja_rt_receive_timeout";
pub(crate) const RT_REDUCTIONS_COUNTER_SYMBOL: &str = "koja_reductions_left";
pub(crate) const RT_REDUCTIONS_GRANT_SYMBOL: &str = "koja_rt_reductions_grant";
pub(crate) const RT_REPLY_SYMBOL: &str = "koja_rt_reply";
pub(crate) const RT_SELF_SYMBOL: &str = "koja_rt_self";
pub(crate) const RT_SEND_AFTER_SYMBOL: &str = "koja_rt_send_after";
pub(crate) const RT_SEND_LIFECYCLE_SYMBOL: &str = "koja_rt_send_lifecycle";
pub(crate) const RT_SEND_SYMBOL: &str = "koja_rt_send";
pub(crate) const RT_SET_PRIORITY_SYMBOL: &str = "koja_rt_set_priority";
pub(crate) const RT_SPAN_CLOSE_SYMBOL: &str = "koja_rt_span_close";
pub(crate) const RT_SPAN_ID_SYMBOL: &str = "koja_rt_span_id";
pub(crate) const RT_SPAN_OPEN_SYMBOL: &str = "koja_rt_span_open";
pub(crate) const RT_SPAN_PUT_SYMBOL: &str = "koja_rt_span_put";
pub(crate) const RT_SPAN_TAKE_SYMBOL: &str = "koja_rt_span_take";
pub(crate) const RT_SPAWN_SYMBOL: &str = "koja_rt_spawn";
pub(crate) const RT_TRACE_INSTALL_SYMBOL: &str = "koja_rt_trace_install";
pub(crate) const RT_YIELD_CHECK_SYMBOL: &str = "koja_rt_yield_check";
pub(crate) const SOCKET_RECV_FROM_SYMBOL: &str = "koja_socket_recv_from";
pub(crate) const SOCKET_RESOLVE_SYMBOL: &str = "koja_socket_resolve";
pub(crate) const STRING_CONTAINS_NUL_SYMBOL: &str = "koja_string_contains_nul";
pub(crate) const STRING_EQ_SYMBOL: &str = "koja_string_eq";
pub(crate) const STRING_FIND_SYMBOL: &str = "koja_string_find";
pub(crate) const STRING_GET_SYMBOL: &str = "koja_string_get";
pub(crate) const STRING_LENGTH_SYMBOL: &str = "koja_string_length";
pub(crate) const STRING_NEXT_SYMBOL: &str = "koja_string_next";
pub(crate) const STRING_SLICE_BYTES_SYMBOL: &str = "koja_string_slice_bytes";
pub(crate) const STRING_SLICE_SYMBOL: &str = "koja_string_slice";
pub(crate) const UTF8_VALIDATE_SYMBOL: &str = "koja_utf8_validate";

/// Scalar shapes that runtime extern signatures use. `Void` is a
/// return shape only.
#[derive(Clone, Copy)]
enum Ty {
    I8,
    I32,
    I64,
    Ptr,
    Void,
}

impl Ty {
    /// Function type with `self` as the return shape.
    fn fn_type<'ctx>(
        self,
        ctx: &EmitContext<'ctx>,
        params: &[BasicMetadataTypeEnum<'ctx>],
    ) -> FunctionType<'ctx> {
        match self {
            Ty::I8 => ctx.context.i8_type().fn_type(params, false),
            Ty::I32 => ctx.context.i32_type().fn_type(params, false),
            Ty::I64 => ctx.context.i64_type().fn_type(params, false),
            Ty::Ptr => ctx
                .context
                .ptr_type(AddressSpace::default())
                .fn_type(params, false),
            Ty::Void => ctx.context.void_type().fn_type(params, false),
        }
    }

    /// Parameter type for `self`. `Void` is not a parameter shape.
    fn param_type<'ctx>(self, ctx: &EmitContext<'ctx>) -> BasicMetadataTypeEnum<'ctx> {
        match self {
            Ty::I8 => ctx.context.i8_type().into(),
            Ty::I32 => ctx.context.i32_type().into(),
            Ty::I64 => ctx.context.i64_type().into(),
            Ty::Ptr => ctx.context.ptr_type(AddressSpace::default()).into(),
            Ty::Void => panic!("LLVM emit: runtime extern declared a `void` parameter"),
        }
    }
}

/// Declare (or look up) the `koja_binary_find` runtime helper.
/// Same contract as [`declare_string_find_extern`] over a `Binary`
/// payload.
pub(crate) fn declare_binary_find_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        BINARY_FIND_SYMBOL,
        &[Ty::Ptr, Ty::Ptr, Ty::I64],
        Ty::I64,
    )
}

/// Declare (or look up) the `koja_binary_slice` runtime helper.
/// Signature: `i8* koja_binary_slice(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated `Binary` payload covering the inclusive
/// byte range `[start, stop]`. Out-of-bounds endpoints clamp to the
/// binary boundaries.
pub(crate) fn declare_binary_slice_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        BINARY_SLICE_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::I64],
        Ty::Ptr,
    )
}

/// Declare (or look up) the `koja_closure_deep_copy` extern.
/// Signature: `i8* koja_closure_deep_copy(i8* env)`. Dispatches
/// through the env header's `copy_fn` glue and returns a fresh env
/// base with `rc = 1` and every heap-managed capture copied. Null
/// envs return null.
pub(crate) fn declare_closure_deep_copy_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, CLOSURE_DEEP_COPY_SYMBOL, &[Ty::Ptr], Ty::Ptr)
}

/// Declare (or look up) the `koja_closure_rc_dec` extern. Signature:
/// `void koja_closure_rc_dec(i8* env)`, taking the env block base. At
/// zero it runs the env header's capture-release glue (if non-null)
/// and frees the block. Null and immortal envs are no-ops.
pub(crate) fn declare_closure_rc_dec_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, CLOSURE_RC_DEC_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) the `__koja_concat_bits` runtime helper.
/// Signature: `i8* __koja_concat_bits(i8* lhs_payload, i8*
/// rhs_payload)`. Allocates a fresh block and bit-shifts rhs to land
/// at the lhs trailing partial byte.
pub(crate) fn declare_concat_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, CONCAT_BITS_SYMBOL, &[Ty::Ptr, Ty::Ptr], Ty::Ptr)
}

/// Declare (or look up) the `__koja_concat_bytes_owned` runtime
/// helper. Signature: `i8* __koja_concat_bytes_owned(i8*
/// lhs_payload, i8* rhs_payload, i64 with_nul)`. The consuming
/// `String` / `Binary` concat grows lhs in place when its block is
/// uniquely owned, otherwise copies and releases lhs.
pub(crate) fn declare_concat_bytes_owned_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        CONCAT_BYTES_OWNED_SYMBOL,
        &[Ty::Ptr, Ty::Ptr, Ty::I64],
        Ty::Ptr,
    )
}

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

/// Declare (or look up) the `koja_float_parse` runtime helper.
/// Signature: `i64 koja_float_parse(i8* input_payload, f64* out)`.
/// Same return convention as [`declare_int_parse_extern`].
pub(crate) fn declare_float_parse_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, FLOAT_PARSE_SYMBOL, &[Ty::Ptr, Ty::Ptr], Ty::I64)
}

/// Declare (or look up) the `koja_free` extern, the runtime allocator
/// funnel's free. Signature: `void koja_free(i8* base)`, taking the
/// block base.
pub(crate) fn declare_free_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, FREE_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) the `koja_heap_deep_copy` extern. Signature:
/// `i8* koja_heap_deep_copy(i8* payload)`. Returns a fresh payload
/// with `rc = 1` and the bytes copied. Immortal blocks are shared
/// as-is and null returns null.
pub(crate) fn declare_heap_deep_copy_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, HEAP_DEEP_COPY_SYMBOL, &[Ty::Ptr], Ty::Ptr)
}

/// Declare (or look up) the `koja_int_parse` runtime helper.
/// Signature: `i64 koja_int_parse(i8* input_payload, i64* out)`.
/// Parses the input as a base-10 i64, writes the result to `*out`,
/// and returns `1` on success or `0` on failure (leaving `*out`
/// untouched).
pub(crate) fn declare_int_parse_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, INT_PARSE_SYMBOL, &[Ty::Ptr, Ty::Ptr], Ty::I64)
}

/// Declare (or look up) the `koja_last_error` runtime helper.
/// Signature: `i8* koja_last_error()`. Returns a fresh Koja string
/// describing the last I/O error set on the calling thread, or
/// `"unknown error"` when none is set.
pub(crate) fn declare_last_error_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, LAST_ERROR_SYMBOL, &[], Ty::Ptr)
}

/// Declare (or look up) the `koja_alloc` extern, the runtime
/// allocator funnel's alloc, which aborts on OOM. Signature:
/// `i8* koja_alloc(i64)`.
pub(crate) fn declare_malloc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, MALLOC_SYMBOL, &[Ty::I64], Ty::Ptr)
}

/// Declare (or look up) the libc `memcmp` extern. Signature:
/// `i32 memcmp(i8* lhs, i8* rhs, i64 n)`. Returns `0` when the byte
/// ranges match.
pub(crate) fn declare_memcmp_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, MEMCMP_SYMBOL, &[Ty::Ptr, Ty::Ptr, Ty::I64], Ty::I32)
}

/// Declare (or look up) the libc `memcpy` extern. Signature:
/// `i8* memcpy(i8* dst, i8* src, i64 n)`.
pub(crate) fn declare_memcpy_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, MEMCPY_SYMBOL, &[Ty::Ptr, Ty::Ptr, Ty::I64], Ty::Ptr)
}

/// Declare (or look up) the libc `memset` extern. Signature:
/// `i8* memset(i8* dst, i32 value, i64 n)`.
pub(crate) fn declare_memset_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, MEMSET_SYMBOL, &[Ty::Ptr, Ty::I32, Ty::I64], Ty::Ptr)
}

/// Declare (or look up) the `__koja_pack_bits` runtime helper.
/// Signature: `void __koja_pack_bits(i8* payload, i64 value,
/// i8 width, i64 bit_offset)`. Packs `width` bits of `value` into
/// `payload` MSB-first starting at `bit_offset`.
pub(crate) fn declare_pack_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        PACK_BITS_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::I8, Ty::I64],
        Ty::Void,
    )
}

/// Declare (or look up) the `__koja_panic` runtime helper.
/// Signature: `void __koja_panic(i8* message_payload)`. Prints
/// `panic: <message>` to stderr and aborts.
pub(crate) fn declare_panic_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, PANIC_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) the `koja_rc_dec` extern, which frees the
/// block when the count hits zero. Signature:
/// `void koja_rc_dec(i8* base)`, taking the block base.
pub(crate) fn declare_rc_dec_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RC_DEC_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) the `koja_rc_inc` extern. Signature:
/// `void koja_rc_inc(i8* base)`, taking the block base. Immortal
/// blocks are skipped by the runtime.
pub(crate) fn declare_rc_inc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RC_INC_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) `koja_rt_build_argv`. Signature:
/// `void koja_rt_build_argv(i32 argc, i8** argv, i8* out)`. Builds a
/// `List<String>` from C `argc`/`argv` (skipping `argv[0]`) and
/// writes it into `*out`.
pub(crate) fn declare_rt_build_argv_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_BUILD_ARGV_SYMBOL,
        &[Ty::I32, Ty::Ptr, Ty::Ptr],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_call_receive`. Signature:
/// `i64 koja_rt_call_receive(i64 token, i8* out, i64 out_cap, i64
/// timeout_ms, i64 target_pid)`. Blocks until the reply correlated with
/// `token` arrives, copies its payload into `out`, and returns `0`.
/// Returns `-1` on timeout, or as soon as `target_pid` (the callee) is
/// dead with no reply slotted. Stale replies (token mismatch) are
/// discarded by the runtime.
pub(crate) fn declare_rt_call_receive_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_CALL_RECEIVE_SYMBOL,
        &[Ty::I64, Ty::Ptr, Ty::I64, Ty::I64, Ty::I64],
        Ty::I64,
    )
}

/// Declare (or look up) `koja_rt_call_token`. Signature:
/// `i64 koja_rt_call_token()`. Mints a fresh correlation token for a
/// `Ref.call`. The caller stamps it into the outgoing `ReplyTo` and
/// waits for it via `koja_rt_call_receive`.
pub(crate) fn declare_rt_call_token_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_CALL_TOKEN_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_context_get`. Signature:
/// `void koja_rt_context_get(i8* out)`. Copies the calling process's
/// 32-byte `Process.Context` into `out`.
pub(crate) fn declare_rt_context_get_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_CONTEXT_GET_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) `koja_rt_demonitor`. Signature:
/// `void koja_rt_demonitor(i64 token)`. Removes the monitor minted
/// with that token.
pub(crate) fn declare_rt_demonitor_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_DEMONITOR_SYMBOL, &[Ty::I64], Ty::Void)
}

/// Declare (or look up) `koja_rt_export_dropped`. Signature:
/// `i64 koja_rt_export_dropped()`. Records the export queue has
/// dropped.
pub(crate) fn declare_rt_export_dropped_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_EXPORT_DROPPED_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_export_pop`. Signature:
/// `i64 koja_rt_export_pop(i8* out, i64 out_cap)`. Moves the oldest
/// queued record into `out` and returns 0, or returns -1 when empty.
pub(crate) fn declare_rt_export_pop_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_EXPORT_POP_SYMBOL, &[Ty::Ptr, Ty::I64], Ty::I64)
}

/// Declare (or look up) `koja_rt_export_push`. Signature:
/// `void koja_rt_export_push(i8* record, i64 len, void(i8*)* drop_glue)`.
/// Queues a copy of the finished record for the exporter.
pub(crate) fn declare_rt_export_push_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_EXPORT_PUSH_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_log_config`. Signature:
/// `i64 koja_rt_log_config(i8* out, i64 out_cap)`. Deep-copies the
/// calling process's stored log configuration into `out` and returns
/// 0, or returns -1 when none is stored.
pub(crate) fn declare_rt_log_config_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_LOG_CONFIG_SYMBOL, &[Ty::Ptr, Ty::I64], Ty::I64)
}

/// Declare (or look up) `koja_rt_log_configure`. Signature:
/// `void koja_rt_log_configure(i8* config, i64 len, void(i8*)* drop_glue,
/// void(i8*)* copy_glue, i64 level)`. Stores a copy of the
/// configuration and the floor word on the calling process.
pub(crate) fn declare_rt_log_configure_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_LOG_CONFIGURE_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::Ptr, Ty::Ptr, Ty::I64],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_log_enter`. Signature:
/// `i64 koja_rt_log_enter()`. Marks the calling process as running
/// its log handlers and returns 1, or returns 0 when it already was.
pub(crate) fn declare_rt_log_enter_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_LOG_ENTER_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_log_leave`. Signature:
/// `void koja_rt_log_leave()`. Clears the mark `koja_rt_log_enter` set.
pub(crate) fn declare_rt_log_leave_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_LOG_LEAVE_SYMBOL, &[], Ty::Void)
}

/// Declare (or look up) `koja_rt_log_level`. Signature:
/// `i64 koja_rt_log_level()`. The calling process's log floor word.
pub(crate) fn declare_rt_log_level_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_LOG_LEVEL_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_is_process_alive`. Signature:
/// `i64 koja_rt_is_process_alive(i64 pid)`. Returns 1 when the
/// target process is alive, 0 otherwise (including out-of-range
/// pids).
pub(crate) fn declare_rt_is_process_alive_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_PROCESS_ALIVE_SYMBOL, &[Ty::I64], Ty::I64)
}

/// Declare (or look up) `koja_rt_kill`. Signature:
/// `void koja_rt_kill(i64 pid)`. Marks the target process Dead
/// without giving it a chance to run cleanup.
pub(crate) fn declare_rt_kill_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_KILL_SYMBOL, &[Ty::I64], Ty::Void)
}

/// Declare (or look up) `koja_rt_main_done`. Signature:
/// `void koja_rt_main_done()`. Boots the I/O reactor and worker pool,
/// then runs the scheduling loop until PID 1 dies.
pub(crate) fn declare_rt_main_done_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_MAIN_DONE_SYMBOL, &[], Ty::Void)
}

/// Declare (or look up) `koja_rt_monitor`. Signature:
/// `i64 koja_rt_monitor(i64 target_pid)`. Registers the calling
/// process as a monitor of the target and returns the monitor token.
pub(crate) fn declare_rt_monitor_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_MONITOR_SYMBOL, &[Ty::I64], Ty::I64)
}

/// Declare (or look up) `koja_rt_parent`. Signature:
/// `i64 koja_rt_parent()`. Returns the calling process's parent PID,
/// or 0 for the entry process.
pub(crate) fn declare_rt_parent_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_PARENT_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_process_exit`. Signature:
/// `void koja_rt_process_exit(i64 reason)`. Records the terminating
/// process's exit reason (0=Normal, 1=Shutdown, ...) on its control block.
pub(crate) fn declare_rt_process_exit_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_PROCESS_EXIT_SYMBOL, &[Ty::I64], Ty::Void)
}

/// Declare (or look up) `koja_rt_receive`. Signature:
/// `i64 koja_rt_receive(i8* out, i64 out_cap)`. Copies the next
/// message's payload (header stripped) into the `out` slot, clamped to
/// `out_cap` bytes, frees the transport buffer, and returns the wire
/// tag. Blocks until a message arrives. Returns `-1` on an empty wake.
pub(crate) fn declare_rt_receive_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_RECEIVE_SYMBOL, &[Ty::Ptr, Ty::I64], Ty::I64)
}

/// Declare (or look up) `koja_rt_receive_timeout`. Signature:
/// `i64 koja_rt_receive_timeout(i8* out, i64 out_cap, i64 timeout_ms)`.
/// Like [`declare_rt_receive_extern`] but returns `-1` on timeout.
pub(crate) fn declare_rt_receive_timeout_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_RECEIVE_TIMEOUT_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::I64],
        Ty::I64,
    )
}

/// Declare (or look up) `u32 koja_rt_reductions_grant()`, the running
/// process's reduction grant for the current quantum.
pub(crate) fn declare_rt_reductions_grant_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_REDUCTIONS_GRANT_SYMBOL, &[], Ty::I32)
}

/// Declare (or look up) `koja_rt_reply`. Signature:
/// `i64 koja_rt_reply(i64 pid, i64 token, i8* msg_ptr, i64 msg_len,
/// void(i8*)* drop_glue)`. Like `koja_rt_send` but the envelope is
/// routed to the caller's one-shot reply slot, where
/// `koja_rt_call_receive` correlates it by `token`. It never enters
/// the receive queues. Returns `0` when the reply was slotted for a
/// still-waiting caller and `1` when the caller had already given up.
pub(crate) fn declare_rt_reply_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_REPLY_SYMBOL,
        &[Ty::I64, Ty::I64, Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::I64,
    )
}

/// Declare (or look up) `koja_rt_self`. Signature:
/// `i64 koja_rt_self()`. Returns the current process's pid.
pub(crate) fn declare_rt_self_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_SELF_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_send_after`. Signature:
/// `void koja_rt_send_after(i64 pid, i8* msg_ptr, i64 msg_len, i64
/// delay_ms, void(i8*)* drop_glue)`. Copies the message immediately.
/// Delivery happens when the timer fires. `drop_glue` (null when the
/// payload owns no nested heap) rides the timer onto the fired
/// envelope so an undeliverable fire releases the nested heap.
pub(crate) fn declare_rt_send_after_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SEND_AFTER_SYMBOL,
        &[Ty::I64, Ty::Ptr, Ty::I64, Ty::I64, Ty::Ptr],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_send`. Signature:
/// `void koja_rt_send(i64 pid, i8* msg_ptr, i64 msg_len, void(i8*)*
/// drop_glue)`. Copies `msg_len` bytes into the target's mailbox. The
/// runtime tags the payload with `tag=0` (business message) before
/// delivery. `drop_glue` (null when the payload owns no nested heap)
/// releases the payload's nested heap if the envelope is discarded
/// undelivered.
pub(crate) fn declare_rt_send_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SEND_SYMBOL,
        &[Ty::I64, Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_send_lifecycle`. Signature:
/// `void koja_rt_send_lifecycle(i64 pid, i64 variant)`. Variant
/// indices follow the `Lifecycle` enum: 0=Shutdown, 1=Interrupt,
/// 2=Reload. Routed to the target's system queue, which `receive`
/// drains before business traffic.
pub(crate) fn declare_rt_send_lifecycle_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_SEND_LIFECYCLE_SYMBOL, &[Ty::I64, Ty::I64], Ty::Void)
}

/// Declare (or look up) `koja_rt_set_priority`. Signature:
/// `void koja_rt_set_priority(i64 level)`. Sets the current process's
/// scheduling weight (0=Low, 1=Normal, 2=High).
pub(crate) fn declare_rt_set_priority_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_SET_PRIORITY_SYMBOL, &[Ty::I64], Ty::Void)
}

/// Declare (or look up) `koja_rt_span_close`. Signature:
/// `void koja_rt_span_close(i64 handle, i8* out, i64 out_cap)`. Pops
/// the innermost open record, which must be `handle`, into `out`.
pub(crate) fn declare_rt_span_close_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SPAN_CLOSE_SYMBOL,
        &[Ty::I64, Ty::Ptr, Ty::I64],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_span_id`. Signature:
/// `i64 koja_rt_span_id()`. A fresh non-zero span id.
pub(crate) fn declare_rt_span_id_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_SPAN_ID_SYMBOL, &[], Ty::I64)
}

/// Declare (or look up) `koja_rt_span_open`. Signature:
/// `i64 koja_rt_span_open(i8* record, i64 len, void(i8*)* drop_glue)`.
/// Copies the record onto the calling process's open span stack and
/// returns its handle.
pub(crate) fn declare_rt_span_open_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SPAN_OPEN_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::I64,
    )
}

/// Declare (or look up) `koja_rt_span_put`. Signature:
/// `void koja_rt_span_put(i64 handle, i8* record, i64 len, void(i8*)* drop_glue)`.
/// Copies the record back into the empty slot at `handle`.
pub(crate) fn declare_rt_span_put_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SPAN_PUT_SYMBOL,
        &[Ty::I64, Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_span_take`. Signature:
/// `void koja_rt_span_take(i64 handle, i8* out, i64 out_cap)`. Moves
/// the open record at `handle` into `out`.
pub(crate) fn declare_rt_span_take_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SPAN_TAKE_SYMBOL,
        &[Ty::I64, Ty::Ptr, Ty::I64],
        Ty::Void,
    )
}

/// Declare (or look up) `koja_rt_spawn`. Signature:
/// `i64 koja_rt_spawn(void (*fn)(i8*), i8* state_ptr, i64 state_len,
/// void(i8*)* drop_glue)`. Returns the new process's pid. The runtime
/// copies the config bytes and owns them: `drop_glue` (null when the
/// config owns no nested heap) runs over the copy when the process's
/// resources are reclaimed.
pub(crate) fn declare_rt_spawn_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        RT_SPAWN_SYMBOL,
        &[Ty::Ptr, Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::I64,
    )
}

/// Declare (or look up) `koja_rt_trace_install`. Signature:
/// `void koja_rt_trace_install(i8* context)`. Installs the 32-byte
/// `Process.Context` at `context` on the calling process.
pub(crate) fn declare_rt_trace_install_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_TRACE_INSTALL_SYMBOL, &[Ty::Ptr], Ty::Void)
}

/// Declare (or look up) `koja_rt_yield_check`. Signature:
/// `u32 koja_rt_yield_check()`: the slow path of a cooperative preemption
/// point, called inline only once the reduction budget is exhausted, which
/// re-queues the process and switches back to its worker. Returns the
/// next quantum's grant so the register strategy can reseed after the
/// process resumes (the thread-local strategy ignores it).
pub(crate) fn declare_rt_yield_check_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, RT_YIELD_CHECK_SYMBOL, &[], Ty::I32)
}

/// Declare (or look up) one of the `koja_format_*` runtime helpers.
/// Signature: `i8* koja_format_<ty>(<argument_type> value)`. Each
/// formats `value` into a fresh Koja string and returns the payload
/// pointer. The argument shape comes from the caller because the
/// float helpers take `f32` / `f64`, which [`Ty`] does not spell.
pub(crate) fn declare_runtime_format<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    declare_extern(ctx, symbol, Ty::Ptr.fn_type(ctx, &[argument_type]))
}

/// Declare (or look up) the `koja_socket_recv_from` runtime
/// helper. Signature: `i8* koja_socket_recv_from(i32 fd, i64 count)`.
/// Suspends the calling process until the fd is readable, then
/// receives one datagram and returns a heap-allocated
/// `[*u8 data, *u8 ip_bin, i64 port]` triple (or null on error).
pub(crate) fn declare_socket_recv_from_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, SOCKET_RECV_FROM_SYMBOL, &[Ty::I32, Ty::I64], Ty::Ptr)
}

/// Declare (or look up) the `koja_socket_resolve` runtime helper.
/// Signature: `i8* koja_socket_resolve(i8* hostname_payload)`.
/// Wraps `getaddrinfo` and returns a heap-allocated
/// `[i64 count, *u8 ip0, *u8 ip1, ...]` buffer (or null on error).
pub(crate) fn declare_socket_resolve_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, SOCKET_RESOLVE_SYMBOL, &[Ty::Ptr], Ty::Ptr)
}

/// Declare the Koja string interior-NUL helper.
/// Signature: `i64 koja_string_contains_nul(i8* payload)`.
pub(crate) fn declare_string_contains_nul_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(ctx, STRING_CONTAINS_NUL_SYMBOL, &[Ty::Ptr], Ty::I64)
}

/// Declare the length-aware Koja string equality helper.
/// Signature: `i64 koja_string_eq(i8* lhs, i8* rhs)`.
pub(crate) fn declare_string_eq_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, STRING_EQ_SYMBOL, &[Ty::Ptr, Ty::Ptr], Ty::I64)
}

/// Declare (or look up) the `koja_string_find` runtime helper.
/// Signature: `i64 koja_string_find(i8* payload, i8* needle, i64 from)`.
/// Returns the byte offset of the first occurrence of `needle` at or
/// after byte offset `from`, or -1 when absent.
pub(crate) fn declare_string_find_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        STRING_FIND_SYMBOL,
        &[Ty::Ptr, Ty::Ptr, Ty::I64],
        Ty::I64,
    )
}

/// Declare (or look up) the `koja_string_get` runtime helper.
/// Signature: `i8* koja_string_get(i8* payload, i64 index)`. Returns
/// a freshly-allocated payload for the codepoint at `index`, or
/// `null` when out-of-bounds.
pub(crate) fn declare_string_get_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, STRING_GET_SYMBOL, &[Ty::Ptr, Ty::I64], Ty::Ptr)
}

/// Declare (or look up) the `koja_string_length` runtime helper.
/// Signature: `i64 koja_string_length(i8* payload)`. Walks the
/// payload as UTF-8 and returns the Unicode codepoint count.
pub(crate) fn declare_string_length_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, STRING_LENGTH_SYMBOL, &[Ty::Ptr], Ty::I64)
}

/// Declare (or look up) the `koja_string_next` runtime helper.
/// Signature: `i8* koja_string_next(i8* payload, i64 cursor, i64* next)`.
/// Returns a fresh one-character string, or null for an invalid cursor.
pub(crate) fn declare_string_next_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        STRING_NEXT_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::Ptr],
        Ty::Ptr,
    )
}

/// Declare (or look up) the `koja_string_slice_bytes` runtime helper.
/// Signature: `i8* koja_string_slice_bytes(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated `String` payload covering the byte
/// range `[start, stop)`. Endpoints clamp to the string's byte length
/// and must land on codepoint boundaries.
pub(crate) fn declare_string_slice_bytes_extern<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        STRING_SLICE_BYTES_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::I64],
        Ty::Ptr,
    )
}

/// Declare (or look up) the `koja_string_slice` runtime helper.
/// Signature: `i8* koja_string_slice(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated payload covering the inclusive
/// codepoint range `[start, stop]`. Out-of-bounds endpoints clamp to
/// the string boundaries.
pub(crate) fn declare_string_slice_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(
        ctx,
        STRING_SLICE_SYMBOL,
        &[Ty::Ptr, Ty::I64, Ty::I64],
        Ty::Ptr,
    )
}

/// Declare (or look up) the `koja_utf8_validate` runtime helper.
/// Signature: `i64 koja_utf8_validate(i8* ptr, i64 len)`. Returns
/// `1` if `[ptr..ptr+len)` is valid UTF-8, `0` otherwise.
pub(crate) fn declare_utf8_validate_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    declare_with(ctx, UTF8_VALIDATE_SYMBOL, &[Ty::Ptr, Ty::I64], Ty::I64)
}

/// Declare (or look up) `symbol` with a signature spelled in [`Ty`]
/// shapes. Every `declare_*_extern` in this module is one call to it.
fn declare_with<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    params: &[Ty],
    ret: Ty,
) -> FunctionValue<'ctx> {
    let params: Vec<BasicMetadataTypeEnum<'ctx>> =
        params.iter().map(|ty| ty.param_type(ctx)).collect();
    declare_extern(ctx, symbol, ret.fn_type(ctx, &params))
}

/// Declare (or look up) `koja_reductions_left`, the per-worker reduction
/// budget defined as a thread-local in `koja-runtime-posix/src/reductions.c`.
/// x86_64 `YieldCheck`s decrement it inline and the runtime seeds it on
/// each resume (aarch64 keeps the budget in a reserved register, see
/// [`crate::reductions`]). Initial-exec because it is resolved within
/// the final executable.
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
