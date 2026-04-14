//! Codegen helpers for `spawn` expressions and the entry process wrapper.
//!
//! `spawn T.start(config)` creates a new process by:
//! 1. Serializing the config value into a stack-allocated byte buffer
//! 2. Building a wrapper function that loads config, calls `start`, and enters `run`
//! 3. Passing the wrapper + buffer to the runtime's `expo_rt_spawn`
//! 4. Wrapping the returned pid in a typed `Ref<M, R>` struct
//!
//! The entry process (`expo.toml` `entry = "App"`) reuses the same wrapper
//! infrastructure but extends it with exit-code tracking so `main()` can
//! return an OS-level exit code derived from `StopReason`.

use expo_ast::ast::{Arg, Expr, ExprKind};
use expo_typecheck::types::{
    Primitive, Type, build_substitution, mangle_name, named_generic, substitute,
};
use inkwell::IntPredicate;
use inkwell::types::{BasicType, BasicTypeEnum, IntType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, GlobalValue, IntValue};

use crate::compiler::{Compiler, TypedValue};
use crate::generics::{monomorphize_struct, try_parse_mangled_name};

// ---------------------------------------------------------------------------
// AST extraction
// ---------------------------------------------------------------------------

/// The parsed target of a `spawn T.start(config)` expression.
pub(crate) struct SpawnTarget<'a> {
    /// The process type name (e.g. `"Server"`, `"Task"`).
    pub type_name: String,
    /// The arguments passed to `start()` (exactly one config argument expected).
    pub config_args: &'a [Arg],
}

