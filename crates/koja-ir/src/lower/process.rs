//! Spawn / receive lowering. Both expressions are concurrency
//! primitives that the LLVM backend turns into `koja_rt_*` calls;
//! the IR layer captures enough typed structure that the LLVM emit
//! pass and the eval interpreter can match on
//! [`crate::IRInstruction::Spawn`] / [`crate::IRInstruction::Receive`]
//! without re-walking the AST.
//!
//! `spawn Type.start(config)` lowers to:
//!
//! - one or more discovered [`crate::generics::Instantiation`]s for
//!   the state type's `start` / `run` methods,
//! - a synthesized [`crate::FunctionKind::SpawnWrapper`] thunk
//!   keyed by the state symbol (deduplicated across spawn sites),
//! - a single [`crate::IRInstruction::Spawn`] in the host block
//!   producing a `Ref<M, R>`-typed value.
//!
//! `receive` lowers to:
//!
//! - one body block per arm, with the arm's typed-binding payload
//!   declared as an [`crate::IRInstruction::LocalDecl`] in the
//!   function's entry block (the runtime writes the deserialized
//!   payload into the slot before branching to the body),
//! - an optional `after` body block plus a lowered timeout value,
//! - a single [`crate::IRInstruction::Receive`] in the host block
//!   that the LLVM backend lowers into the actual mailbox dispatch.
//!
//! The receive's result rides through a synthesized merge block
//! whose [`crate::function::BlockParam`] holds the join value;
//! every arm body branches to the merge block carrying its tail.
//! The host block's terminator is [`crate::IRTerminator::Unreachable`]
//! because dispatch always leaves the receive instruction.

use koja_ast::ast::{Arg, Diagnostic, Expr, ExprKind, MatchArm, Pattern, Statement};
use koja_ast::identifier::{GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_typecheck::{GlobalKind, GlobalRegistry, RegistryEntry};

use crate::enum_decl::IRVariantTag;
use crate::function::{
    BranchTarget, FunctionKind, IRBlockId, IRFunction, IRFunctionParam, IRInstruction, IRSymbol,
    IRTerminator, ReceiveAfter, ReceiveArm, ReceiveTag,
};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::mangling::mangled_method_name;
use crate::types::{ConstValue, IRBinOp, IRType, ValueId};

use super::arms::{lower_arm_into, lower_result_ty};
use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::ownership::{
    drop_discarded_temp, emit_slot_drops, materialize_boundary_copy, materialize_owned,
    promote_param,
};
use super::package::resolved_type_to_ir_type;

/// Lower `spawn Type.start(config)`. Typecheck has already validated
/// the inner shape; lowering turns it into a config value, an
/// enqueued instantiation for the receiver's `start` / `run`
/// methods, a synthesized [`FunctionKind::SpawnWrapper`], and a
/// single [`IRInstruction::Spawn`] producing a typed `Ref<M, R>`.
pub(super) fn lower_spawn(
    inner: &Expr,
    span: Span,
    ref_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let target = resolve_spawn_target(inner).ok_or_else(|| {
        output.diagnostics.push(Diagnostic::error(
            "IR lower: `spawn` inner must be a static `Type.start(config)` call \
             (typecheck seal invariant)",
            span,
        ));
    })?;

    let config_arg = target.args.first().ok_or_else(|| {
        output.diagnostics.push(Diagnostic::error(
            "IR lower: `spawn Type.start(...)` requires a single config argument",
            span,
        ));
    })?;
    let (config_value, current) = lower_expr(&config_arg.value, ctx, block, registry, output)?;
    let config_type = ctx.type_of(config_value);
    // `spawn` hands the config to the child process, so deep-copy it —
    // the child must share no heap storage with the spawner (rc
    // bookkeeping is unsynchronized), mirroring the message-send
    // payload copy. The copy is *transferred*: it is never released
    // here (the runtime owns it through the spawn payload's drop glue);
    // an owned temp source is dead once the copy is taken.
    let copied_config = materialize_boundary_copy(ctx, current, config_value, &config_type);
    if copied_config != config_value {
        drop_discarded_temp(ctx, current, config_value);
    }
    let config_value = copied_config;

    let state_ir_type = resolved_type_to_ir_type(
        &target.receiver_resolution,
        registry,
        &mut output.instantiations,
    );
    let state_symbol = struct_symbol(&state_ir_type).ok_or_else(|| {
        output.diagnostics.push(Diagnostic::error(
            format!(
                "IR lower: `spawn` receiver must lower to a struct type \
                 (got `{state_ir_type:?}`)",
            ),
            span,
        ));
    })?;

    enqueue_process_method_instantiations(&target, registry, output);

    let body_types = ProcessBodyTypes::resolve(
        &target.receiver_resolution,
        state_ir_type.clone(),
        config_type.clone(),
        registry,
        output,
    );
    let wrapper_symbol = synthesize_spawn_wrapper(&state_symbol, body_types, output);

    let ref_ir_type =
        resolved_type_to_ir_type(ref_resolution, registry, &mut output.instantiations);
    let ref_symbol = struct_symbol(&ref_ir_type).ok_or_else(|| {
        output.diagnostics.push(Diagnostic::error(
            format!("IR lower: `spawn` result must be `Ref<M, R>` (got `{ref_ir_type:?}`)",),
            span,
        ));
    })?;

    let dest = ctx.fresh_value(ref_ir_type);
    ctx.cfg.append(
        current,
        IRInstruction::Spawn {
            config: config_value,
            config_type,
            dest,
            ref_type: ref_symbol,
            wrapper: wrapper_symbol,
        },
    );
    Ok((dest, current))
}

/// AST-side inputs to [`lower_receive`]. Bundled per the same
/// `too_many_arguments` discipline [`super::match_expr::MatchLowering`]
/// uses.
pub(super) struct ReceiveLowering<'a> {
    pub(super) after_body: &'a [Statement],
    pub(super) after_timeout: Option<&'a Expr>,
    pub(super) arms: &'a [MatchArm],
    pub(super) result_resolution: &'a ResolvedType,
    pub(super) span: Span,
}

