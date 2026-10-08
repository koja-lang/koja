//! Call-site lowering: bare calls (`f(args)`) and method-style
//! calls (`recv.m(args)`). Splits out from the expression dispatcher
//! ([`super::expr::lower_expr`]) because both flavors share the
//! same registry-driven mangling / instantiation-recording shape and
//! benefit from a single emitter ([`emit_call`]).

use koja_ast::ast::{Arg, Expr, ExprKind, Name};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};
use koja_typecheck::{Dispatch, GlobalKind, GlobalRegistry, peel_alias};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::equality::lower_equality_call;
use super::expr::{emit_string_const, lower_expr};
use super::ownership::{drop_discarded_temp, materialize_boundary_copy};
use super::package::resolved_type_to_ir_type;
use super::tuples::lower_tuple_conformance_call;
use super::unions::lower_union_conformance_call;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::{Instantiation, substitute_resolved_type};
use crate::local::IRLocalId;
use crate::mangling::{mangled_function_name, mangled_method_name, source_function_symbol};
use crate::types::{ConstValue, IRType, ValueId};

/// Lower a `ExprKind::Call`. Seal guarantees the callee is one of:
/// - Bare `Ident { Global(id) }`: direct [`IRInstruction::Call`]
///   (mangling applied for generic callees).
/// - Bare `Ident { Local(local_id) }`: indirect
///   [`IRInstruction::CallClosure`] through the local's
///   `IRType::Function` slot.
/// - `FieldAccess` with an `AnonymousKind::Function` resolution
///   (produced by the field-as-callable rewrite in typecheck):
///   lower the callee expression to a fn-typed value, then emit
///   [`IRInstruction::CallClosure`].
pub(super) fn lower_call(
    callee: &Expr,
    args: &[Arg],
    type_args: &[ResolvedType],
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    if matches!(callee.kind, ExprKind::FieldAccess { .. }) {
        return lower_closure_expr_call(callee, args, ctx, block);
    }
    let ExprKind::Ident { resolution, name } = &callee.kind else {
        panic!(
            "IR lower: call callee must be a bare Ident or FieldAccess after typecheck seal \
             (got {:?})",
            callee.kind,
        );
    };
    if let Resolution::Local(local_id) = resolution {
        return lower_local_closure_call(*local_id, &callee.resolution, args, ctx, block);
    }
    let Resolution::Global(id) = resolution else {
        panic!("IR lower: callee `{name}` has Unresolved resolution after typecheck seal",);
    };
    let registry = ctx.registry;
    let entry = registry.get(*id).unwrap_or_else(|| {
        panic!(
            "IR lower: callee id {id} not present in the registry \
             (seal invariant violation)",
        )
    });
    let signature = entry.expect_function_signature();
    let definition = entry.expect_function_definition();
    let template_symbol = source_function_symbol(&entry.identifier, definition.arity);
    let (callee_symbol, return_ty) = if type_args.is_empty() {
        let return_ty = resolved_type_to_ir_type(
            &signature.return_type,
            registry,
            &mut ctx.output.instantiations,
        );
        if signature.impl_args.is_empty() {
            (template_symbol, return_ty)
        } else {
            // Bare static call into a sibling inside a concrete-pinned
            // impl block (inside `impl CPtr<UInt8>`, `Global.CPtr.strlen`
            // mangles as `Global.CPtr_$UInt8$.strlen`). Match the
            // mono-side `enqueue_member_methods` output so the call
            // resolves through the IRPackage.
            let mangled = impl_pinned_call_symbol(
                &entry.identifier,
                signature.params.len(),
                &signature.impl_args,
                registry,
                ctx.output,
            );
            (mangled, return_ty)
        }
    } else {
        let callee_id = *id;
        let arg_ir_types: Vec<IRType> = type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut ctx.output.instantiations))
            .collect();
        let mangled = mangled_function_name(&template_symbol, &arg_ir_types);
        ctx.output.instantiations.push(Instantiation {
            template: callee_id,
            args: type_args.to_vec(),
            method_args: Vec::new(),
            owner: callee_id,
        });
        let substituted_return =
            substitute_resolved_type(&signature.return_type, type_args, callee_id);
        let return_ty = resolved_type_to_ir_type(
            &substituted_return,
            registry,
            &mut ctx.output.instantiations,
        );
        (mangled, return_ty)
    };
    let site = CallSite {
        callee_symbol,
        return_ty,
        args,
        prepend: None,
        transfer_arg: None,
    };
    emit_call(site, ctx, block)
}