/// Extracts the type name and config arguments from a `spawn T.start(config)`
/// AST node.
///
/// Handles both `ExprKind::MethodCall` and `ExprKind::Call` forms produced by the
/// parser. Returns an error if the expression doesn't match the required
/// `Type.start(config)` shape.
pub(crate) fn extract_spawn_target(expr: &Expr) -> Result<SpawnTarget<'_>, String> {
    match &expr.kind {
        ExprKind::MethodCall { receiver, args, .. }
        | ExprKind::Call {
            callee: receiver,
            args,
            ..
        } => {
            let type_name = match &receiver.kind {
                ExprKind::Ident { name, .. } => name.clone(),
                ExprKind::FieldAccess { receiver: r, .. } => {
                    if let ExprKind::Ident { name, .. } = &r.kind {
                        name.clone()
                    } else {
                        return Err("spawn requires T.start(config) form".to_string());
                    }
                }
                _ => return Err("spawn requires T.start(config) form".to_string()),
            };
            Ok(SpawnTarget {
                type_name,
                config_args: args,
            })
        }
        _ => Err("spawn requires T.start(config) form".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Monomorphization helpers
// ---------------------------------------------------------------------------

/// Determines the concrete monomorphized type name for a process.
///
/// For generic processes like `Task<Int>`, the LLVM struct name of the
/// compiled config value (e.g. `Task_$Int$`) carries the concrete type
/// arguments. This function inspects that name and uses it when it matches
/// the expected base type, falling back to the bare `type_name` for
/// non-generic processes.
pub(crate) fn resolve_mangled_state(type_name: &str, config_value: BasicValueEnum) -> String {
    if config_value.is_struct_value() {
        let config_struct = config_value.into_struct_value().get_type();
        if let Some(name) = config_struct.get_name().and_then(|n| n.to_str().ok())
            && (name.starts_with(&format!("{type_name}_$")) || name == type_name)
        {
            return name.to_string();
        }
    }
    type_name.to_string()
}

/// Looks up the `Process<C, M, R>` protocol implementation for a type and
/// returns the concrete `(M, R)` message/reply types.
///
/// For generic processes the type parameters from the mangled name are
/// substituted into the protocol arguments. Non-generic processes use the
/// protocol args directly.
pub(crate) fn resolve_process_msg_reply(
    c: &Compiler,
    type_name: &str,
    mangled_state: &str,
) -> Result<(Type, Type), String> {
    if let Some((base, type_args)) = try_parse_mangled_name(mangled_state, c) {
        let impls = c
            .type_ctx
            .protocol_impls
            .get(&base)
            .ok_or_else(|| format!("`{base}` does not implement Process"))?;
        let (_, proto_args) = impls
            .iter()
            .find(|(proto, _)| proto == "Process")
            .ok_or_else(|| format!("`{base}` does not implement Process"))?;
        let ti = c
            .type_ctx
            .find_type(&base)
            .ok_or_else(|| format!("no type `{base}` for Process impl"))?;
        let subst = build_substitution(&ti.type_params, &type_args);
        let default = Type::Primitive(Primitive::String);
        let m = substitute(proto_args.get(1).unwrap_or(&default), &subst);
        let r = substitute(proto_args.get(2).unwrap_or(&default), &subst);
        Ok((m, r))
    } else {
        let process_args = c
            .type_ctx
            .protocol_impls
            .get(type_name)
            .and_then(|impls| {
                impls
                    .iter()
                    .find(|(proto, _)| proto == "Process")
                    .map(|(_, args)| args.clone())
            })
            .ok_or_else(|| format!("`{type_name}` does not implement Process"))?;
        let default = Type::Primitive(Primitive::String);
        let m = process_args.get(1).cloned().unwrap_or(default.clone());
        let r = process_args.get(2).cloned().unwrap_or(default);
        Ok((m, r))
    }
}

// ---------------------------------------------------------------------------
// Config serialization
// ---------------------------------------------------------------------------

/// A config value serialized into a stack-allocated byte buffer, ready for
/// cross-process transfer via `expo_rt_spawn`.
pub(crate) struct SerializedConfig<'ctx> {
    /// Opaque `i8*` pointer to the config bytes on the stack.
    pub ptr: BasicValueEnum<'ctx>,
    /// Byte size of the serialized config (as `i64`).
    pub size: IntValue<'ctx>,
    /// The original LLVM type, needed by the wrapper to load the config back.
    pub llvm_type: BasicTypeEnum<'ctx>,
}

/// Stack-allocates the config value, bitcasts the address to `i8*`, and
/// computes the byte size. The pointer and size are what `expo_rt_spawn`
/// expects after the wrapper function pointer.
pub(crate) fn serialize_config<'ctx>(
    c: &mut Compiler<'ctx>,
    config_value: BasicValueEnum<'ctx>,
) -> Result<SerializedConfig<'ctx>, String> {
    let config_type = config_value.get_type();
    let config_alloca = c.builder.build_alloca(config_type, "spawn_config").unwrap();
    c.builder.build_store(config_alloca, config_value).unwrap();

    let config_ptr = c
        .builder
        .build_bit_cast(
            config_alloca,
            c.context.ptr_type(inkwell::AddressSpace::default()),
            "config_ptr",
        )
        .unwrap();

    let config_size = config_type
        .size_of()
        .ok_or("could not compute config type size")?;
    let config_size_i64 = c
        .builder
        .build_int_cast(config_size, c.context.i64_type(), "config_size")
        .unwrap();

    Ok(SerializedConfig {
        ptr: config_ptr,
        size: config_size_i64,
        llvm_type: config_type,
    })
}

// ---------------------------------------------------------------------------
// Wrapper function builder
// ---------------------------------------------------------------------------

/// Optional exit-code tracking for the entry process wrapper.
///
/// When present, the wrapper converts `StopReason` values from both
/// `start()` (error path) and `run()` (ok path) into OS exit codes via
/// `ExitStatus.code()` and stores them in a global variable that `main()`
/// reads before returning.
pub(crate) struct ExitCodeCtx<'ctx> {
    pub exit_code_global: GlobalValue<'ctx>,
    pub code_fn: FunctionValue<'ctx>,
    pub stop_reason_llvm: StructType<'ctx>,
    pub i32_ty: inkwell::types::IntType<'ctx>,
}

