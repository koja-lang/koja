//! FFI declarations for C stdlib, Expo runtime, and intrinsic functions.
//!
//! Extracted from `compiler.rs` to keep the `Compiler` impl focused on
//! orchestration rather than enumerating external symbols.

use std::collections::HashMap;

use expo_ir::identity::FunctionIdentifier;
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::module::Module as LlvmModule;
use inkwell::types::FunctionType;
use inkwell::values::FunctionValue;

/// Declares all external C and Expo runtime functions that codegen may call.
pub(crate) fn declare_builtins<'ctx>(
    context: &'ctx Context,
    module: &LlvmModule<'ctx>,
    functions: &mut HashMap<FunctionIdentifier, FunctionValue<'ctx>>,
) {
    let void = context.void_type();
    let i32 = context.i32_type();
    let i64 = context.i64_type();
    let ptr = context.ptr_type(AddressSpace::default());

    let mut decl = |name: &str, ty: FunctionType<'ctx>| {
        let f = module.add_function(name, ty, None);
        functions.insert(FunctionIdentifier::new(name), f);
    };

    // C stdlib
    decl("printf", i32.fn_type(&[ptr.into()], true));
    decl(
        "snprintf",
        i32.fn_type(&[ptr.into(), i32.into(), ptr.into()], true),
    );
    decl("fprintf", i32.fn_type(&[ptr.into(), ptr.into()], true));
    decl("abort", void.fn_type(&[], false));
    decl("fdopen", ptr.fn_type(&[i32.into(), ptr.into()], false));
    decl("malloc", ptr.fn_type(&[i64.into()], false));
    decl("realloc", ptr.fn_type(&[ptr.into(), i64.into()], false));
    decl("free", void.fn_type(&[ptr.into()], false));
    decl("strcmp", i32.fn_type(&[ptr.into(), ptr.into()], false));
    decl("strlen", i64.fn_type(&[ptr.into()], false));
    decl(
        "memset",
        ptr.fn_type(&[ptr.into(), i32.into(), i64.into()], false),
    );
    decl(
        "memcpy",
        ptr.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
    );
    decl(
        "memcmp",
        i32.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
    );

    // Process runtime
    decl(
        "expo_rt_spawn",
        i64.fn_type(&[ptr.into(), ptr.into(), i64.into()], false),
    );
    decl(
        "expo_rt_send",
        void.fn_type(&[i64.into(), ptr.into(), i64.into()], false),
    );
    decl("expo_rt_receive", ptr.fn_type(&[], false));
    decl("expo_rt_receive_timeout", ptr.fn_type(&[i64.into()], false));
    decl("expo_rt_self", i64.fn_type(&[], false));
    decl("expo_rt_main_done", void.fn_type(&[], false));
    decl(
        "expo_rt_send_lifecycle",
        void.fn_type(&[i64.into(), i64.into()], false),
    );
    decl(
        "expo_rt_is_process_alive",
        i64.fn_type(&[i64.into()], false),
    );
    decl("expo_rt_kill", void.fn_type(&[i64.into()], false));
    decl(
        "expo_rt_send_after",
        void.fn_type(&[i64.into(), ptr.into(), i64.into(), i64.into()], false),
    );
    decl(
        "expo_rt_watch_fd",
        void.fn_type(&[i32.into(), i64.into()], false),
    );
    decl("expo_rt_unwatch_fd", void.fn_type(&[i32.into()], false));

    // String intrinsics
    decl(
        "expo_utf8_validate",
        i64.fn_type(&[ptr.into(), i64.into()], false),
    );
    decl("expo_string_length", i64.fn_type(&[ptr.into()], false));
    decl(
        "expo_string_get",
        ptr.fn_type(&[ptr.into(), i64.into()], false),
    );
    decl(
        "expo_string_slice",
        ptr.fn_type(&[ptr.into(), i64.into(), i64.into()], false),
    );
    decl(
        "expo_int_parse",
        i64.fn_type(&[ptr.into(), ptr.into()], false),
    );
    decl(
        "expo_float_parse",
        i64.fn_type(&[ptr.into(), ptr.into()], false),
    );

    // I/O
    decl("expo_last_error", ptr.fn_type(&[], false));

    // System
    decl("expo_get_env", ptr.fn_type(&[ptr.into()], false));
    decl(
        "expo_set_env",
        void.fn_type(&[ptr.into(), ptr.into()], false),
    );
    decl("expo_cwd", ptr.fn_type(&[], false));
    decl("expo_hostname", ptr.fn_type(&[], false));

    // Debug formatting
    decl(
        "expo_format_binary",
        ptr.fn_type(&[ptr.into(), i64.into()], false),
    );

    // Time
    decl("expo_time_now_millis", i64.fn_type(&[], false));

    // Random
    decl(
        "expo_random_int",
        i64.fn_type(&[i64.into(), i64.into()], false),
    );

    // Socket I/O
    decl("expo_socket_resolve", ptr.fn_type(&[ptr.into()], false));
    decl(
        "expo_socket_recv_from",
        ptr.fn_type(&[i32.into(), i64.into()], false),
    );

    // Panic runtime
    decl("expo_panic_backtrace", void.fn_type(&[ptr.into()], false));
}