/// Mangle a bare static call into a sibling inside a concrete-pinned
/// `impl Type<Args>` block. Splits the callee identifier into its
/// owner (everything but the last segment) and the method name,
/// translates `impl_args` to IR types, and rebuilds the symbol via
/// [`mangled_method_name`] so the call resolves to the same shape
/// `enqueue_member_methods` produces from the receiver side
/// (`Type_$Args$.method`). `impl_args` is guaranteed concrete by
/// the typecheck-side `concrete_impl_args` filter.
fn impl_pinned_call_symbol(
    identifier: &Identifier,
    arity: usize,
    impl_args: &[ResolvedType],
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRSymbol {
    let path = identifier.path();
    assert!(
        path.len() >= 2,
        "IR lower: impl_args expects an owner-qualified identifier (got `{identifier}`)",
    );
    let owner = Identifier::new(identifier.package(), path[..path.len() - 1].to_vec());
    let owner_symbol = IRSymbol::from_identifier(&owner);
    let arg_types: Vec<IRType> = impl_args
        .iter()
        .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
        .collect();
    mangled_method_name(&owner_symbol, &arg_types, identifier.last(), arity, &[])
}

/// Lower a `Resolution::Local` callee, `f(args)` where `f` is a
/// closure-typed local slot. Reads the slot through the normal
/// local-or-capture path (`expr::lower_local_read` is the
/// equivalent path, but this helper inlines it because it already
/// holds the slot's resolved type), lowers each arg in sequence, then
/// emits [`IRInstruction::CallClosure`] dispatching through the loaded
/// fat pointer.
fn lower_local_closure_call(
    local_id: LocalId,
    callee_ty: &ResolvedType,
    args: &[Arg],
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let ResolvedType::Anonymous(AnonymousKind::Function { ret, .. }) = callee_ty else {
        panic!(
            "IR lower: local closure call callee resolved to non-function type \
             ({callee_ty:?}), typecheck seal violation",
        );
    };
    let callee_ir_type =
        resolved_type_to_ir_type(callee_ty, ctx.registry, &mut ctx.output.instantiations);
    let param_types = closure_param_types(&callee_ir_type);
    let return_ty = resolved_type_to_ir_type(ret, ctx.registry, &mut ctx.output.instantiations);

    let ir_local = IRLocalId::from_local_id(local_id);
    let callee_value = if let Some(capture_index) = ctx.closures().capture_index(local_id) {
        let dest = ctx.fresh_value(callee_ir_type.clone());
        ctx.cfg.append(
            block,
            IRInstruction::LoadCapture {
                capture_index,
                dest,
                ty: callee_ir_type.clone(),
            },
        );
        dest
    } else {
        let dest = ctx.fresh_value(callee_ir_type.clone());
        ctx.cfg.append(
            block,
            IRInstruction::LocalRead {
                dest,
                local: ir_local,
                ty: callee_ir_type.clone(),
            },
        );
        dest
    };

    let mut lowered_args = Vec::with_capacity(args.len());
    let mut current = block;
    for arg in args {
        let (value, next) = lower_expr(&arg.value, ctx, current)?;
        lowered_args.push(value);
        current = next;
    }

    let dest = ctx.fresh_value(return_ty.clone());
    ctx.cfg.append(
        current,
        IRInstruction::CallClosure {
            args: lowered_args.clone(),
            callee: callee_value,
            dest,
            param_types,
            result_ty: return_ty,
        },
    );
    ctx.mark_owned(dest);
    // The closure clones each param into its slot and only borrows its
    // env to dispatch, so owned-temp args (the callee here is a slot
    // read, never owned) are dead after the call.
    release_call_temps(ctx, current, &lowered_args, None);
    Ok((dest, current))
}

/// Parameter types of a closure callee's [`IRType::Function`].
fn closure_param_types(callee_ir_type: &IRType) -> Vec<IRType> {
    let IRType::Function { params, .. } = callee_ir_type else {
        panic!("IR lower: closure callee lowered to non-function IRType ({callee_ir_type:?})");
    };
    params.clone()
}

/// Lower a call whose callee is a non-Ident expression of fn type
/// (today, a `FieldAccess` produced by the field-as-callable
/// rewrite). Lowers the callee to a fn-typed value, lowers args in
/// order, then emits [`IRInstruction::CallClosure`].
fn lower_closure_expr_call(
    callee: &Expr,
    args: &[Arg],
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let ResolvedType::Anonymous(AnonymousKind::Function { ret, .. }) = &callee.resolution else {
        panic!(
            "IR lower: closure-expr call callee resolved to non-function type ({:?}), \
             typecheck seal violation",
            callee.resolution,
        );
    };
    let callee_ir_type = resolved_type_to_ir_type(
        &callee.resolution,
        ctx.registry,
        &mut ctx.output.instantiations,
    );
    let param_types = closure_param_types(&callee_ir_type);
    let return_ty = resolved_type_to_ir_type(ret, ctx.registry, &mut ctx.output.instantiations);

    let (callee_value, mut current) = lower_expr(callee, ctx, block)?;
    let mut lowered_args = Vec::with_capacity(args.len());
    for arg in args {
        let (value, next) = lower_expr(&arg.value, ctx, current)?;
        lowered_args.push(value);
        current = next;
    }

    let dest = ctx.fresh_value(return_ty.clone());
    ctx.cfg.append(
        current,
        IRInstruction::CallClosure {
            args: lowered_args.clone(),
            callee: callee_value,
            dest,
            param_types,
            result_ty: return_ty,
        },
    );
    ctx.mark_owned(dest);
    // Release owned-temp args plus the callee fat pointer itself when it
    // was produced fresh (e.g. a field access that returns a closure),
    // since the call only borrows the env to dispatch.
    release_call_temps(ctx, current, &lowered_args, None);
    drop_discarded_temp(ctx, current, callee_value);
    Ok((dest, current))
}

pub(super) fn synthesized_method_target(
    registry: &GlobalRegistry,
    owner_id: GlobalRegistryId,
    method: &str,
    arity: usize,
) -> Resolution {
    let owner = registry
        .get(owner_id)
        .unwrap_or_else(|| panic!("IR lower: synthesized method owner id `{owner_id}` is missing"));
    let identifier =
        Identifier::member(owner.identifier.package(), owner.identifier.path(), method);
    let (id, _) = registry
        .lookup_function(&identifier, arity)
        .unwrap_or_else(|| {
            panic!("IR lower: synthesized method `{identifier}/{arity}` is missing")
        });
    Resolution::Global(id)
}

/// Lower `ExprKind::MethodCall`. Static dispatch (`Type.method(...)`)
/// reads the struct id off the receiver's `Resolution::Global`.
/// Instance dispatch (`recv.method(...)`) lowers the receiver to a
/// `ValueId`, derives the struct id from its resolved value type,
/// and prepends the receiver to fill `params[0]` (`self`).
///
/// Methods on generic structs/enums mangle the call symbol with the
/// receiver's type-args plus any method-level type-args via
/// [`mangled_method_name`]. The receiver's struct instantiation is
/// auto-recorded by [`resolved_type_to_ir_type`], and method-level args
/// (`ExprKind::MethodCall.type_args`) drive a fresh `Instantiation`
/// pinned to the method template so [`crate::generics::instantiate`]
/// produces a specialized body.
///
/// `method_type_args` are the method-level type args of a
/// `recv.m::<U>(arg)` call.
pub(super) fn lower_method_call(
    receiver: &Expr,
    method: &Name,
    args: &[Arg],
    method_type_args: &[ResolvedType],
    target: Resolution,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let method = method.as_str();
    let registry = ctx.registry;
    // Structural receivers have no nominal method to call, so their
    // universal-protocol functions expand inline. Every route lands
    // here after monomorphization: `f == g`, a derived body comparing
    // or formatting a function / union / tuple field, a stdlib
    // conditional impl at `T = fn`, and bounded `a.equals?(b)` on a
    // type parameter.
    let structural_receiver = peel_alias(&receiver.resolution, registry);
    match &structural_receiver {
        ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
            return lower_tuple_conformance_call(receiver, method, args, ctx, block);
        }
        ResolvedType::Union(_) if method == "equals?" => {
            return lower_equality_call(receiver, args, ctx, block);
        }
        ResolvedType::Union(_) => {
            return lower_union_conformance_call(receiver, method, args, ctx, block);
        }
        ResolvedType::Anonymous(AnonymousKind::Function { .. }) if method == "equals?" => {
            return lower_equality_call(receiver, args, ctx, block);
        }
        // Function values have no `format` of their own and render as
        // `"..."`, matching what `derive_debug` emits for function fields.
        ResolvedType::Anonymous(AnonymousKind::Function { .. }) => {
            return lower_debug_family(method, receiver, ctx, block, |_, ctx, block| {
                (emit_string_const("...".to_string(), ctx, block), block)
            });
        }
        _ => {}
    }
    let dispatch = method_dispatch_kind(receiver, registry);
    let (prepend, current_block) = match dispatch {
        Dispatch::Static => (None, block),
        Dispatch::Instance => {
            let (recv_id, next_block) = lower_expr(receiver, ctx, block)?;
            (Some(recv_id), next_block)
        }
    };
    let struct_id =
        canonical_receiver_id(receiver_struct_id(receiver, dispatch, registry), registry);

    let struct_entry = registry.get(struct_id).unwrap_or_else(|| {
        panic!(
            "IR lower: method call receiver id {struct_id} not present in the registry \
             (seal invariant violation)",
        )
    });
    let receiver_type_args = receiver_type_args(receiver, dispatch);
    let method_id = if let Resolution::Global(method_id) = target {
        method_id
    } else {
        let identifier = Identifier::member(
            struct_entry.identifier.package(),
            struct_entry.identifier.path(),
            method,
        );
        let arity = args.len() + usize::from(dispatch == Dispatch::Instance);
        registry
            .lookup_function(&identifier, arity)
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("IR lower: bounded method `{identifier}/{arity}` is missing"))
    };
    let method_entry = registry.get(method_id).unwrap_or_else(|| {
        panic!("IR lower: method target id `{method_id}` is missing from the registry")
    });
    let definition = method_entry.expect_function_definition();
    let signature = method_entry.expect_function_signature();

    let (callee_symbol, return_ty) = if receiver_type_args.is_empty() && method_type_args.is_empty()
    {
        let return_ty = resolved_type_to_ir_type(
            &signature.return_type,
            registry,
            &mut ctx.output.instantiations,
        );
        (
            mangled_method_name(
                &IRSymbol::from_identifier(&struct_entry.identifier),
                &[],
                method,
                definition.arity,
                &[],
            ),
            return_ty,
        )
    } else {
        let receiver_arg_ir: Vec<IRType> = receiver_type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut ctx.output.instantiations))
            .collect();
        let method_arg_ir: Vec<IRType> = method_type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut ctx.output.instantiations))
            .collect();
        let receiver_template = IRSymbol::from_identifier(&struct_entry.identifier);
        let callee = mangled_method_name(
            &receiver_template,
            &receiver_arg_ir,
            method,
            signature.params.len(),
            &method_arg_ir,
        );
        // Enqueue the specific method the call targets so the mono
        // worklist sees the call's `(method_id, receiver_args,
        // method_args)` triple. Static dispatch on a generic type
        // (`Task.async(...)`) never lowers the receiver expression,
        // so without this push the call symbol mangled above would
        // have no matching `IRFunction` and `seal_program_calls`
        // would panic.
        if !receiver_type_args.is_empty() || !method_type_args.is_empty() {
            ctx.output.instantiations.push(Instantiation {
                template: method_id,
                args: receiver_type_args.clone(),
                method_args: method_type_args.to_vec(),
                owner: struct_id,
            });
        }
        let with_receiver =
            substitute_resolved_type(&signature.return_type, &receiver_type_args, struct_id);
        let with_method = substitute_resolved_type(&with_receiver, method_type_args, method_id);
        let return_ty =
            resolved_type_to_ir_type(&with_method, registry, &mut ctx.output.instantiations);
        (callee, return_ty)
    };
    let site = CallSite {
        callee_symbol,
        return_ty,
        args,
        prepend,
        transfer_arg: runtime_transfer_arg(&struct_entry.identifier, method),
    };
    emit_call(site, ctx, current_block)
}

