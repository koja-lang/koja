//! Closure-shaped instruction emission: `MakeClosure`, `CallClosure`,
//! `LoadCapture`, plus the `DropLocal` helper for `IRType::Function`
//! slots. Mirrors the IR vocabulary the [`crate::emit::instruction`]
//! dispatcher routes to.
//!
//! Closure values are `{fn_ptr, env_ptr}` fat pointers (see
//! [`crate::types::closure_fat_ptr_type`]); closure-kind bodies
//! declare an extra `env_ptr` parameter at LLVM position 0 (see
//! [`crate::function::declare_function`]). Active closure bodies
//! stash their env pointer + env-struct type on
//! [`crate::ctx::EmitContext`] so `LoadCapture` can GEP into the
//! right slot at body-emit time.

use inkwell::AddressSpace;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};
use koja_ir::mangling::closure_drop_env_symbol;
use koja_ir::{IRLocalId, IRSymbol, IRType, ValueId};

use crate::ctx::{ClosureFrame, EmitContext};
use crate::error::LlvmError;
use crate::runtime::{declare_closure_rc_dec_extern, declare_malloc_extern};
use crate::types::{
    CLOSURE_ENV_HEADER_FIELDS, closure_body_signature, closure_fat_ptr_type, ir_basic_type,
};

use super::{ValueMap, inkwell_err, lookup};

/// Materialize the closure value: malloc the env block (skipped for
/// captureless adapters where the env is never read), store each
/// capture by index, then build the `{fn_ptr, env_ptr}` fat
/// pointer. The fn_ptr resolves through the declared-functions
/// index so the caller's [`crate::program::compile_program`]
/// declare-then-define ordering keeps the lookup populated.
pub(super) fn emit_make_closure<'ctx>(
    ctx: &EmitContext<'ctx>,
    body: &IRSymbol,
    captures: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let mut capture_values: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(captures.len());
    for capture in captures {
        capture_values.push(lookup(values, *capture)?);
    }
    let body_fn = ctx.declared_function(body).unwrap_or_else(|| {
        panic!(
            "LLVM emit: closure body `{}` not registered in declared-functions \
             index — declaration order or seal violation",
            body.mangled(),
        )
    });
    let fn_ptr = body_fn.as_global_value().as_pointer_value();
    let env_ptr = if capture_values.is_empty() {
        ctx.context.ptr_type(AddressSpace::default()).const_null()
    } else {
        let drop_fn = closure_drop_env_ptr(ctx, body);
        emit_env_alloc_and_store(ctx, body, &capture_values, drop_fn)?
    };
    build_closure_fat_pointer(ctx, body, fn_ptr, env_ptr)
}

/// Resolve the address of a closure's capture-release glue
/// (`<body>.$drop_env$`, a [`koja_ir::FunctionKind::DropClosureGlue`])
/// for stashing in the env header. Closures with no heap-managed
/// capture have no glue, so this returns a null pointer and the
/// runtime frees the env without per-capture teardown.
fn closure_drop_env_ptr<'ctx>(ctx: &EmitContext<'ctx>, body: &IRSymbol) -> PointerValue<'ctx> {
    match ctx.declared_function(&closure_drop_env_symbol(body)) {
        Some(glue) => glue.as_global_value().as_pointer_value(),
        None => ctx.context.ptr_type(AddressSpace::default()).const_null(),
    }
}

/// Indirect call through a fat-pointer closure value. Splits the
/// fat pointer into `fn_ptr` + `env_ptr`, prepends `env_ptr` to the
/// user-visible args, and dispatches via `build_indirect_call` with
/// the closure-body signature. Returns `None` for `Unit`-returning
/// callees so the caller skips the value-map insert.
pub(super) fn emit_call_closure<'ctx>(
    ctx: &EmitContext<'ctx>,
    callee: ValueId,
    args: &[ValueId],
    result_ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
    let callee_value = lookup(values, callee)?;
    let mut user_param_types: Vec<IRType> = Vec::with_capacity(args.len());
    let mut user_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for arg in args {
        let value = lookup(values, *arg)?;
        user_param_types.push(ir_type_for_basic_value(value));
        user_args.push(value.into());
    }
    let fat_ty = closure_fat_ptr_type(ctx);
    let alloca = ctx.build_entry_alloca(fat_ty, "closure_call");
    ctx.builder
        .build_store(alloca, callee_value)
        .map_err(|e| inkwell_err("CallClosure spill", e))?;
    let fn_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 0, "closure_call.fn_ptr")
        .map_err(|e| inkwell_err("CallClosure fn_ptr GEP", e))?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, "closure_call.env_ptr")
        .map_err(|e| inkwell_err("CallClosure env_ptr GEP", e))?;
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let fn_ptr = ctx
        .builder
        .build_load(ptr_ty, fn_slot, "closure_call.fn")
        .map_err(|e| inkwell_err("CallClosure fn_ptr load", e))?
        .into_pointer_value();
    let env_ptr = ctx
        .builder
        .build_load(ptr_ty, env_slot, "closure_call.env")
        .map_err(|e| inkwell_err("CallClosure env_ptr load", e))?
        .into_pointer_value();
    let signature = closure_body_signature(ctx, &user_param_types, result_ty)?;
    let mut all_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(user_args.len() + 1);
    all_args.push(env_ptr.into());
    all_args.extend(user_args);
    let call_site = ctx
        .builder
        .build_indirect_call(signature, fn_ptr, &all_args, "closure_call")
        .map_err(|e| inkwell_err("CallClosure indirect_call", e))?;
    Ok(call_site.try_as_basic_value().basic())
}

