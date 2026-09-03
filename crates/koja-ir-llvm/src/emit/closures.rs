//! Closure-shaped instruction emission: `MakeClosure`, `CallClosure`,
//! `LoadCapture`, plus the `DropLocal` helper for `IRType::Function`
//! slots. Mirrors the IR vocabulary the [`crate::emit::instruction`]
//! dispatcher routes to.
//!
//! Closure values are `{fn_ptr, env_ptr}` fat pointers (see
//! [`crate::types::closure_fat_ptr_type`]). Closure-kind bodies
//! declare an extra `env_ptr` parameter at LLVM position 0 (see
//! [`crate::function::declare_function`]). Active closure bodies
//! stash their env pointer + env-struct type on
//! [`crate::ctx::EmitContext`] so `LoadCapture` can GEP into the
//! right slot at body-emit time.

use inkwell::module::Linkage;
use inkwell::types::StructType;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use inkwell::{AddressSpace, IntPredicate};
use koja_ir::mangling::{
    closure_copy_env_symbol, closure_drop_env_symbol, closure_eq_env_symbol, closure_site_id,
};
use koja_ir::{IRFunction, IRLocalId, IRSymbol, IRType, ValueId};

use crate::ctx::{ClosureFrame, EmitContext};
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::element::deep_copy_in_slot;
use crate::runtime::{declare_closure_rc_dec_extern, declare_malloc_extern};
use crate::types::{
    CLOSURE_ENV_HEADER_FIELDS, ENV_COPY_FN_FIELD, ENV_DROP_FN_FIELD, ENV_EQ_FN_FIELD, ENV_RC_FIELD,
    ENV_SITE_ID_FIELD, closure_body_signature, closure_fat_ptr_type, env_header_fields,
    env_struct_type, ir_basic_type,
};

use super::heap_layout::RC_IMMORTAL;
use super::{ValueMap, lookup};

/// Materialize the closure value: malloc the env block (or point at
/// the body's static immortal env for captureless adapters), store
/// each capture by index, then build the `{fn_ptr, env_ptr}` fat
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
             index (declaration order or seal violation)",
            body.mangled(),
        )
    });
    let fn_ptr = body_fn.as_global_value().as_pointer_value();
    let env_ptr = if capture_values.is_empty() {
        static_immortal_env(ctx, body)
    } else {
        emit_env_alloc_and_store(ctx, body, &capture_values)?
    };
    build_closure_fat_pointer(ctx, body, fn_ptr, env_ptr)
}

/// The header words every env of `body` carries: `drop_fn` /
/// `copy_fn` / `eq_fn` glue addresses (null where lowering registered
/// no sibling) and the body's site id.
struct EnvHeader<'ctx> {
    copy_fn: PointerValue<'ctx>,
    drop_fn: PointerValue<'ctx>,
    eq_fn: PointerValue<'ctx>,
    site_id: IntValue<'ctx>,
}

impl<'ctx> EnvHeader<'ctx> {
    fn for_body(ctx: &EmitContext<'ctx>, body: &IRSymbol) -> Self {
        Self {
            copy_fn: env_glue_ptr(ctx, &closure_copy_env_symbol(body)),
            drop_fn: env_glue_ptr(ctx, &closure_drop_env_symbol(body)),
            eq_fn: env_glue_ptr(ctx, &closure_eq_env_symbol(body)),
            site_id: ctx
                .context
                .i64_type()
                .const_int(closure_site_id(body), false),
        }
    }
}

/// Resolve the address of one of a closure's env glue siblings
/// (`$drop_env$` / `$copy_env$` / `$eq_env$`) for stashing in the env
/// header. A missing function yields a null pointer: lowering omits
/// `$drop_env$` when no capture is heap-managed (the runtime then
/// frees the env without per-capture teardown), omits `$eq_env$` for
/// captureless bodies (equal site ids settle equality), and only
/// hand-built IR omits `$copy_env$` (the runtime aborts if such an
/// env ever crosses a process boundary).
fn env_glue_ptr<'ctx>(ctx: &EmitContext<'ctx>, glue: &IRSymbol) -> PointerValue<'ctx> {
    match ctx.declared_function(glue) {
        Some(function) => function.as_global_value().as_pointer_value(),
        None => ctx.context.ptr_type(AddressSpace::default()).const_null(),
    }
}