/// The index of the surface argument a `(receiver, method)` pair hands
/// to the runtime, or `None` for an ordinary call. The message / reply
/// send intrinsics (`Ref.cast` / `Ref.call` / `Ref.send_after` and
/// `ReplyTo.send`) copy their first argument (the message `M` or reply
/// `R`) across a process boundary, so the receiving process must hold
/// a physically independent value. The `TraceRuntime` span intrinsics
/// (`span_open` / `export_push` at index 0, `span_put` after the handle
/// at index 1) hand a span record to the runtime's open span stack or
/// export queue, which owns it from then on. `LogRuntime.configure`
/// hands the log configuration (index 0) to the calling process's
/// slot the same way. See [`CallSite::transfer_arg`].
fn runtime_transfer_arg(receiver: &Identifier, method: &str) -> Option<usize> {
    if receiver.package() != "Global" {
        return None;
    }
    match receiver.path() {
        [name] if name == "Ref" && matches!(method, "cast" | "call" | "send_after") => Some(0),
        [name] if name == "ReplyTo" && method == "send" => Some(0),
        [name] if name == "LogRuntime" && method == "configure" => Some(0),
        [name] if name == "TraceRuntime" => match method {
            "export_push" | "span_open" => Some(0),
            "span_put" => Some(1),
            _ => None,
        },
        _ => None,
    }
}

