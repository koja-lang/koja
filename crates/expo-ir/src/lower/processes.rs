//! Lowering for the process / spawn / receive surface.
//!
//! Resolves envelope types for `receive`, msg/reply pairs for spawn
//! targets, the `Ref<M, R>` mangled metadata, and the arm partitioning
//! for tagged receives. Emission consumes [`crate::resolved::processes`]
//! values to build the `expo_rt_*` calls.

use expo_ast::ast::{Expr, MatchArm, Pattern};
use expo_ast::identifier::TypeIdentifier;
use expo_ast::types::{named_generic_global, process_envelope_type};
use expo_typecheck::types::{Primitive, Type, build_substitution, mangle_name, substitute};

use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};
use crate::lower::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::naming::method_symbol_prefix;
use crate::lower::types::{resolve_name_current, resolve_type_expr};
use crate::resolved::processes::{
    ResolvedReceive, ResolvedRefType, ResolvedSpawn, ResolvedTaggedReceive,
};

/// Resolves the mailbox message type `Pair<M, Option<ReplyTo<R>>>` for
/// `receive` when compiling a `Process` impl method. Uses an exact
/// `protocol_impls` key (e.g. `Task`) or, for monomorphized impls, the
/// base type name plus substitution from the mangled self type
/// (e.g. `Task_$Int$`).
pub fn resolve_process_envelope_type(ctx: &LowerCtx<'_>, target: &str) -> Option<Type> {
    if let Some(id) = resolve_name_current(ctx, target)
        && let Some(impls) = ctx.type_ctx.protocol_impls.get(id)
        && let Some((_, args)) = impls.iter().find(|(proto, _)| proto == "Process")
    {
        let m = args.get(1)?;
        let r = args.get(2)?;
        return Some(process_envelope_type(m, r));
    }
    if let Some((base, type_args)) = try_parse_mangled_name(ctx, target) {
        let base_id = resolve_name_current(ctx, &base)?;
        let impls = ctx.type_ctx.protocol_impls.get(base_id)?;
        let (_, proto_args) = impls.iter().find(|(proto, _)| proto == "Process")?;
        let ti = ctx.type_ctx.get_type(base_id)?;
        let subst = build_substitution(&ti.type_params, &type_args);
        let m = substitute(proto_args.get(1)?, &subst);
        let r = substitute(proto_args.get(2)?, &subst);
        return Some(process_envelope_type(&m, &r));
    }
    None
}