/// Lower `receive arms after timeout body end`. Each arm becomes an
/// [`IRBlockId`] whose payload local has been declared in the
/// function's entry block and whose tail branches to a synthesized
/// merge block carrying the join value as a [`crate::function::BlockParam`].
/// The host block ends with the [`IRInstruction::Receive`] dispatch
/// followed by [`IRTerminator::Unreachable`] — every reachable exit
/// goes through the arm bodies into the merge block.
pub(super) fn lower_receive(
    inputs: ReceiveLowering<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let ReceiveLowering {
        after_body,
        after_timeout,
        arms,
        result_resolution,
        span,
    } = inputs;
    if arms.is_empty() {
        output.diagnostics.push(Diagnostic::error(
            "IR lower: `receive` reaches lower with zero arms — typecheck seal violation",
            span,
        ));
        return Err(());
    }
    let result_ty = lower_result_ty(result_resolution, registry, output);
    let merge_block = ctx.fresh_block("receive_merge");
    let result_id = ctx.declare_merge_param(merge_block, result_ty.clone());

    let mut lowered_arms = Vec::with_capacity(arms.len());
    for (index, arm) in arms.iter().enumerate() {
        let lowered =
            lower_receive_arm(arm, index, merge_block, &result_ty, ctx, registry, output)?;
        lowered_arms.push(lowered);
    }

    let mut current = block;
    let after = if let Some(timeout_expr) = after_timeout {
        let (timeout_value, after_eval_block) =
            lower_expr(timeout_expr, ctx, current, registry, output)?;
        current = after_eval_block;
        let after_block = ctx.fresh_block("receive_after");
        lower_arm_into(
            after_body,
            ctx,
            after_block,
            merge_block,
            &result_ty,
            registry,
            output,
        )?;
        Some(ReceiveAfter {
            body: after_block,
            timeout: timeout_value,
        })
    } else {
        None
    };

    let dest = ctx.fresh_value(result_ty.clone());
    ctx.cfg.append(
        current,
        IRInstruction::Receive {
            after,
            arms: lowered_arms,
            dest,
            result_type: result_ty,
        },
    );
    ctx.cfg.set_terminator(current, IRTerminator::Unreachable);

    Ok((result_id, merge_block))
}