/// Builds the child-side wrapper function `void __name(i8* config_ptr)`.
///
/// The wrapper:
/// 1. Loads config from the raw pointer provided by the runtime
/// 2. Calls `start(config)` → `Result<Self, StopReason>`
/// 3. On `Result.Ok`: extracts state, calls `run(state)`
/// 4. On `Result.Err`: process exits immediately
///
/// When `exit_ctx` is provided (entry process only), both paths convert
/// the resulting `StopReason` to an `i32` exit code and store it in a
/// global variable. Without `exit_ctx` (regular `spawn`), both paths
/// simply return void.
///
/// Saves and restores the builder insert position so the caller's IR
/// generation context is unaffected.
pub(crate) fn build_spawn_wrapper<'ctx>(
    c: &mut Compiler<'ctx>,
    wrapper_name: &str,
    config_llvm: BasicTypeEnum<'ctx>,
    state_type: StructType<'ctx>,
    start_fn: FunctionValue<'ctx>,
    run_fn: FunctionValue<'ctx>,
    exit_ctx: Option<&ExitCodeCtx<'ctx>>,
) -> Result<FunctionValue<'ctx>, String> {
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let i8_ty = c.context.i8_type();
    let wrapper_type = c.context.void_type().fn_type(&[ptr_ty.into()], false);
    let wrapper_fn = c.module.add_function(wrapper_name, wrapper_type, None);

    let entry_bb = c.context.append_basic_block(wrapper_fn, "entry");
    let ok_bb = c.context.append_basic_block(wrapper_fn, "start_ok");
    let err_bb = c.context.append_basic_block(wrapper_fn, "start_err");
    let done_bb = exit_ctx.map(|_| c.context.append_basic_block(wrapper_fn, "done"));

    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry_bb);

    let raw_ptr = wrapper_fn.get_nth_param(0).unwrap().into_pointer_value();
    let typed_ptr = c
        .builder
        .build_bit_cast(raw_ptr, ptr_ty, "typed_ptr")
        .unwrap()
        .into_pointer_value();
    let loaded_config = c
        .builder
        .build_load(config_llvm, typed_ptr, "loaded_config")
        .unwrap();

    let result_val = c
        .call(start_fn, &[loaded_config.into()], "start_result")
        .ok_or("start() did not produce a value")?;

    // Branch on the Result tag (field 0): Ok = 0, Err = 1.
    let result_struct_type = result_val.get_type().into_struct_type();
    let result_alloca = c
        .builder
        .build_alloca(result_struct_type, "result")
        .unwrap();
    c.builder.build_store(result_alloca, result_val).unwrap();

    let tag_ptr = c
        .builder
        .build_struct_gep(result_struct_type, result_alloca, 0, "tag_ptr")
        .unwrap();
    let tag = c
        .builder
        .build_load(i8_ty, tag_ptr, "tag")
        .unwrap()
        .into_int_value();
    let is_ok = c
        .builder
        .build_int_compare(IntPredicate::EQ, tag, i8_ty.const_int(0, false), "is_ok")
        .unwrap();
    c.builder
        .build_conditional_branch(is_ok, ok_bb, err_bb)
        .unwrap();

    // -- Ok path: extract state, call run --------------------------------
    c.builder.position_at_end(ok_bb);
    let ok_payload_ptr = c
        .builder
        .build_struct_gep(result_struct_type, result_alloca, 1, "ok_payload")
        .unwrap();
    let state_val = c
        .builder
        .build_load(state_type, ok_payload_ptr, "state")
        .unwrap();

    if let Some(ectx) = exit_ctx {
        let stop_reason = c
            .call(run_fn, &[state_val.into()], "stop_reason")
            .ok_or("run() did not produce a value")?;
        let exit_code = stop_reason_to_i32(c, stop_reason, ectx.code_fn, ectx.i32_ty, "exit_ok")?;
        c.builder
            .build_store(ectx.exit_code_global.as_pointer_value(), exit_code)
            .unwrap();
        c.builder
            .build_unconditional_branch(done_bb.unwrap())
            .unwrap();
    } else {
        c.call_void(run_fn, &[state_val.into()], "");
        c.builder.build_return(None).unwrap();
    }

    // -- Err path --------------------------------------------------------
    c.builder.position_at_end(err_bb);
    if let Some(ectx) = exit_ctx {
        let err_payload_ptr = c
            .builder
            .build_struct_gep(result_struct_type, result_alloca, 1, "err_payload")
            .unwrap();
        let err_reason = c
            .builder
            .build_load(ectx.stop_reason_llvm, err_payload_ptr, "err_reason")
            .unwrap();
        let exit_code = stop_reason_to_i32(c, err_reason, ectx.code_fn, ectx.i32_ty, "exit_err")?;
        c.builder
            .build_store(ectx.exit_code_global.as_pointer_value(), exit_code)
            .unwrap();
        c.builder
            .build_unconditional_branch(done_bb.unwrap())
            .unwrap();
    } else {
        c.builder.build_return(None).unwrap();
    }

    // -- Merge block (entry process only) --------------------------------
    if let Some(done) = done_bb {
        c.builder.position_at_end(done);
        c.builder.build_return(None).unwrap();
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }

    Ok(wrapper_fn)
}