/// Read a single captured value from the active closure body's env
/// block. `LoadCapture` is only valid inside a `FunctionKind::Closure`
/// body (seal-enforced); a missing closure frame is a compiler bug
/// rather than a recoverable codegen error.
pub(super) fn emit_load_capture<'ctx>(
    ctx: &EmitContext<'ctx>,
    capture_index: u32,
    ty: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let ClosureFrame {
        env_ptr,
        env_struct,
    } = ctx.closure_frame().unwrap_or_else(|| {
        panic!("LLVM emit: LoadCapture outside a closure body — seal invariant violation")
    });
    let slot_ptr = ctx
        .builder
        .build_struct_gep(
            env_struct,
            env_ptr,
            capture_index + CLOSURE_ENV_HEADER_FIELDS,
            &format!("env.{capture_index}"),
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("LoadCapture GEP for capture #{capture_index}"),
                e,
            )
        })?;
    let llvm_ty = ir_basic_type(ctx, ty)?;
    ctx.builder
        .build_load(llvm_ty, slot_ptr, &format!("capture.{capture_index}"))
        .map_err(|e| {
            inkwell_err(
                format_args!("LoadCapture load for capture #{capture_index}"),
                e,
            )
        })
}

/// Drop a closure value: `rc--` on its env block via
/// [`declare_closure_rc_dec_extern`]. The runtime handles the null
/// (captureless adapter) and immortal cases, and at zero runs the
/// env header's capture-release glue
/// ([`koja_ir::FunctionKind::DropClosureGlue`]) before freeing — so a
/// closure capturing heap values releases them transitively. Shared
/// by the slot-keyed ([`emit_drop_closure_env`]) and value-keyed
/// (`emit_drop_value`) closure drop paths.
pub(crate) fn emit_drop_closure_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    closure_value: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<(), LlvmError> {
    let env_ptr = load_closure_env_ptr(ctx, closure_value, label)?;
    let dec = declare_closure_rc_dec_extern(ctx);
    ctx.builder
        .build_call(dec, &[env_ptr.into()], &format!("{label}.env_rc_dec"))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("closure env rc_dec for `{label}`"), e))
}

/// Slot-keyed closure drop (`DropLocal` of an `IRType::Function`
/// slot). Thin wrapper over [`emit_drop_closure_value`].
pub(super) fn emit_drop_closure_env<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    closure_value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    emit_drop_closure_value(ctx, closure_value, &format!("{local}.drop"))
}

/// Split a `{fn_ptr, env_ptr}` fat pointer and load its `env_ptr`
/// field. Spill-then-GEP so the load works off the canonical
/// [`closure_fat_ptr_type`] regardless of how the SSA value was
/// produced. Shared by the closure clone (`rc++`) and drop
/// (`rc--`) paths.
pub(crate) fn load_closure_env_ptr<'ctx>(
    ctx: &EmitContext<'ctx>,
    closure_value: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let fat_ty = closure_fat_ptr_type(ctx);
    let alloca = ctx.build_entry_alloca(fat_ty, label);
    ctx.builder
        .build_store(alloca, closure_value)
        .map_err(|e| inkwell_err(format_args!("closure spill for `{label}`"), e))?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, &format!("{label}.env_ptr"))
        .map_err(|e| inkwell_err(format_args!("closure env_ptr GEP for `{label}`"), e))?;
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_load(ptr_ty, env_slot, &format!("{label}.env"))
        .map(|v| v.into_pointer_value())
        .map_err(|e| inkwell_err(format_args!("closure env_ptr load for `{label}`"), e))
}