/// Lower one receive arm. Pulls the typed-binding's local id +
/// resolved payload type off the pattern (stamped during typecheck-
/// resolve in [`koja_typecheck::pipeline::resolve::process`]),
/// declares the corresponding payload slot in the function's entry
/// block, then walks the arm's body in a fresh body block, branching
/// the tail back to `merge_block` with the lattice-coerced result.
fn lower_receive_arm(
    arm: &MatchArm,
    index: usize,
    merge_block: IRBlockId,
    result_ty: &IRType,
    ctx: &mut FnLowerCtx,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<ReceiveArm, ()> {
    let Pattern::TypedBinding {
        local_id,
        resolved_type,
        ..
    } = &arm.pattern
    else {
        panic!(
            "IR lower: receive arm reaches lower without a TypedBinding pattern \
             (typecheck seal violation; got {:?})",
            arm.pattern,
        );
    };
    let local_id = local_id.unwrap_or_else(|| {
        panic!(
            "IR lower: receive arm pattern carries no LocalId — typecheck resolve \
             invariant violation",
        );
    });
    let payload_resolution = resolved_type.as_ref().unwrap_or_else(|| {
        panic!(
            "IR lower: receive arm pattern carries no resolved_type — typecheck resolve \
             invariant violation",
        );
    });

    let payload_type =
        resolved_type_to_ir_type(payload_resolution, registry, &mut output.instantiations);
    let tag = receive_tag_for(payload_resolution, registry).unwrap_or_else(|| {
        panic!(
            "IR lower: receive arm payload type does not match a known envelope \
             (typecheck seal violation): {payload_resolution:?}"
        );
    });

    let ir_local = IRLocalId::from_local_id(local_id);
    let entry = ctx.entry_block();
    if !ctx.local_is_declared(ir_local) {
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: payload_type.clone(),
            },
        );
        ctx.mark_local_declared(ir_local, payload_type.clone());
    }

    if let Some(guard) = &arm.guard {
        output.diagnostics.push(Diagnostic::error(
            "IR lower: `receive` arms with guards are not yet supported",
            guard.span,
        ));
        return Err(());
    }

    let body_block = ctx.fresh_block(format!("receive_arm_{index}"));
    lower_arm_into(
        &arm.body,
        ctx,
        body_block,
        merge_block,
        result_ty,
        registry,
        output,
    )?;

    Ok(ReceiveArm {
        body: body_block,
        payload_local: ir_local,
        payload_type,
        tag,
    })
}

/// Inputs distilled from a `spawn Type.start(config)` site. Pulled
/// off the typecheck-resolved AST in [`resolve_spawn_target`] so the
/// downstream wrapper-synthesis and method-instantiation paths can
/// share one struct.
struct SpawnTarget<'a> {
    args: &'a [Arg],
    receiver_id: GlobalRegistryId,
    receiver_resolution: ResolvedType,
}

fn resolve_spawn_target(inner: &Expr) -> Option<SpawnTarget<'_>> {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        ..
    } = &inner.kind
    else {
        return None;
    };
    if method != "start" {
        return None;
    }
    let ResolvedType::Named {
        resolution: Resolution::Global(receiver_id),
        ..
    } = &receiver.resolution
    else {
        return None;
    };
    Some(SpawnTarget {
        args,
        receiver_id: *receiver_id,
        receiver_resolution: receiver.resolution.clone(),
    })
}

/// Discover the `start`, `run`, and `priority` method instantiations
/// for the receiver. `start` is what the wrapper invokes; `run` is the
/// receive loop the wrapper chains into when `start` succeeds;
/// `priority` is read once right after `start` to set the process's
/// scheduling priority. All go through [`LowerOutput::instantiations`] so the
/// monomorphization driver synthesizes concrete bodies before seal.
/// Non-generic receivers skip enqueueing — the methods already lift
/// as concrete decls.
fn enqueue_process_method_instantiations(
    target: &SpawnTarget<'_>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) {
    let receiver_args = receiver_type_args(&target.receiver_resolution);
    if receiver_args.is_empty() {
        return;
    }
    for method in ["priority", "run", "start"] {
        if let Some(method_id) = lookup_sibling_method(target.receiver_id, method, registry) {
            output.instantiations.push(Instantiation {
                template: method_id,
                args: receiver_args.clone(),
                method_args: Vec::new(),
                owner: target.receiver_id,
            });
        }
    }
}

fn lookup_sibling_method(
    receiver_id: GlobalRegistryId,
    method: &str,
    registry: &GlobalRegistry,
) -> Option<GlobalRegistryId> {
    let entry = registry.get(receiver_id)?;
    let mut path = entry.identifier.path().to_vec();
    path.push(method.to_string());
    let identifier = Identifier::new(entry.identifier.package(), path);
    registry.lookup(&identifier).map(|(id, _)| id)
}

fn receiver_type_args(receiver: &ResolvedType) -> Vec<ResolvedType> {
    match receiver {
        ResolvedType::Named { type_args, .. } => type_args.clone(),
        _ => Vec::new(),
    }
}