/// The per-body static env a captureless closure points at:
/// `{RC_IMMORTAL, null, null, site_id, null}` in rodata, minted once
/// per module under `<body>.$env$`. Immortal rc makes every inc / dec
/// / deep-copy a no-op, the same convention string literals use, so
/// captureless closures still stay borrowed in lowering while carrying
/// a real site id for [`emit_closure_equals`].
fn static_immortal_env<'ctx>(ctx: &EmitContext<'ctx>, body: &IRSymbol) -> PointerValue<'ctx> {
    let name = format!("{}.$env$", body.mangled());
    if let Some(existing) = ctx.module.get_global(&name) {
        return existing.as_pointer_value();
    }
    let header = EnvHeader::for_body(ctx, body);
    let header_ty = ctx.context.struct_type(&env_header_fields(ctx), false);
    let initializer = header_ty.const_named_struct(&[
        ctx.context
            .i64_type()
            .const_int(RC_IMMORTAL as u64, false)
            .into(),
        header.drop_fn.into(),
        header.copy_fn.into(),
        header.site_id.into(),
        header.eq_fn.into(),
    ]);
    let global = ctx.module.add_global(header_ty, None, &name);
    global.set_initializer(&initializer);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    global.as_pointer_value()
}

/// `lhs == rhs` on two closure values of the same function type:
///
/// ```text
/// site_id(lhs) != site_id(rhs)  -> false
/// eq_fn(lhs) == null            -> true   (captureless body)
/// otherwise                     -> eq_fn(lhs_env, rhs)
/// ```
///
/// `eq_fn` is the body's `$eq_env$` glue, called with the closure-body
/// ABI: `lhs`'s env at position 0 and `rhs` as the one user param.
pub(super) fn emit_closure_equals<'ctx>(
    ctx: &EmitContext<'ctx>,
    lhs: ValueId,
    rhs: ValueId,
    ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let lhs_value = lookup(values, lhs)?;
    let rhs_value = lookup(values, rhs)?;
    let function = ctx
        .builder
        .get_insert_block()
        .and_then(|block| block.get_parent())
        .ok_or_else(|| {
            LlvmError::Codegen("LLVM emit: ClosureEquals emitted outside a function body".into())
        })?;
    let captures_block = ctx
        .context
        .append_basic_block(function, "closure_eq.captures");
    let call_block = ctx.context.append_basic_block(function, "closure_eq.call");
    let merge_block = ctx.context.append_basic_block(function, "closure_eq.merge");
    let bool_ty = ctx.context.bool_type();

    let lhs_env = load_closure_env_ptr(ctx, lhs_value, "closure_eq.lhs")?;
    let rhs_env = load_closure_env_ptr(ctx, rhs_value, "closure_eq.rhs")?;
    let header_ty = ctx.context.struct_type(&env_header_fields(ctx), false);
    let lhs_site = load_env_header_word(ctx, header_ty, lhs_env, ENV_SITE_ID_FIELD, "lhs.site")?;
    let rhs_site = load_env_header_word(ctx, header_ty, rhs_env, ENV_SITE_ID_FIELD, "rhs.site")?;
    let same_site = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            lhs_site.into_int_value(),
            rhs_site.into_int_value(),
            "closure_eq.same_site",
        )
        .or_ice()?;
    let site_block = ctx.builder.get_insert_block().expect("positioned");
    ctx.builder
        .build_conditional_branch(same_site, captures_block, merge_block)
        .or_ice()?;

    ctx.builder.position_at_end(captures_block);
    let eq_fn = load_env_header_word(ctx, header_ty, lhs_env, ENV_EQ_FN_FIELD, "lhs.eq_fn")?
        .into_pointer_value();
    let captureless = ctx
        .builder
        .build_is_null(eq_fn, "closure_eq.captureless")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(captureless, merge_block, call_block)
        .or_ice()?;

    ctx.builder.position_at_end(call_block);
    let signature = closure_body_signature(ctx, std::slice::from_ref(ty), &IRType::Bool)?;
    let captures_equal = ctx
        .builder
        .build_indirect_call(
            signature,
            eq_fn,
            &[lhs_env.into(), rhs_value.into()],
            "closure_eq.captures_equal",
        )
        .or_ice()?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("LLVM emit: closure eq glue returned void".into()))?;
    ctx.builder
        .build_unconditional_branch(merge_block)
        .or_ice()?;

    ctx.builder.position_at_end(merge_block);
    let phi = ctx.builder.build_phi(bool_ty, "closure_eq").or_ice()?;
    phi.add_incoming(&[
        (&bool_ty.const_zero(), site_block),
        (&bool_ty.const_all_ones(), captures_block),
        (&captures_equal, call_block),
    ]);
    Ok(phi.as_basic_value())
}

fn load_env_header_word<'ctx>(
    ctx: &EmitContext<'ctx>,
    header_ty: StructType<'ctx>,
    env_ptr: PointerValue<'ctx>,
    field: u32,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let slot = ctx
        .builder
        .build_struct_gep(header_ty, env_ptr, field, &format!("{label}.slot"))
        .or_ice()?;
    let word_ty = env_header_fields(ctx)[field as usize];
    ctx.builder.build_load(word_ty, slot, label).or_ice()
}

