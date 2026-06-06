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
use koja_typecheck::{GlobalRegistry, RegistryEntry};

use crate::function::{
    FunctionKind, IRBlockId, IRFunction, IRFunctionParam, IRInstruction, IRSymbol, IRTerminator,
    ReceiveAfter, ReceiveArm, ReceiveTag,
};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::types::{ConstValue, IRType, ValueId};

use super::arms::{lower_arm_into, lower_result_ty};
use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
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

    let wrapper_symbol = synthesize_spawn_wrapper(
        &state_symbol,
        state_ir_type.clone(),
        config_type.clone(),
        output,
    );

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

/// Lower `receive arms after timeout body end`. Each arm becomes an
/// [`IRBlockId`] whose payload local has been declared in the
/// function's entry block and whose tail branches to a synthesized
/// merge block carrying the join value as a [`crate::function::BlockParam`].
/// The host block ends with the [`IRInstruction::Receive`] dispatch
/// followed by [`IRTerminator::Unreachable`] — every reachable exit
/// goes through the arm bodies into the merge block.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_receive(
    arms: &[MatchArm],
    after_timeout: Option<&Expr>,
    after_body: &[Statement],
    result_resolution: &ResolvedType,
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    if arms.is_empty() {
        output.diagnostics.push(Diagnostic::error(
            "IR lower: `receive` reaches lower with zero arms — typecheck seal violation",
            span,
        ));
        return Err(());
    }
    let result_ty = lower_result_ty(result_resolution, registry, output);
    let merge_block = ctx.fresh_block("receive_merge");
    let result_id = ctx.declare_block_param(merge_block, result_ty.clone());

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

/// Discover the `start` and `run` method instantiations for the
/// receiver. `start` is what the wrapper invokes; `run` is the
/// receive loop the wrapper chains into when `start` succeeds.
/// Both go through [`LowerOutput::instantiations`] so the
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
    for method in ["start", "run"] {
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

/// Mint (or reuse) the [`FunctionKind::SpawnWrapper`] thunk keyed by
/// `state_symbol`. The wrapper is content-addressed off the state
/// type so distinct `spawn S.start(...)` sites for the same `S`
/// share one wrapper; distinct monomorphized state cells get
/// distinct wrappers exactly like generic structs do.
///
/// The body is intentionally minimal: a single entry block that
/// promotes `config` into a slot, emits `Const Unit`, and returns.
/// The wrapper's real semantics — deserializing config, calling
/// `start`, then chaining into `run` — live in the LLVM emit pass
/// which uses [`FunctionKind::SpawnWrapper::state`] to drive its
/// per-state shim. Keeping the IR body trivial avoids re-emitting
/// the `Result<Self, StopReason>` match shape here before the
/// runtime ABI is wired up.
fn synthesize_spawn_wrapper(
    state_symbol: &IRSymbol,
    state_type: IRType,
    config_type: IRType,
    output: &mut LowerOutput,
) -> IRSymbol {
    let wrapper_symbol = state_symbol.derived(".__spawn_wrapper");
    if !output.spawn_wrappers.insert(wrapper_symbol.clone()) {
        return wrapper_symbol;
    }
    let function = build_wrapper_body(
        wrapper_symbol.clone(),
        FunctionKind::SpawnWrapper { state: state_type },
        config_type,
    );
    output.synthesized_functions.push(function);
    wrapper_symbol
}

/// Mint the project-mode [`FunctionKind::ProcessEntryWrapper`] thunk
/// for `state_symbol`. Differs from [`synthesize_spawn_wrapper`] in
/// two ways: the resulting symbol carries the `.__entry_wrapper`
/// suffix (so it can coexist with a regular `.__spawn_wrapper` if
/// the program also spawns its own state cell), and the LLVM emit
/// pass picks up the `ProcessEntryWrapper` kind to thread the
/// `StopReason` returned from `run` through `ExitStatus.code()` and
/// store it in the module-level `__koja_exit_code` global that the
/// synthesized `main` trampoline returns from.
pub(crate) fn synthesize_process_entry_wrapper(
    state_symbol: &IRSymbol,
    state_type: IRType,
    config_type: IRType,
) -> IRFunction {
    let wrapper_symbol = state_symbol.derived(".__entry_wrapper");
    build_wrapper_body(
        wrapper_symbol,
        FunctionKind::ProcessEntryWrapper { state: state_type },
        config_type,
    )
}

fn build_wrapper_body(
    wrapper_symbol: IRSymbol,
    kind: FunctionKind,
    config_type: IRType,
) -> IRFunction {
    let mut ctx = FnLowerCtx::new();
    ctx.closures_mut()
        .set_enclosing_symbol(wrapper_symbol.clone());
    let entry = ctx.fresh_block("entry");

    let config_id = ctx.fresh_value(config_type.clone());
    let config_local = IRLocalId::from_local_id(LocalId::new(0));
    ctx.cfg.append(
        entry,
        IRInstruction::LocalDecl {
            local: config_local,
            ty: config_type.clone(),
        },
    );
    ctx.cfg.append(
        entry,
        IRInstruction::LocalWrite {
            local: config_local,
            value: config_id,
        },
    );
    ctx.mark_local_declared(config_local, config_type.clone());

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