/// The IR types a process body needs beyond the state symbol itself.
/// Resolved once per synthesis site via [`Self::resolve`] so the
/// body builder never touches the AST or registry.
pub(crate) struct ProcessBodyTypes {
    pub(crate) config: IRType,
    pub(crate) priority: IRType,
    /// Compile-time tag of `Priority.High`, resolved by name so the
    /// scheduling-weight diamond in [`emit_apply_priority`] never
    /// depends on the enum's source variant order.
    pub(crate) priority_high_tag: u8,
    /// Compile-time tag of `Priority.Low`, resolved by name.
    pub(crate) priority_low_tag: u8,
    pub(crate) result: IRType,
    pub(crate) state: IRType,
    pub(crate) stop_reason: IRType,
}

impl ProcessBodyTypes {
    /// Resolve the `Result<State, StopReason>` cell, the `StopReason`
    /// enum, and the `Priority` enum for `state_resolved`. Constructed
    /// from the registry rather than read off `start`'s signature:
    /// typecheck already guarantees `start` returns exactly this shape,
    /// and building it here keeps the path free of `Self`-typed
    /// signatures. Routing through [`resolved_type_to_ir_type`]
    /// enqueues the generic `Result` cell instantiation.
    pub(crate) fn resolve(
        state_resolved: &ResolvedType,
        state: IRType,
        config: IRType,
        registry: &GlobalRegistry,
        output: &mut LowerOutput,
    ) -> Self {
        let result_id = lookup_global(registry, "Result");
        let stop_reason_id = lookup_global(registry, "StopReason");
        let priority_id = lookup_global(registry, "Priority");
        let stop_reason_resolved = ResolvedType::leaf(Resolution::Global(stop_reason_id));
        let priority_resolved = ResolvedType::leaf(Resolution::Global(priority_id));
        let result_resolved = ResolvedType::Named {
            resolution: Resolution::Global(result_id),
            type_args: vec![state_resolved.clone(), stop_reason_resolved.clone()],
        };
        let result =
            resolved_type_to_ir_type(&result_resolved, registry, &mut output.instantiations);
        let stop_reason =
            resolved_type_to_ir_type(&stop_reason_resolved, registry, &mut output.instantiations);
        let priority =
            resolved_type_to_ir_type(&priority_resolved, registry, &mut output.instantiations);
        let (priority_high_tag, priority_low_tag) = priority_variant_tags(registry, priority_id);
        Self {
            config,
            priority,
            priority_high_tag,
            priority_low_tag,
            result,
            state,
            stop_reason,
        }
    }
}

/// Resolve the `(High, Low)` variant tags of `Priority` by name. Tags
/// are order-independent, so reordering the enum's source variants
/// can't shift the scheduling weight the compiler emits. Asserts the
/// three known variants resolve to distinct tags — a renamed or merged
/// variant is a seal-level breakage we want to fail loudly on.
fn priority_variant_tags(registry: &GlobalRegistry, priority_id: GlobalRegistryId) -> (u8, u8) {
    let Some(RegistryEntry {
        kind: GlobalKind::Enum(Some(definition)),
        ..
    }) = registry.get(priority_id)
    else {
        panic!("IR lower: `Global.Priority` is not a stamped enum in the registry");
    };
    let tag_of = |name: &str| {
        definition
            .lookup_variant(name)
            .unwrap_or_else(|| panic!("IR lower: `Priority` is missing variant `{name}`"))
            .0 as u8
    };
    let (high, low, normal) = (tag_of("High"), tag_of("Low"), tag_of("Normal"));
    assert!(
        high != low && low != normal && high != normal,
        "IR lower: `Priority` variants High/Low/Normal must have distinct tags \
         (got high={high}, low={low}, normal={normal})",
    );
    (high, low)
}

fn lookup_global(registry: &GlobalRegistry, name: &str) -> GlobalRegistryId {
    registry
        .lookup(&Identifier::new("Global", vec![name.to_string()]))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("IR lower: `Global.{name}` missing from registry"))
}