/// Indirect call through a fat-pointer closure value. Splits the
/// fat pointer into `fn_ptr` + `env_ptr`, prepends `env_ptr` to the
/// user-visible args, and dispatches via `build_indirect_call` with
/// the closure-body signature. `Unit`-returning callees compile to
/// `void` calls, so their result is the inert `i8 0` unit
/// placeholder.
pub(super) fn emit_call_closure<'ctx>(
    ctx: &EmitContext<'ctx>,
    callee: ValueId,
    args: &[ValueId],
    param_types: &[IRType],
    result_ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let callee_value = lookup(values, callee)?;
    let mut user_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for arg in args {
        user_args.push(lookup(values, *arg)?.into());
    }
    let fat_ty = closure_fat_ptr_type(ctx);
    let alloca = ctx.build_entry_alloca(fat_ty, "closure_call");
    ctx.builder.build_store(alloca, callee_value).or_ice()?;
    let fn_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 0, "closure_call.fn_ptr")
        .or_ice()?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, "closure_call.env_ptr")
        .or_ice()?;
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let fn_ptr = ctx
        .builder
        .build_load(ptr_ty, fn_slot, "closure_call.fn")
        .or_ice()?
        .into_pointer_value();
    let env_ptr = ctx
        .builder
        .build_load(ptr_ty, env_slot, "closure_call.env")
        .or_ice()?
        .into_pointer_value();
    let signature = closure_body_signature(ctx, param_types, result_ty)?;
    let mut all_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(user_args.len() + 1);
    all_args.push(env_ptr.into());
    all_args.extend(user_args);
    let call_site = ctx
        .builder
        .build_indirect_call(signature, fn_ptr, &all_args, "closure_call")
        .or_ice()?;
    Ok(call_site
        .try_as_basic_value()
        .basic()
        .unwrap_or_else(|| ctx.context.i8_type().const_zero().into()))
}

/// Read a single captured value from the active closure body's env
/// block. `LoadCapture` is only valid inside a `FunctionKind::Closure`
/// body (seal-enforced), so a missing closure frame is a compiler bug
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
        panic!("LLVM emit: LoadCapture outside a closure body (seal invariant violation)")
    });
    load_capture_slot(ctx, env_struct, env_ptr, capture_index, ty, "capture")
}

/// Read capture `capture_index` out of another closure value's env.
/// Only valid inside an `$eq_env$` body (seal-enforced), whose
/// `other` param shares the active frame's `env_struct` layout, so
/// the same GEP shape applies to the foreign env pointer.
pub(super) fn emit_load_capture_of<'ctx>(
    ctx: &EmitContext<'ctx>,
    closure: ValueId,
    capture_index: u32,
    ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let ClosureFrame { env_struct, .. } = ctx.closure_frame().unwrap_or_else(|| {
        panic!("LLVM emit: LoadCaptureOf outside a closure body (seal invariant violation)")
    });
    let closure_value = lookup(values, closure)?;
    let env_ptr = load_closure_env_ptr(ctx, closure_value, "capture_of")?;
    load_capture_slot(ctx, env_struct, env_ptr, capture_index, ty, "capture_of")
}

fn load_capture_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    env_struct: StructType<'ctx>,
    env_ptr: PointerValue<'ctx>,
    capture_index: u32,
    ty: &IRType,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let slot_ptr = ctx
        .builder
        .build_struct_gep(
            env_struct,
            env_ptr,
            capture_index + CLOSURE_ENV_HEADER_FIELDS,
            &format!("env.{capture_index}"),
        )
        .or_ice()?;
    let llvm_ty = ir_basic_type(ctx, ty)?;
    ctx.builder
        .build_load(llvm_ty, slot_ptr, &format!("{label}.{capture_index}"))
        .or_ice()
}

/// Drop a closure value: `rc--` on its env block via
/// [`declare_closure_rc_dec_extern`]. The runtime handles the null
/// (captureless adapter) and immortal cases, and at zero runs the
/// env header's capture-release glue
/// ([`koja_ir::FunctionKind::DropClosureGlue`]) before freeing, so a
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
        .or_ice()
        .map(|_| ())
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