/// Pull the receiver's type-args off a method-call site. For
/// instance dispatch they live on `receiver.resolution.type_args`.
/// For static dispatch the receiver is a bare type name with no
/// type-args attached at the AST layer (the pipeline does not support
/// turbofish-style invocation), so this is always empty.
fn receiver_type_args(receiver: &Expr, _dispatch: Dispatch) -> Vec<ResolvedType> {
    // Static dispatch on a generic struct (`List.new()` against
    // `List<Int>`) stitches the inferred type-args back onto
    // `receiver.resolution` during typecheck, so the same shape
    // covers both arms.
    match &receiver.resolution {
        ResolvedType::Named { type_args, .. } => type_args.clone(),
        _ => Vec::new(),
    }
}

/// Callee symbol + IR return type for `receiver_ty.method()` where
/// `receiver_ty` is a resolved nominal type. Mirrors the symbol and
/// instantiation logic of [`lower_method_call`] for call sites the
/// lowering itself synthesizes (tuple conformance expansion), where
/// there is no receiver `Expr` to consult.
pub(super) fn conformance_method_symbol(
    receiver_ty: &ResolvedType,
    method: &str,
    arity: usize,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (IRSymbol, IRType) {
    let structural_receiver = peel_alias(receiver_ty, registry);
    let ResolvedType::Named {
        resolution: Resolution::Global(struct_id),
        type_args,
        ..
    } = &structural_receiver
    else {
        panic!(
            "IR lower: conformance call receiver `{receiver_ty:?}` is not a nominal type \
             (typecheck resolve invariant violation)",
        );
    };
    let struct_id = canonical_receiver_id(*struct_id, registry);
    let struct_entry = registry.get(struct_id).unwrap_or_else(|| {
        panic!(
            "IR lower: conformance call receiver id {struct_id} not present in the \
             registry (seal invariant violation)",
        )
    });
    let method_identifier = Identifier::member(
        struct_entry.identifier.package(),
        struct_entry.identifier.path(),
        method,
    );
    let (method_id, method_entry) = registry
        .lookup_function(&method_identifier, arity)
        .unwrap_or_else(|| {
            panic!(
                "IR lower: conformance method `{method_identifier}/{arity}` missing from registry \
             (typecheck must have validated the call)",
            )
        });
    let signature = method_entry.expect_function_signature();
    if type_args.is_empty() {
        let return_ty =
            resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
        return (
            source_function_symbol(
                &method_entry.identifier,
                method_entry.expect_function_definition().arity,
            ),
            return_ty,
        );
    }
    let receiver_arg_ir: Vec<IRType> = type_args
        .iter()
        .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
        .collect();
    let receiver_template = IRSymbol::from_identifier(&struct_entry.identifier);
    let callee = mangled_method_name(
        &receiver_template,
        &receiver_arg_ir,
        method,
        signature.params.len(),
        &[],
    );
    output.instantiations.push(Instantiation {
        template: method_id,
        args: type_args.clone(),
        method_args: Vec::new(),
        owner: struct_id,
    });
    let substituted = substitute_resolved_type(&signature.return_type, type_args, struct_id);
    let return_ty = resolved_type_to_ir_type(&substituted, registry, &mut output.instantiations);
    (callee, return_ty)
}

/// A bare `Ident` resolving to a struct, enum, or protocol names the
/// type itself (static dispatch). Anything else is a value receiver
/// (instance dispatch).
fn method_dispatch_kind(receiver: &Expr, registry: &GlobalRegistry) -> Dispatch {
    if let ExprKind::Ident {
        resolution: Resolution::Global(id),
        ..
    } = &receiver.kind
        && let Some(entry) = registry.get(*id)
        && matches!(
            entry.kind,
            GlobalKind::Builtin(_)
                | GlobalKind::Enum(_)
                | GlobalKind::Protocol(_)
                | GlobalKind::Struct(_)
        )
    {
        return Dispatch::Static;
    }
    Dispatch::Instance
}

/// Collapse `Global.Int64` / `Global.Float64` onto `Global.Int` /
/// `Global.Float` for method lookup. The typecheck pass treats these
/// pairs as alias-equivalent (see
/// the typecheck `types_equivalent` helper).
/// Because `Int` and `Float` are not unions over their sized
/// variants, methods registered on the unsized canonical (e.g.
/// `Debug.format`, `Equality.equals?`, `Hash.hash`) need to be reachable
/// through an `Int64`-resolved receiver too. Other primitive widths
/// (`Int8`, `UInt32`, etc.) keep their own ids, since they are distinct
/// types in the alias rule, not collapsed.
fn canonical_receiver_id(id: GlobalRegistryId, registry: &GlobalRegistry) -> GlobalRegistryId {
    let Some(entry) = registry.get(id) else {
        return id;
    };
    if entry.identifier.package() != "Global" {
        return id;
    }
    let path = entry.identifier.path();
    if path.len() != 1 {
        return id;
    }
    let canonical = match path[0].as_str() {
        "Float64" => "Float",
        "Int64" => "Int",
        _ => return id,
    };
    let canonical_ident = Identifier::single("Global", canonical);
    registry
        .lookup(&canonical_ident)
        .map(|(id, _)| id)
        .unwrap_or(id)
}

/// Pull the struct's `GlobalRegistryId` off a method-call receiver.
/// Static reads from `receiver.kind`'s `Resolution::Global`, instance
/// reads from `receiver.resolution`'s resolved value type.
fn receiver_struct_id(
    receiver: &Expr,
    dispatch: Dispatch,
    registry: &GlobalRegistry,
) -> GlobalRegistryId {
    match dispatch {
        Dispatch::Static => {
            let ExprKind::Ident {
                resolution, name, ..
            } = &receiver.kind
            else {
                panic!(
                    "IR lower: static method call receiver must be a bare Ident after \
                     typecheck seal (got {:?})",
                    receiver.kind,
                );
            };
            let Resolution::Global(struct_id) = resolution else {
                panic!(
                    "IR lower: static method call receiver `{name}` has Unresolved \
                     resolution after typecheck seal",
                );
            };
            *struct_id
        }
        Dispatch::Instance => {
            let resolution = &receiver.resolution;
            let structural_receiver = peel_alias(resolution, registry);
            let ResolvedType::Named {
                resolution: Resolution::Global(struct_id),
                ..
            } = structural_receiver
            else {
                panic!(
                    "IR lower: instance method receiver resolved to non-Global type \
                     ({resolution:?}), typecheck seal must have rejected this",
                );
            };
            struct_id
        }
    }
}

/// Per-call inputs to [`emit_call`]. `prepend` is the receiver
/// [`ValueId`] for instance dispatch, `None` for bare calls and
/// static method dispatch. `callee_symbol` is already mangled if the
/// callee is a generic instantiation, and `return_ty` is already
/// substituted.
struct CallSite<'a> {
    callee_symbol: IRSymbol,
    return_ty: IRType,
    args: &'a [Arg],
    prepend: Option<ValueId>,
    /// When set, the surface argument at this index is *deep-copied*
    /// ([`IRInstruction::DeepCopy`]) before the call. The message /
    /// reply send intrinsics (`Ref.cast` / `Ref.call` /
    /// `Ref.send_after` / `ReplyTo.send`) hand the payload to another
    /// process, and Koja's rc bookkeeping is unsynchronized, so the
    /// transported value must share no heap storage with the sender.
    /// The `TraceRuntime` span intrinsics hand a record to the runtime,
    /// which frees it through drop glue rather than the caller's slot.
    /// The caller's own value keeps its normal slot lifecycle (an owned
    /// temp source is released right after the copy). The copy is moved
    /// into the runtime and reclaimed there (delivered to the receiver,
    /// handed back through a `TraceRuntime` take / close / pop, or
    /// released via the drop glue on discard).
    transfer_arg: Option<usize>,
}