/// Mint (or reuse) the [`FunctionKind::SpawnWrapper`] thunk keyed by
/// `state_symbol`, plus its `<state>.__spawn_body` companion. The
/// pair is content-addressed off the state type so distinct
/// `spawn S.start(...)` sites for the same `S` share one wrapper;
/// distinct monomorphized state cells get distinct pairs exactly
/// like generic structs do.
///
/// The wrapper is a pure ABI shim: the LLVM emit pass declares it
/// `void(i8*)` (the scheduler's `ProcessFn` shape), loads the typed
/// config out of the runtime-provided pointer, and calls the body —
/// which is the function its IR `Call` already names. All real
/// semantics (`start`, the `Result` match, `run`) live in the body,
/// a [`FunctionKind::Regular`] function built by
/// [`build_process_body`] with normal ownership markers.
fn synthesize_spawn_wrapper(
    state_symbol: &IRSymbol,
    types: ProcessBodyTypes,
    output: &mut LowerOutput,
) -> IRSymbol {
    let wrapper_symbol = state_symbol.derived(".__spawn_wrapper");
    if !output.spawn_wrappers.insert(wrapper_symbol.clone()) {
        return wrapper_symbol;
    }
    let body = build_process_body(
        state_symbol.derived(".__spawn_body"),
        state_symbol,
        &types,
        ProcessBodyTail::Discard,
    );
    let wrapper = build_wrapper_shim(
        wrapper_symbol.clone(),
        FunctionKind::SpawnWrapper { state: types.state },
        &body,
    );
    output.synthesized_functions.push(body);
    output.synthesized_functions.push(wrapper);
    wrapper_symbol
}

/// Mint the project-mode [`FunctionKind::ProcessEntryWrapper`] thunk
/// for `state_symbol`, plus its `<state>.__entry_body` companion.
/// Differs from [`synthesize_spawn_wrapper`] in two ways: the
/// symbols carry `.__entry_*` suffixes (so they can coexist with a
/// regular `.__spawn_*` pair if the program also spawns its own
/// state cell), and the body routes both arms' `StopReason` through
/// `Global.StopReason.code` and returns the exit code, which the
/// LLVM shim stores into the module-level `__koja_exit_code` global
/// that the synthesized `main` trampoline returns from.
///
/// Returns `[body, wrapper]`; the caller routes both into the
/// state's owning package.
pub(crate) fn synthesize_process_entry_wrapper(
    state_symbol: &IRSymbol,
    types: ProcessBodyTypes,
) -> [IRFunction; 2] {
    let body = build_process_body(
        state_symbol.derived(".__entry_body"),
        state_symbol,
        &types,
        ProcessBodyTail::ExitCode,
    );
    let wrapper = build_wrapper_shim(
        state_symbol.derived(".__entry_wrapper"),
        FunctionKind::ProcessEntryWrapper { state: types.state },
        &body,
    );
    [body, wrapper]
}

/// What a process body does with the `StopReason` each arm ends up
/// holding (`run`'s return on the `Ok` path, `start`'s `Err` payload
/// otherwise).
enum ProcessBodyTail {
    /// Drop it and return `Unit` — the scheduler manages the spawned
    /// process's lifecycle; nobody reads the wrapper's result.
    Discard,
    /// Hand it to `Global.StopReason.code` and return the `Int64`
    /// exit code for the LLVM shim to store into `__koja_exit_code`.
    ExitCode,
}

impl ProcessBodyTail {
    fn return_type(&self) -> IRType {
        match self {
            Self::Discard => IRType::Unit,
            Self::ExitCode => IRType::Int64,
        }
    }
}

