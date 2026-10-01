//! `Process.*` static `@intrinsic` emitters for monitoring and the
//! process tree. Each is a thin ABI adapter over one `koja_rt_*`
//! extern.

use inkwell::IntPredicate;
use inkwell::values::FunctionValue;
use koja_ir::{IRFunction, IRType, IRVariantPayload};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::{extract_int, nth_struct};
use crate::runtime::{
    declare_rt_demonitor_extern, declare_rt_monitor_extern, declare_rt_parent_extern,
};

/// `Process.demonitor(reference: Process.MonitorRef)`. Retract the
/// monitor via `koja_rt_demonitor`. `MonitorRef` lays out as
/// `{ i64 token }`.
pub(super) fn emit_demonitor<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let reference_value = nth_struct(function, llvm_function, 0, "reference");
    let token = extract_int(ctx, reference_value, 0, "monitor_token")?;
    ctx.call_rt_unit(declare_rt_demonitor_extern, &[token.into()])?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `Process.monitor(target: Pid) -> Process.MonitorRef`. Register the
/// calling process as a watcher of `target` via `koja_rt_monitor` and
/// wrap the returned token in the `MonitorRef` struct.
pub(super) fn emit_monitor<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    // `Pid` lays out as `{ i64 id }`.
    let target_value = nth_struct(function, llvm_function, 0, "target");
    let target_pid = extract_int(ctx, target_value, 0, "target_pid")?;
    let monitor_fn = declare_rt_monitor_extern(ctx);
    let token = ctx
        .call_basic(monitor_fn, &[target_pid.into()], "monitor_token")?
        .into_int_value();

    let ref_struct = match &function.return_type {
        IRType::Struct(symbol) => ctx.layouts.struct_type(symbol.mangled()),
        other => panic!(
            "LLVM emit: `Process.monitor` returns `{other:?}`, expected the \
             `Process.MonitorRef` struct (IR seal invariant violation)",
        ),
    };
    let monitor_ref = ctx
        .builder
        .build_insert_value(ref_struct.get_undef(), token, 0, "monitor_ref")
        .or_ice()?
        .into_struct_value();
    ctx.builder
        .build_return(Some(&monitor_ref))
        .or_ice()
        .map(|_| ())
}

/// `Process.parent() -> Option<Pid>`. Fetch the calling process's
/// parent PID via `koja_rt_parent` and wrap it as `Option.Some(Pid)`,
/// or `Option.None` when the runtime reports 0 (the entry process).
/// Both variants are built straight-line and a `select` picks one,
/// since neither construction has a side effect. Variant tags
/// resolve by name so `Option`'s declaration order is not baked into
/// codegen.
pub(super) fn emit_parent<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
) -> Result<(), LlvmError> {
    let IRType::Enum(option_symbol) = &function.return_type else {
        panic!(
            "LLVM emit: `Process.parent` returns `{:?}`, expected the \
             `Option<Pid>` enum (IR seal invariant violation)",
            function.return_type,
        );
    };
    let some_tag = ctx.layouts.enum_variant_tag(option_symbol, "Some");
    let none_tag = ctx.layouts.enum_variant_tag(option_symbol, "None");
    let pid_symbol = match ctx.layouts.enum_variant_payload(option_symbol, some_tag) {
        IRVariantPayload::Tuple(types) => match types.as_slice() {
            [IRType::Struct(symbol)] => symbol.clone(),
            other => panic!(
                "LLVM emit: `Process.parent` Some payload is `{other:?}`, expected \
                 a single `Pid` struct (IR seal invariant violation)",
            ),
        },
        other => panic!(
            "LLVM emit: `Process.parent` Some payload is `{other:?}`, expected \
             a tuple (IR seal invariant violation)",
        ),
    };

    let parent_fn = declare_rt_parent_extern(ctx);
    let parent_pid = ctx
        .call_basic(parent_fn, &[], "parent_pid")?
        .into_int_value();
    let has_parent = ctx
        .builder
        .build_int_compare(
            IntPredicate::NE,
            parent_pid,
            ctx.context.i64_type().const_zero(),
            "has_parent",
        )
        .or_ice()?;

    // `Pid` lays out as `{ i64 id }`.
    let pid_llvm = ctx.layouts.struct_type(pid_symbol.mangled());
    let pid_value = ctx
        .builder
        .build_insert_value(pid_llvm.get_undef(), parent_pid, 0, "parent_pid_struct")
        .or_ice()?
        .into_struct_value();
    let some_value = build_enum_value(ctx, option_symbol, some_tag, &[pid_value.into()])?;
    let none_value = build_enum_value(ctx, option_symbol, none_tag, &[])?;
    let option = ctx
        .builder
        .build_select(has_parent, some_value, none_value, "parent_option")
        .or_ice()?;
    ctx.builder.build_return(Some(&option)).or_ice().map(|_| ())
}