/// Shared tail of [`lower_call`] / [`lower_method_call`]: lower
/// each arg in sequence, then emit the [`IRInstruction::Call`] in
/// the final block. Under value semantics every argument is passed
/// by value, and the caller retains its slots and frees them at scope
/// exit.
fn emit_call(
    site: CallSite<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let CallSite {
        callee_symbol,
        return_ty,
        args,
        prepend,
        transfer_arg,
    } = site;
    let mut lowered_args = Vec::with_capacity(args.len() + usize::from(prepend.is_some()));
    if let Some(receiver) = prepend {
        lowered_args.push(receiver);
    }
    let mut current = block;
    let mut transferred: Option<ValueId> = None;
    for (index, arg) in args.iter().enumerate() {
        let (mut value, next) = lower_expr(&arg.value, ctx, current)?;
        current = next;
        if transfer_arg == Some(index) {
            let ty = ctx.type_of(value);
            let copied = materialize_boundary_copy(ctx, current, value, &ty);
            if copied != value {
                // The copy is independent, so an owned temp source
                // (e.g. `ref.cast(build_msg())`) is dead here.
                drop_discarded_temp(ctx, current, value);
            }
            transferred = Some(copied);
            value = copied;
        }
        lowered_args.push(value);
    }

    let dest = ctx.fresh_value(return_ty);
    ctx.cfg.append(
        current,
        IRInstruction::Call {
            dest,
            callee: callee_symbol,
            args: lowered_args.clone(),
        },
    );
    ctx.mark_owned(dest);
    release_call_temps(ctx, current, &lowered_args, transferred);
    Ok((dest, current))
}