/// Hand-build the `(config) -> Unit | Int64` process body:
///
/// ```text
/// entry:
///   slot = promote(config)            ; normal param acquisition
///   result = call <state>.start(slot)
///   cond_br result.tag == Ok, start_ok, start_err
/// start_ok:
///   state = clone result.Ok.0 ; drop result
///   stop_reason = call <state>.run(state) ; drop state
///   <tail>
/// start_err:
///   stop_reason = clone result.Err.0 ; drop result
///   <tail>
/// ```
///
/// Every owned value is paired with a `Clone` / `Drop` marker, so
/// the [`crate::elaborate`] pass rewrites composite ownership into
/// glue exactly as for AST-lowered functions — no manual lifetime
/// obligations survive into the backends.
fn build_process_body(
    body_symbol: IRSymbol,
    state_symbol: &IRSymbol,
    types: &ProcessBodyTypes,
    tail: ProcessBodyTail,
) -> IRFunction {
    let IRType::Enum(result_symbol) = &types.result else {
        panic!(
            "IR lower: process body `{}` start return must lower to an enum, got `{:?}`",
            body_symbol.mangled(),
            types.result,
        );
    };
    let result_symbol = result_symbol.clone();

    let mut ctx = FnLowerCtx::new();
    ctx.closures_mut().set_enclosing_symbol(body_symbol.clone());
    let entry = ctx.fresh_block("entry");

    let config_local = IRLocalId::from_local_id(LocalId::new(0));
    let param = promote_param(&mut ctx, entry, config_local, types.config.clone());
    let config_read = ctx.fresh_value(types.config.clone());
    ctx.cfg.append(
        entry,
        IRInstruction::LocalRead {
            dest: config_read,
            local: config_local,
            ty: types.config.clone(),
        },
    );

    let start_result = ctx.fresh_value(types.result.clone());
    ctx.cfg.append(
        entry,
        IRInstruction::Call {
            args: vec![config_read],
            callee: mangled_method_name(state_symbol, &[], "start", &[]),
            dest: start_result,
        },
    );
    ctx.mark_owned(start_result);

    let ok_block = ctx.fresh_block("start_ok");
    let err_block = ctx.fresh_block("start_err");
    emit_result_tag_branch(
        &mut ctx,
        entry,
        start_result,
        &result_symbol,
        ok_block,
        err_block,
    );

    // Ok arm: clone the state out, release the scrutinee, apply the
    // declared priority, then chain into the run loop (which borrows
    // the state), release the state.
    let state_field = extract_result_payload(
        &mut ctx,
        ok_block,
        start_result,
        &result_symbol,
        0,
        &types.state,
    );
    drop_discarded_temp(&mut ctx, ok_block, start_result);
    let ok_block = emit_apply_priority(&mut ctx, ok_block, state_symbol, state_field, types);
    let stop_reason = ctx.fresh_value(types.stop_reason.clone());
    ctx.cfg.append(
        ok_block,
        IRInstruction::Call {
            args: vec![state_field],
            callee: mangled_method_name(state_symbol, &[], "run", &[]),
            dest: stop_reason,
        },
    );
    ctx.mark_owned(stop_reason);
    drop_discarded_temp(&mut ctx, ok_block, state_field);
    finish_process_arm(&mut ctx, ok_block, stop_reason, types, &tail);

    // Err arm: `start` declined; its `StopReason` payload is the
    // process's stop reason directly.
    let err_reason = extract_result_payload(
        &mut ctx,
        err_block,
        start_result,
        &result_symbol,
        1,
        &types.stop_reason,
    );
    drop_discarded_temp(&mut ctx, err_block, start_result);
    finish_process_arm(&mut ctx, err_block, err_reason, types, &tail);

    IRFunction {
        blocks: ctx.into_blocks(),
        def_location: None,
        kind: FunctionKind::Regular,
        params: vec![param],
        return_type: tail.return_type(),
        symbol: body_symbol,
    }
}

/// Emit `cond_br (result.tag == 0) ok, err` as `block`'s terminator.
fn emit_result_tag_branch(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    result: ValueId,
    result_symbol: &IRSymbol,
    ok_block: IRBlockId,
    err_block: IRBlockId,
) {
    let tag = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::EnumTagGet {
            dest: tag,
            value: result,
            ty: result_symbol.clone(),
        },
    );
    let ok_tag = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: ok_tag,
            value: ConstValue::Int8(0),
        },
    );
    let is_ok = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest: is_ok,
            lhs: tag,
            op: IRBinOp::Eq,
            rhs: ok_tag,
        },
    );
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: is_ok,
            else_target: BranchTarget::to(err_block),
            then_target: BranchTarget::to(ok_block),
        },
    );
}

/// Project the `Result` variant's single payload field and acquire
/// it as an owned value (the standard match-arm pattern: the clone
/// is taken while the scrutinee is still live).
fn extract_result_payload(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    result: ValueId,
    result_symbol: &IRSymbol,
    tag: u8,
    field_type: &IRType,
) -> ValueId {
    let field = ctx.fresh_value(field_type.clone());
    ctx.cfg.append(
        block,
        IRInstruction::EnumPayloadFieldGet {
            dest: field,
            field_type: field_type.clone(),
            payload_index: 0,
            tag: IRVariantTag(tag),
            ty: result_symbol.clone(),
            value: result,
        },
    );
    materialize_owned(ctx, block, field, field_type)
}