/// Synthesize the `<body>.$copy_env$` env deep-copy glue body
/// ([`koja_ir::FunctionKind::CopyClosureGlue`], `i8* (i8*)` over env
/// bases). The runtime's `koja_closure_deep_copy` dispatches here
/// through the env header's `copy_fn` word when a closure crosses a
/// process boundary:
///
/// 1. malloc a block the size of the env struct and `memcpy` the
///    whole source env over it: header (`drop_fn` / `copy_fn` /
///    `site_id` / `eq_fn`) and `Copy` captures land correct as-is.
/// 2. reset the fresh block's rc to 1 (the source's count came along
///    in the copy).
/// 3. deep-copy every heap-managed capture in place
///    ([`deep_copy_in_slot`] skips scalars), severing every share
///    with the source env.
pub(crate) fn emit_copy_closure_glue_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    env_layout: &[IRType],
) -> Result<(), LlvmError> {
    let symbol = &function.symbol;
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    let src_env = llvm_function
        .get_nth_param(0)
        .unwrap_or_else(|| {
            panic!(
                "LLVM emit: env deep-copy glue `{symbol}` declared no env parameter \
                 (declare_function ABI invariant violation)",
            )
        })
        .into_pointer_value();

    let env_struct = env_struct_type(ctx, env_layout)?;
    let i64_ty = ctx.context.i64_type();
    let size = i64_ty.const_int(ctx.layouts.target_data.get_abi_size(&env_struct), false);
    let malloc = declare_malloc_extern(ctx);
    let new_env = ctx
        .call_basic(malloc, &[size.into()], "new_env")?
        .into_pointer_value();
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(memcpy, &[new_env.into(), src_env.into(), size.into()], "")
        .or_ice()?;
    ctx.builder
        .build_store(new_env, i64_ty.const_int(1, false))
        .or_ice()?;

    for (index, capture_ty) in env_layout.iter().enumerate() {
        let field = index as u32 + CLOSURE_ENV_HEADER_FIELDS;
        let slot = ctx
            .builder
            .build_struct_gep(env_struct, new_env, field, &format!("env.{index}"))
            .or_ice()?;
        deep_copy_in_slot(ctx, capture_ty, slot)?;
    }

    ctx.builder
        .build_return(Some(&new_env))
        .or_ice()
        .map(|_| ())
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
    ctx.builder.build_store(alloca, closure_value).or_ice()?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, &format!("{label}.env_ptr"))
        .or_ice()?;
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_load(ptr_ty, env_slot, &format!("{label}.env"))
        .or_ice()
        .map(|v| v.into_pointer_value())
}

/// Heap-allocate the env block, stamp its header (rc = 1, the glue
/// siblings or null, and the body's site id), populate each capture
/// slot via `getelementptr inbounds`, and return the env base pointer
/// (which doubles as the rc word for `koja_rc_inc` /
/// `koja_closure_rc_dec`). Empty layouts take the static env path
/// instead (see [`emit_make_closure`]).
fn emit_env_alloc_and_store<'ctx>(
    ctx: &EmitContext<'ctx>,
    body: &IRSymbol,
    captures: &[BasicValueEnum<'ctx>],
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let mut field_types = env_header_fields(ctx);
    field_types.extend(captures.iter().map(|c| c.get_type()));
    let env_struct = ctx.context.struct_type(&field_types, false);
    let size_bytes = ctx.layouts.target_data.get_abi_size(&env_struct);
    let size_value = i64_ty.const_int(size_bytes, false);
    let malloc = declare_malloc_extern(ctx);
    let env_ptr = ctx
        .call_basic(malloc, &[size_value.into()], &format!("{body}.env"))?
        .into_pointer_value();
    let header = EnvHeader::for_body(ctx, body);
    let header_words: [(u32, BasicValueEnum<'ctx>, &str); 5] = [
        (ENV_RC_FIELD, i64_ty.const_int(1, false).into(), "rc"),
        (ENV_DROP_FN_FIELD, header.drop_fn.into(), "drop_fn"),
        (ENV_COPY_FN_FIELD, header.copy_fn.into(), "copy_fn"),
        (ENV_SITE_ID_FIELD, header.site_id.into(), "site_id"),
        (ENV_EQ_FN_FIELD, header.eq_fn.into(), "eq_fn"),
    ];
    for (field, value, tag) in header_words {
        store_env_field(ctx, env_struct, env_ptr, field, value, body, tag)?;
    }
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
        .or_ice()?;
    ctx.builder
        .build_store(slot_ptr, value)
        .or_ice()
        .map(|_| ())
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
        .or_ice()?;
    ctx.builder.build_store(fn_slot, fn_ptr).or_ice()?;
    let env_slot = ctx
        .builder
        .build_struct_gep(fat_ty, alloca, 1, &format!("{body}.env_ptr"))
        .or_ice()?;
    ctx.builder.build_store(env_slot, env_ptr).or_ice()?;
    ctx.builder
        .build_load(fat_ty, alloca, &format!("{body}.closure_value"))
        .or_ice()
}