/// Looks up the `Process<C, M, R>` protocol implementation for a type and
/// returns the concrete `(M, R)` message/reply types.
///
/// For generic processes the type parameters from the mangled name are
/// substituted into the protocol arguments. Non-generic processes use the
/// protocol args directly.
pub fn resolve_process_msg_reply(
    ctx: &LowerCtx<'_>,
    type_name: &str,
    mangled_state: &str,
) -> Result<(Type, Type), String> {
    if let Some((base, type_args)) = try_parse_mangled_name(ctx, mangled_state) {
        let base_id = resolve_name_current(ctx, &base)
            .ok_or_else(|| format!("no type `{base}` for Process impl"))?
            .clone();
        let impls = ctx
            .type_ctx
            .protocol_impls
            .get(&base_id)
            .ok_or_else(|| format!("`{base}` does not implement Process"))?;
        let (_, proto_args) = impls
            .iter()
            .find(|(proto, _)| proto == "Process")
            .ok_or_else(|| format!("`{base}` does not implement Process"))?;
        let ti = ctx
            .type_ctx
            .get_type(&base_id)
            .ok_or_else(|| format!("no type `{base}` for Process impl"))?;
        let subst = build_substitution(&ti.type_params, &type_args);
        let default = Type::Primitive(Primitive::String);
        let m = substitute(proto_args.get(1).unwrap_or(&default), &subst);
        let r = substitute(proto_args.get(2).unwrap_or(&default), &subst);
        Ok((m, r))
    } else {
        let type_id = resolve_name_current(ctx, type_name)
            .ok_or_else(|| format!("`{type_name}` does not implement Process"))?;
        let process_args = ctx
            .type_ctx
            .protocol_impls
            .get(type_id)
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

/// Computes the mangled name and Expo type for a `Ref<M, R>` struct.
pub fn resolve_ref_type(msg_type: Type, reply_type: Type) -> ResolvedRefType {
    let type_args = vec![msg_type.clone(), reply_type.clone()];
    let mangled_name =
        MonomorphizedTypeIdentifier::new(mangle_name(&TypeIdentifier::global("Ref"), &type_args));
    let expo_type = named_generic_global("Ref", type_args);
    ResolvedRefType {
        expo_type,
        mangled_name,
        msg_type,
        reply_type,
    }
}

/// Resolves the envelope type from the current function's `Process` context
/// and reports whether the receive expression has an `after` timeout arm.
pub fn resolve_receive(
    ctx: &LowerCtx<'_>,
    after_timeout: Option<&Expr>,
) -> Result<ResolvedReceive, String> {
    let envelope_type = ctx
        .fn_lower
        .process_msg_type
        .clone()
        .ok_or("receive requires a typed Process envelope; no message type found")?;

    Ok(ResolvedReceive {
        envelope_type,
        has_timeout: after_timeout.is_some(),
    })
}

/// Partitions tagged-receive arms into `IOReady`, `Lifecycle`, and business
/// buckets, and reports whether the message type contains an `IOReady`
/// member (so emission can decide whether to synthesize a default arm).
pub fn resolve_tagged_receive<'a>(
    ctx: &LowerCtx<'_>,
    arms: &'a [MatchArm],
    envelope_type: &Type,
) -> ResolvedTaggedReceive<'a> {
    let envelope_type = envelope_type.clone();

    let m_type = if let Type::Named { type_args, .. } = &envelope_type {
        type_args.first().cloned()
    } else {
        None
    };

    let m_has_io_ready = m_type.as_ref().is_some_and(|m| {
        if let Type::Union(members) = m {
            members.iter().any(|member| {
                matches!(member, Type::Named { identifier, .. } if identifier.name == "IOReady")
            })
        } else {
            false
        }
    });

    let mut business_arms: Vec<&MatchArm> = Vec::new();
    let mut io_ready_arms: Vec<&MatchArm> = Vec::new();
    let mut lifecycle_arms: Vec<&MatchArm> = Vec::new();

    for arm in arms {
        if let Pattern::TypedBinding { type_expr, .. } = &arm.pattern {
            let resolved = resolve_type_expr(ctx, type_expr);
            if matches!(&resolved, Type::Named { identifier, type_args } if identifier.name == "IOReady" && type_args.is_empty())
            {
                io_ready_arms.push(arm);
                continue;
            }
            if matches!(&resolved, Type::Named { identifier, type_args } if identifier.name == "Lifecycle" && type_args.is_empty())
            {
                lifecycle_arms.push(arm);
                continue;
            }
        }
        business_arms.push(arm);
    }

    ResolvedTaggedReceive {
        business_arms,
        envelope_type,
        io_ready_arms,
        lifecycle_arms,
        m_has_io_ready,
    }
}

/// Computes the mangled names and function identifiers for a spawn
/// expression. Non-generic spawns use the package-qualified method
/// symbol prefix so call sites match the prefix emitted at definition
/// time for user packages (e.g. `myapp.Counter_start`); generic
/// monomorphizations keep the mangled state key as the prefix.
///
/// Callers precompute `mangled_state` via the LLVM-bound
/// `spawn::resolve_mangled_state` helper because it inspects the
/// runtime config value; everything downstream of that is pure.
pub fn resolve_spawn_info(ctx: &LowerCtx<'_>, mangled_state: String) -> ResolvedSpawn {
    let generic_args = try_parse_mangled_name(ctx, &mangled_state);
    let method_prefix = if generic_args.is_some() {
        mangled_state.clone()
    } else {
        resolve_name_current(ctx, &mangled_state)
            .map(|id| method_symbol_prefix(&id.package, &id.name))
            .unwrap_or_else(|| mangled_state.clone())
    };
    ResolvedSpawn {
        generic_args,
        run_fn_name: FunctionIdentifier::new(format!("{method_prefix}_run")),
        start_fn_name: FunctionIdentifier::new(format!("{method_prefix}_start")),
        wrapper_name: FunctionIdentifier::new(format!("__spawn_{mangled_state}")),
        mangled_state: MonomorphizedTypeIdentifier::new(mangled_state),
    }
}