/// Read the process's declared `priority()` off the started `state`
/// and hand the runtime its scheduling weight via
/// [`IRInstruction::SetPriority`], before the `run` loop begins.
///
/// The variant is a runtime value, so the weight is selected by a
/// compile-time, name-keyed branch diamond: the `High`/`Low` tags are
/// resolved by variant name (see [`priority_variant_tags`]) and the
/// emitted CFG maps `High → 2`, `Low → 0`, and every other variant
/// (`Normal`) → 1, matching `koja_runtime_core::Priority::from_index`.
/// No Koja `weight()` method and no dependence on the enum's source
/// variant order. The three arms converge on a join block carrying the
/// chosen `Int64` weight as a [`crate::function::BlockParam`]; that
/// join block is returned so the caller appends `run` to it.
///
/// `priority` borrows `state` (clone-on-entry, like every method
/// call — see [`super::calls`]'s borrow convention), so the caller's
/// owned `state` value flows on to `run` untouched and is released by
/// the single post-`run` drop. The `priority()` result is acquired
/// (`mark_owned`) and released in the join block (reachable on every
/// arm); `Priority`'s all-unit variants make the release a no-op, but
/// the discipline keeps the path correct if the enum ever grows heap
/// payload.
fn emit_apply_priority(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    state_symbol: &IRSymbol,
    state: ValueId,
    types: &ProcessBodyTypes,
) -> IRBlockId {
    let IRType::Enum(priority_symbol) = &types.priority else {
        panic!(
            "IR lower: `Priority` must lower to an enum, got `{:?}`",
            types.priority
        );
    };
    let priority_value = ctx.fresh_value(types.priority.clone());
    ctx.cfg.append(
        block,
        IRInstruction::Call {
            args: vec![state],
            callee: mangled_method_name(state_symbol, &[], "priority", &[]),
            dest: priority_value,
        },
    );
    ctx.mark_owned(priority_value);

    let tag = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::EnumTagGet {
            dest: tag,
            value: priority_value,
            ty: priority_symbol.clone(),
        },
    );

    let high_block = ctx.fresh_block("priority_high");
    let check_low_block = ctx.fresh_block("priority_check_low");
    let low_block = ctx.fresh_block("priority_low");
    let normal_block = ctx.fresh_block("priority_normal");
    let join_block = ctx.fresh_block("priority_set");
    let weight = ctx.declare_block_param(join_block, IRType::Int64);

    emit_priority_tag_branch(
        ctx,
        block,
        tag,
        types.priority_high_tag,
        high_block,
        check_low_block,
    );
    emit_priority_tag_branch(
        ctx,
        check_low_block,
        tag,
        types.priority_low_tag,
        low_block,
        normal_block,
    );
    branch_with_weight(ctx, high_block, join_block, 2);
    branch_with_weight(ctx, low_block, join_block, 0);
    branch_with_weight(ctx, normal_block, join_block, 1);

    ctx.cfg
        .append(join_block, IRInstruction::SetPriority { tag: weight });
    drop_discarded_temp(ctx, join_block, priority_value);
    join_block
}

/// Emit `cond_br (tag == variant_tag) then, else` as `block`'s
/// terminator — one rung of [`emit_apply_priority`]'s weight diamond,
/// modeled on [`emit_result_tag_branch`].
fn emit_priority_tag_branch(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    tag: ValueId,
    variant_tag: u8,
    then_block: IRBlockId,
    else_block: IRBlockId,
) {
    let expected = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: expected,
            value: ConstValue::Int8(variant_tag as i8),
        },
    );
    let matches = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest: matches,
            lhs: tag,
            op: IRBinOp::Eq,
            rhs: expected,
        },
    );
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: matches,
            else_target: BranchTarget::to(else_block),
            then_target: BranchTarget::to(then_block),
        },
    );
}

/// Materialize `weight` as an `Int64` const in `block` and branch to
/// `join` carrying it as the join block's `Int64` param.
fn branch_with_weight(ctx: &mut FnLowerCtx, block: IRBlockId, join: IRBlockId, weight: i64) {
    let weight_const = ctx.fresh_value(IRType::Int64);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: weight_const,
            value: ConstValue::Int64(weight),
        },
    );
    ctx.cfg.set_terminator(
        block,
        IRTerminator::Branch(BranchTarget::with_args(join, vec![weight_const])),
    );
}