/// Release every owned heap temporary handed to a call once the callee
/// has taken its own copy. Callees follow the borrow convention (named
/// fns / closures clone each param into its slot ([`super::ownership::promote_param`]),
/// collection intrinsics copy-on-write `self` and acquire stored args),
/// so an owned-temp argument (or fluent-chain receiver) the lowerer
/// passed is dead after the call and would otherwise leak.
/// `transferred` names a value moved into a transport (the message /
/// reply send payload) that the runtime now owns, so it is skipped.
/// Borrowed values (slot/field reads) and non-heap values are no-ops in
/// [`drop_discarded_temp`].
fn release_call_temps(
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
    values: &[ValueId],
    transferred: Option<ValueId>,
) {
    for &value in values {
        if Some(value) == transferred {
            continue;
        }
        drop_discarded_temp(ctx, block, value);
    }
}

/// The `Debug` protocol's three entry points, which share one
/// `format` expansion and differ only in what they do with it.
#[derive(Clone, Copy)]
enum DebugMethod {
    Format,
    Inspect,
    Print,
}

impl DebugMethod {
    fn from_name(method: &str) -> Option<Self> {
        match method {
            "format" => Some(Self::Format),
            "inspect" => Some(Self::Inspect),
            "print" => Some(Self::Print),
            _ => None,
        }
    }
}

