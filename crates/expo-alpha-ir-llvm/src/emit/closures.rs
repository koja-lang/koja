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

use expo_alpha_ir::{IRLocalId, IRSymbol, IRType, ValueId};
use inkwell::AddressSpace;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::ctx::{ClosureFrame, EmitContext};
use crate::error::LlvmError;
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::{closure_body_signature, closure_fat_ptr_type, ir_basic_type};

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
            "alpha LLVM emit: closure body `{}` not registered in declared-functions \
             index — declaration order or seal violation",
            body.mangled(),
        )
    });
    let fn_ptr = body_fn.as_global_value().as_pointer_value();
    let env_ptr = if capture_values.is_empty() {
        ctx.context.ptr_type(AddressSpace::default()).const_null()
    } else {
        emit_env_alloc_and_store(ctx, body, &capture_values)?
    };
    build_closure_fat_pointer(ctx, body, fn_ptr, env_ptr)
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
        panic!("alpha LLVM emit: LoadCapture outside a closure body — seal invariant violation")
    });
    let slot_ptr = ctx
        .builder
        .build_struct_gep(
            env_struct,
            env_ptr,
            capture_index,
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

/// Drop a closure-typed local: free the env block (skipping null
/// env_ptrs the captureless adapter shape produces). Heap-typed
/// captures *inside* the env are not recursively dropped — that
/// needs a per-body drop function we synthesize alongside the
/// closure body, a follow-up. Captures of heap-typed locals
/// therefore leak today; the alpha milestone accepts the leak in
/// exchange for a simpler ABI.
pub(super) fn emit_drop_closure_env<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    closure_value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let fat_ty = closure_fat_ptr_type(ctx);
    let alloca = ctx.build_entry_alloca(fat_ty, &format!("{local}.drop"));
    ctx.builder
        .build_store(alloca, closure_value)
        .map_err(|e| inkwell_err(format_args!("drop spill for `{local}`"), e))?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, &format!("{local}.env_ptr"))
        .map_err(|e| inkwell_err(format_args!("drop env_ptr GEP for `{local}`"), e))?;
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let env_ptr = ctx
        .builder
        .build_load(ptr_ty, env_slot, &format!("{local}.env"))
        .map_err(|e| inkwell_err(format_args!("drop env_ptr load for `{local}`"), e))?
        .into_pointer_value();
    let is_null = ctx
        .builder
        .build_is_null(env_ptr, &format!("{local}.env_is_null"))
        .map_err(|e| inkwell_err(format_args!("drop null check for `{local}`"), e))?;
    let parent = ctx
        .builder
        .get_insert_block()
        .and_then(|b| b.get_parent())
        .ok_or_else(|| {
            LlvmError::Codegen(
                "DropLocal emitted outside a function context (compiler bug)".to_string(),
            )
        })?;
    let free_block = ctx
        .context
        .append_basic_block(parent, &format!("{local}.drop_free"));
    let cont_block = ctx
        .context
        .append_basic_block(parent, &format!("{local}.drop_cont"));
    ctx.builder
        .build_conditional_branch(is_null, cont_block, free_block)
        .map_err(|e| inkwell_err(format_args!("drop branch for `{local}`"), e))?;
    ctx.builder.position_at_end(free_block);
    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[env_ptr.into()], &format!("{local}.free"))
        .map_err(|e| inkwell_err(format_args!("env free call for `{local}`"), e))?;
    ctx.builder
        .build_unconditional_branch(cont_block)
        .map_err(|e| inkwell_err(format_args!("drop branch-cont for `{local}`"), e))?;
    ctx.builder.position_at_end(cont_block);
    Ok(())
}

/// Heap-allocate the env block, populate each capture slot via
/// `getelementptr inbounds`, and return the env payload pointer.
/// Empty layouts short-circuit before this is called (see
/// [`emit_make_closure`]).
fn emit_env_alloc_and_store<'ctx>(
    ctx: &EmitContext<'ctx>,
    body: &IRSymbol,
    captures: &[BasicValueEnum<'ctx>],
) -> Result<PointerValue<'ctx>, LlvmError> {
    let field_types: Vec<BasicTypeEnum<'ctx>> = captures.iter().map(|c| c.get_type()).collect();
    let env_struct = ctx.context.struct_type(&field_types, false);
    let size_bytes = ctx.layouts.target_data.get_abi_size(&env_struct);
    let size_value = ctx.context.i64_type().const_int(size_bytes, false);
    let malloc = declare_malloc_extern(ctx);
    let env_ptr = ctx
        .builder
        .build_call(malloc, &[size_value.into()], &format!("{body}.env"))
        .map_err(|e| inkwell_err(format_args!("env malloc for `{body}`"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("malloc returned void".to_string()))?
        .into_pointer_value();
    for (index, capture) in captures.iter().enumerate() {
        let slot_ptr = ctx
            .builder
            .build_struct_gep(
                env_struct,
                env_ptr,
                index as u32,
                &format!("{body}.env.{index}"),
            )
            .map_err(|e| inkwell_err(format_args!("env GEP for `{body}` capture #{index}"), e))?;
        ctx.builder
            .build_store(slot_ptr, *capture)
            .map_err(|e| inkwell_err(format_args!("env store for `{body}` capture #{index}"), e))?;
    }
    Ok(env_ptr)
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