/// Close a process-body arm: dispose of the `StopReason` per the
/// tail mode, release the config slot, and return.
fn finish_process_arm(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    stop_reason: ValueId,
    types: &ProcessBodyTypes,
    tail: &ProcessBodyTail,
) {
    let result = match tail {
        ProcessBodyTail::Discard => {
            drop_discarded_temp(ctx, block, stop_reason);
            let unit = ctx.fresh_value(IRType::Unit);
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest: unit,
                    value: ConstValue::Unit,
                },
            );
            unit
        }
        ProcessBodyTail::ExitCode => {
            let IRType::Enum(stop_reason_symbol) = &types.stop_reason else {
                panic!(
                    "IR lower: `StopReason` must lower to an enum, got `{:?}`",
                    types.stop_reason,
                );
            };
            let code = ctx.fresh_value(IRType::Int64);
            ctx.cfg.append(
                block,
                IRInstruction::Call {
                    args: vec![stop_reason],
                    callee: mangled_method_name(stop_reason_symbol, &[], "code", &[]),
                    dest: code,
                },
            );
            drop_discarded_temp(ctx, block, stop_reason);
            code
        }
    };
    emit_slot_drops(ctx, block);
    ctx.cfg.set_terminator(
        block,
        IRTerminator::Return {
            value: Some(result),
        },
    );
}

/// Hand-build the wrapper shim's IR body: a single `Call` into the
/// process body, discarding its result. Backends never emit this CFG
/// — the LLVM declaration is the scheduler's `void(i8*)` `ProcessFn`
/// shape, whose signature can't be expressed in IR, so its emitter
/// reads the callee out of this `Call` and synthesizes only the
/// load-config ABI adaptation around it.
fn build_wrapper_shim(
    wrapper_symbol: IRSymbol,
    kind: FunctionKind,
    body: &IRFunction,
) -> IRFunction {
    let mut ctx = FnLowerCtx::new();
    ctx.closures_mut()
        .set_enclosing_symbol(wrapper_symbol.clone());
    let entry = ctx.fresh_block("entry");

    let config_type = body
        .params
        .first()
        .map(|param| param.ty.clone())
        .expect("IR lower: process body must carry a config parameter");
    let config_id = ctx.fresh_value(config_type.clone());
    let config_local = IRLocalId::from_local_id(LocalId::new(0));

    let body_result = ctx.fresh_value(body.return_type.clone());
    ctx.cfg.append(
        entry,
        IRInstruction::Call {
            args: vec![config_id],
            callee: body.symbol.clone(),
            dest: body_result,
        },
    );

    let unit_dest = ctx.fresh_value(IRType::Unit);
    ctx.cfg.append(
        entry,
        IRInstruction::Const {
            dest: unit_dest,
            value: ConstValue::Unit,
        },
    );
    ctx.cfg.set_terminator(
        entry,
        IRTerminator::Return {
            value: Some(unit_dest),
        },
    );

    IRFunction {
        blocks: ctx.into_blocks(),
        def_location: None,
        kind,
        params: vec![IRFunctionParam {
            id: config_id,
            local_id: config_local,
            ty: config_type,
        }],
        return_type: IRType::Unit,
        symbol: wrapper_symbol,
    }
}

fn struct_symbol(ty: &IRType) -> Option<IRSymbol> {
    match ty {
        IRType::Struct(symbol) => Some(symbol.clone()),
        _ => None,
    }
}

/// Pick the [`ReceiveTag`] that matches a typed-binding's payload
/// type. Mirrors the typecheck-side admission rule
/// (`is_business_envelope` / `is_lifecycle`) — anything else means
/// the typecheck seal has been violated.
fn receive_tag_for(payload: &ResolvedType, registry: &GlobalRegistry) -> Option<ReceiveTag> {
    if is_lifecycle(payload, registry) {
        return Some(ReceiveTag::Lifecycle);
    }
    if is_business_envelope(payload, registry) {
        return Some(ReceiveTag::Business);
    }
    None
}

fn is_lifecycle(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    matches_global(ty, registry, "Lifecycle", 0)
}

fn is_business_envelope(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(head),
        type_args,
    } = ty
    else {
        return false;
    };
    if type_args.len() != 2 {
        return false;
    }
    registry
        .get(*head)
        .is_some_and(|entry| is_global_named(entry, "Pair"))
}

fn matches_global(ty: &ResolvedType, registry: &GlobalRegistry, name: &str, arity: usize) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = ty
    else {
        return false;
    };
    if type_args.len() != arity {
        return false;
    }
    registry
        .get(*id)
        .is_some_and(|entry| is_global_named(entry, name))
}

fn is_global_named(entry: &RegistryEntry, name: &str) -> bool {
    entry.identifier.is_in_global() && entry.identifier.last() == name
}