/// Lower `receiver.format()` / `print()` / `inspect()` for a receiver
/// whose rendering expands inline. `format` builds the `String` from
/// the lowered receiver value (owned when heap-managed). Behavior
/// matches derived `Debug`:
///
/// * `format(self) -> String` returns the rendering.
/// * `print(self) -> Unit` writes it via `IO.puts` and returns `Unit`.
/// * `inspect(self) -> Self` writes it and returns the receiver
///   unchanged, so call chains preserve the value.
pub(super) fn lower_debug_family(
    method: &str,
    receiver: &Expr,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
    format: impl FnOnce(ValueId, &mut FnLowerCtx<'_>, IRBlockId) -> (ValueId, IRBlockId),
) -> Result<(ValueId, IRBlockId), ()> {
    let method = DebugMethod::from_name(method).unwrap_or_else(|| {
        panic!(
            "IR lower: `{method}` is not a Debug function (typecheck resolve invariant violation)"
        )
    });
    let (receiver_value, current) = lower_expr(receiver, ctx, block)?;
    let (formatted, mut current) = format(receiver_value, ctx, current);
    match method {
        DebugMethod::Format => {
            drop_discarded_temp(ctx, current, receiver_value);
            Ok((formatted, current))
        }
        DebugMethod::Print => {
            current = emit_io_puts(formatted, ctx, current);
            drop_discarded_temp(ctx, current, formatted);
            drop_discarded_temp(ctx, current, receiver_value);
            let unit = ctx.fresh_value(IRType::Unit);
            ctx.cfg.append(
                current,
                IRInstruction::Const {
                    dest: unit,
                    value: ConstValue::Unit,
                },
            );
            Ok((unit, current))
        }
        DebugMethod::Inspect => {
            current = emit_io_puts(formatted, ctx, current);
            drop_discarded_temp(ctx, current, formatted);
            Ok((receiver_value, current))
        }
    }
}

/// Emit `Global.IO.puts(<message>)` and return the block the call
/// landed in. The callee symbol matches the one stamped by lift for
/// the `IO.puts` function in `koja/lib/global/src/io.koja`, so the
/// regular function registration in `lower_function_inner` resolves
/// it at link time.
pub(super) fn emit_io_puts(
    message: ValueId,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> IRBlockId {
    let callee = source_function_symbol(
        &Identifier::new("Global", vec!["IO".to_string(), "puts".to_string()]),
        1,
    );
    let dest = ctx.fresh_value(IRType::Unit);
    ctx.cfg.append(
        block,
        IRInstruction::Call {
            dest,
            callee,
            args: vec![message],
        },
    );
    block
}