/// Heap-allocate the env block, stamp its `[i64 rc][ptr drop_fn]`
/// header (rc = 1, `drop_fn` = the capture-release glue or null),
/// populate each capture slot via `getelementptr inbounds`, and
/// return the env base pointer (which doubles as the rc word for
/// `koja_rc_inc` / `koja_closure_rc_dec`). Empty layouts
/// short-circuit before this is called (see [`emit_make_closure`]).
fn emit_env_alloc_and_store<'ctx>(
    ctx: &EmitContext<'ctx>,
    body: &IRSymbol,
    captures: &[BasicValueEnum<'ctx>],
    drop_fn: PointerValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let mut field_types: Vec<BasicTypeEnum<'ctx>> =
        Vec::with_capacity(captures.len() + CLOSURE_ENV_HEADER_FIELDS as usize);
    field_types.push(i64_ty.into());
    field_types.push(ptr_ty.into());
    field_types.extend(captures.iter().map(|c| c.get_type()));
    let env_struct = ctx.context.struct_type(&field_types, false);
    let size_bytes = ctx.layouts.target_data.get_abi_size(&env_struct);
    let size_value = i64_ty.const_int(size_bytes, false);
    let malloc = declare_malloc_extern(ctx);
    let env_ptr = ctx
        .builder
        .build_call(malloc, &[size_value.into()], &format!("{body}.env"))
        .map_err(|e| inkwell_err(format_args!("env malloc for `{body}`"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("malloc returned void".to_string()))?
        .into_pointer_value();
    store_env_field(
        ctx,
        env_struct,
        env_ptr,
        0,
        i64_ty.const_int(1, false).into(),
        body,
        "rc",
    )?;
    store_env_field(ctx, env_struct, env_ptr, 1, drop_fn.into(), body, "drop_fn")?;
    for (index, capture) in captures.iter().enumerate() {
        let field = index as u32 + CLOSURE_ENV_HEADER_FIELDS;
        store_env_field(
            ctx,
            env_struct,
            env_ptr,
            field,
            *capture,
            body,
            &index.to_string(),
        )?;
    }
    Ok(env_ptr)
}

/// `getelementptr inbounds` to `env_struct` field `field` on
/// `env_ptr` and `store` `value` there. Names the temp `<body>.env.<tag>`.
fn store_env_field<'ctx>(
    ctx: &EmitContext<'ctx>,
    env_struct: inkwell::types::StructType<'ctx>,
    env_ptr: PointerValue<'ctx>,
    field: u32,
    value: BasicValueEnum<'ctx>,
    body: &IRSymbol,
    tag: &str,
) -> Result<(), LlvmError> {
    let slot_ptr = ctx
        .builder
        .build_struct_gep(env_struct, env_ptr, field, &format!("{body}.env.{tag}"))
        .map_err(|e| inkwell_err(format_args!("env GEP for `{body}` field `{tag}`"), e))?;
    ctx.builder
        .build_store(slot_ptr, value)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("env store for `{body}` field `{tag}`"), e))
}

/// Pack `{fn_ptr, env_ptr}` into the canonical closure fat-pointer
/// shape. Materialized via an entry-block alloca + two stores +
/// load so the caller sees a single SSA value of struct type
/// matching [`closure_fat_ptr_type`].
fn build_closure_fat_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    body: &IRSymbol,
    fn_ptr: PointerValue<'ctx>,
    env_ptr: PointerValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let fat_ty = closure_fat_ptr_type(ctx);
    let alloca = ctx.build_entry_alloca(fat_ty, &format!("{body}.closure"));
    let fn_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 0, &format!("{body}.fn_ptr"))
        .map_err(|e| inkwell_err(format_args!("fat-ptr fn_ptr GEP for `{body}`"), e))?;
    ctx.builder
        .build_store(fn_slot, fn_ptr)
        .map_err(|e| inkwell_err(format_args!("fat-ptr fn_ptr store for `{body}`"), e))?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, &format!("{body}.env_ptr"))
        .map_err(|e| inkwell_err(format_args!("fat-ptr env_ptr GEP for `{body}`"), e))?;
    ctx.builder
        .build_store(env_slot, env_ptr)
        .map_err(|e| inkwell_err(format_args!("fat-ptr env_ptr store for `{body}`"), e))?;
    ctx.builder
        .build_load(fat_ty, alloca, &format!("{body}.closure_value"))
        .map_err(|e| inkwell_err(format_args!("fat-ptr load for `{body}`"), e))
}

/// Recover the [`IRType`] surface a closure-call argument was lowered
/// from, given its LLVM `BasicValueEnum`. The [`closure_body_signature`]
/// helper rebuilds the indirect-call signature from these and we
/// only need enough fidelity that `ir_basic_type` round-trips —
/// integer width is preserved from the LLVM int width; floats /
/// pointers / aggregates pick a representative `IRType` whose LLVM
/// translation matches the value's type.
fn ir_type_for_basic_value(value: BasicValueEnum<'_>) -> IRType {
    match value {
        BasicValueEnum::IntValue(int) => match int.get_type().get_bit_width() {
            1 => IRType::Bool,
            8 => IRType::Int8,
            16 => IRType::Int16,
            32 => IRType::Int32,
            _ => IRType::Int64,
        },
        BasicValueEnum::FloatValue(_) => IRType::Float64,
        BasicValueEnum::PointerValue(_) => IRType::String,
        BasicValueEnum::StructValue(_)
        | BasicValueEnum::ArrayValue(_)
        | BasicValueEnum::VectorValue(_)
        | BasicValueEnum::ScalableVectorValue(_) => IRType::Int64,
    }
}
