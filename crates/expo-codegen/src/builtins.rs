//! FFI declarations for C stdlib, Expo runtime, and intrinsic functions.
//!
//! Extracted from `compiler.rs` to keep the `Compiler` impl focused on
//! orchestration rather than enumerating external symbols.

use expo_ir::identity::FunctionIdentifier;
use inkwell::AddressSpace;
use inkwell::types::FunctionType;

use crate::compiler::Compiler;

/// Declares all external C and Expo runtime functions that codegen may call.
/// Each declaration goes through [`Compiler::register_extern`] so the
/// callable-symbol registry on [`expo_ir::IRProgram`] sees these symbols
/// and call resolution can find them.
pub(crate) fn declare_builtins<'ctx>(c: &mut Compiler<'ctx>) {
    let void = c.context.void_type();
    let i32 = c.context.i32_type();
    let i64 = c.context.i64_type();
    let ptr = c.context.ptr_type(AddressSpace::default());

    // C stdlib
    decl(c, "printf", i32.fn_type(&[ptr.into()], true));
    decl(
        c,
        "snprintf",
        i32.fn_type(&[ptr.into(), i32.into(), ptr.into()], true),
    );
    decl(c, "fprintf", i32.fn_type(&[ptr.into(), ptr.into()], true));
    decl(c, "abort", void.fn_type(&[], false));
    decl(c, "fdopen", ptr.fn_type(&[i32.into(), ptr.into()], false));
    decl(c, "malloc", ptr.fn_type(&[i64.into()], false));
    decl(c, "realloc", ptr.fn_type(&[ptr.into(), i64.into()], false));
    decl(c, "free", void.fn_type(&[ptr.into()], false));
    decl(c, "strcmp", i32.fn_type(&[ptr.into(), ptr.into()], false));
    decl(c, "strlen", i64.fn_type(&[ptr.into()], false));
    decl(
        c,
        "memset",
        ptr.fn_type(&[ptr.into(), i32.into(), i64.into()], false),
    );
    decl(
        c,
        "memcpy",
        ptr.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
    );
    decl(
        c,
        "memcmp",
        i32.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
    );

    // Process runtime
    decl(
        c,
        "expo_rt_spawn",
        i64.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
    );
    decl(
        c,
        "expo_rt_send",
        void.fn_type(&[i64.into(), ptr.into(), i64.into()], false),
    );
    decl(c, "expo_rt_receive", ptr.fn_type(&[], false));
    decl(
        c,
        "expo_rt_receive_timeout",
        ptr.fn_type(&[i64.into()], false),
    );
    decl(c, "expo_rt_self", i64.fn_type(&[], false));
    decl(c, "expo_rt_main_done", void.fn_type(&[], false));
    decl(
        c,
        "expo_rt_send_lifecycle",
        void.fn_type(&[i64.into(), i64.into()], false),
    );
    decl(
        c,
        "expo_rt_is_process_alive",
        i64.fn_type(&[i64.into()], false),
    );
    decl(c, "expo_rt_kill", void.fn_type(&[i64.into()], false));
    decl(
        c,
        "expo_rt_send_after",
        void.fn_type(&[i64.into(), ptr.into(), i64.into(), i64.into()], false),
    );
    decl(
        c,
        "expo_rt_watch_fd",
        void.fn_type(&[i32.into(), i64.into()], false),
    );
    decl(c, "expo_rt_unwatch_fd", void.fn_type(&[i32.into()], false));

    // String intrinsics
    decl(
        c,
        "expo_utf8_validate",
        i64.fn_type(&[ptr.into(), i64.into()], false),
    );
    decl(c, "expo_string_length", i64.fn_type(&[ptr.into()], false));
    decl(
        c,
        "expo_string_get",
        ptr.fn_type(&[ptr.into(), i64.into()], false),
    );
    decl(
        c,
        "expo_string_slice",
        ptr.fn_type(&[ptr.into(), i64.into(), i64.into()], false),
    );
    decl(
        c,
        "expo_int_parse",
        i64.fn_type(&[ptr.into(), ptr.into()], false),
    );
    decl(
        c,
        "expo_float_parse",
        i64.fn_type(&[ptr.into(), ptr.into()], false),
    );

    // I/O
    decl(c, "expo_last_error", ptr.fn_type(&[], false));

    // System
    decl(c, "expo_get_env", ptr.fn_type(&[ptr.into()], false));
    decl(
        c,
        "expo_set_env",
        void.fn_type(&[ptr.into(), ptr.into()], false),
    );
    decl(c, "expo_cwd", ptr.fn_type(&[], false));
    decl(c, "expo_hostname", ptr.fn_type(&[], false));

    // Debug formatting
    decl(
        c,
        "expo_format_binary",
        ptr.fn_type(&[ptr.into(), i64.into()], false),
    );

    // Time
    decl(c, "expo_time_now_millis", i64.fn_type(&[], false));

    // Random
    decl(
        c,
        "expo_random_int",
        i64.fn_type(&[i64.into(), i64.into()], false),
    );

    // Socket I/O
    decl(c, "expo_socket_resolve", ptr.fn_type(&[ptr.into()], false));
    decl(
        c,
        "expo_socket_recv_from",
        ptr.fn_type(&[i32.into(), i64.into()], false),
    );

    // Panic runtime
    decl(
        c,
        "expo_panic_backtrace",
        void.fn_type(&[ptr.into()], false),
    );
}

/// Declares one external function and registers its signature-only
/// presence on [`expo_ir::IRProgram`] via [`Compiler::register_extern`].
fn decl<'ctx>(c: &mut Compiler<'ctx>, name: &str, ty: FunctionType<'ctx>) {
    let f = c.module.add_function(name, ty, None);
    c.register_extern(FunctionIdentifier::new(name), f);
}