// ---------------------------------------------------------------------------
// Ref<M, R> construction
// ---------------------------------------------------------------------------

/// Resolved `Ref<M, R>` type metadata.
struct ResolvedRefType {
    expo_type: Type,
    mangled_name: String,
    msg_type: Type,
    reply_type: Type,
}

/// Computes the mangled name and Expo type for a `Ref<M, R>` struct.
fn resolve_ref_type(compiler: &Compiler, msg_type: Type, reply_type: Type) -> ResolvedRefType {
    let type_args = vec![msg_type.clone(), reply_type.clone()];
    let mangled_name = mangle_name("Ref", &type_args);
    let expo_type = named_generic("Ref", type_args, compiler.type_ctx);
    ResolvedRefType {
        expo_type,
        mangled_name,
        msg_type,
        reply_type,
    }
}

/// Wraps a runtime pid in a `Ref<M, R>` struct value.
///
/// Monomorphizes the `Ref` struct type if it hasn't been instantiated for
/// this `(M, R)` pair yet, then inserts the pid into field 0.
pub(crate) fn build_ref_value<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pid: IntValue<'ctx>,
    msg_type: Type,
    reply_type: Type,
) -> Result<TypedValue<'ctx>, String> {
    let resolved = resolve_ref_type(compiler, msg_type, reply_type);

    if !compiler
        .types
        .contains_monomorphized(&resolved.mangled_name)
    {
        let type_args = vec![resolved.msg_type, resolved.reply_type];
        monomorphize_struct(compiler, "Ref", &type_args)?;
    }
    let ref_struct = compiler
        .types
        .get_monomorphized(&resolved.mangled_name)
        .ok_or("Ref struct type not found")?;

    let mut struct_value = ref_struct.get_undef();
    struct_value = compiler
        .builder
        .build_insert_value(struct_value, pid, 0, "wrap_pid")
        .unwrap()
        .into_struct_value();

    Ok(TypedValue::new(struct_value.into(), resolved.expo_type))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Converts a `StopReason` value to `i32` by calling `ExitStatus.code()` and
/// truncating from i64.
fn stop_reason_to_i32<'ctx>(
    compiler: &Compiler<'ctx>,
    stop_reason: BasicValueEnum<'ctx>,
    code_fn: FunctionValue<'ctx>,
    i32_type: IntType<'ctx>,
    name: &str,
) -> Result<IntValue<'ctx>, String> {
    let exit_code_i64 = compiler
        .call(code_fn, &[stop_reason.into()], &format!("{name}_i64"))
        .ok_or("StopReason_code did not produce a value")?;
    Ok(compiler
        .builder
        .build_int_truncate(exit_code_i64.into_int_value(), i32_type, name)
        .unwrap())
}
