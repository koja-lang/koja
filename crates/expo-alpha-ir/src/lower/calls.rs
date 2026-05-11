//! Call-site lowering: bare calls (`f(args)`) and method-style
//! calls (`recv.m(args)`). Splits out from the expression dispatcher
//! ([`super::expr::lower_expr`]) because both flavors share the
//! same registry-driven mangling / instantiation-recording shape and
//! benefit from a single emitter ([`emit_call`]).

use expo_alpha_typecheck::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry,
};
use expo_ast::ast::{Arg, Expr, ExprKind};
use expo_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::package::resolved_type_to_ir_type;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::{Instantiation, substitute_resolved_type};
use crate::local::IRLocalId;
use crate::mangling::{mangled_function_name, mangled_method_name};
use crate::types::{IRType, ValueId};

/// Lower a `ExprKind::Call`. Seal guarantees the callee is a bare
/// `Ident`; the inner [`Resolution`] dispatches: `Global(id)` lowers
/// to a direct [`IRInstruction::Call`] (with mangling for generic
/// callees), `Local(local_id)` lowers to an indirect
/// [`IRInstruction::CallClosure`] through the local's
/// `IRType::Function` slot.
pub(super) fn lower_call(
    callee: &Expr,
    args: &[Arg],
    type_args: &[ResolvedType],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let ExprKind::Ident { resolution, name } = &callee.kind else {
        panic!(
            "alpha IR lower: call callee must be a bare Ident after typecheck seal (got {:?})",
            callee.kind,
        );
    };
    if let Resolution::Local(local_id) = resolution {
        return lower_local_closure_call(
            *local_id,
            &callee.resolution,
            args,
            ctx,
            block,
            registry,
            output,
        );
    }
    let Resolution::Global(id) = resolution else {
        panic!("alpha IR lower: callee `{name}` has Unresolved resolution after typecheck seal",);
    };
    let entry = registry.get(*id).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: callee id {id} not present in the registry — \
             seal invariant violation",
        )
    });
    let signature = function_signature_from_entry(entry);
    let template_symbol = IRSymbol::from_identifier(&entry.identifier);
    let (callee_symbol, return_ty) = if type_args.is_empty() {
        let return_ty =
            resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
        if signature.impl_args.is_empty() {
            (template_symbol, return_ty)
        } else {
            // Bare static call into a sibling inside a concrete-pinned
            // impl block (`impl CPtr<UInt8>` → `Global.CPtr.strlen`
            // mangles as `Global.CPtr_$UInt8$.strlen`) — match the
            // mono-side `enqueue_member_methods` output so the call
            // resolves through the IRPackage.
            let mangled =
                impl_pinned_call_symbol(&entry.identifier, &signature.impl_args, registry, output);
            (mangled, return_ty)
        }
    } else {
        let callee_id = *id;
        let arg_ir_types: Vec<IRType> = type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
            .collect();
        let mangled = mangled_function_name(&template_symbol, &arg_ir_types);
        output.instantiations.push(Instantiation {
            template: callee_id,
            args: type_args.to_vec(),
            method_args: Vec::new(),
            owner: callee_id,
        });
        let substituted_return =
            substitute_resolved_type(&signature.return_type, type_args, callee_id);
        let return_ty =
            resolved_type_to_ir_type(&substituted_return, registry, &mut output.instantiations);
        (mangled, return_ty)
    };
    let site = CallSite {
        callee_symbol,
        return_ty,
        args,
        prepend: None,
    };
    emit_call(site, ctx, block, registry, output)
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
    impl_args: &[ResolvedType],
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRSymbol {
    let path = identifier.path();
    assert!(
        path.len() >= 2,
        "alpha IR lower: impl_args expects an owner-qualified identifier (got `{identifier}`)",
    );
    let owner = Identifier::new(identifier.package(), path[..path.len() - 1].to_vec());
    let owner_symbol = IRSymbol::from_identifier(&owner);
    let arg_types: Vec<IRType> = impl_args
        .iter()
        .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
        .collect();
    mangled_method_name(&owner_symbol, &arg_types, identifier.last(), &[])
}

/// Lower a `Resolution::Local` callee — `f(args)` where `f` is a
/// closure-typed local slot. Reads the slot through the normal
/// local-or-capture path ([`super::expr::lower_local_read`] is the
/// equivalent path; here we inline because we already hold the
/// slot's resolved type), lowers each arg in sequence, then emits
/// [`IRInstruction::CallClosure`] dispatching through the loaded
/// fat pointer.
fn lower_local_closure_call(
    local_id: LocalId,
    callee_ty: &ResolvedType,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let ResolvedType::Anonymous(AnonymousKind::Function { ret, .. }) = callee_ty else {
        panic!(
            "alpha IR lower: local closure call callee resolved to non-function type \
             ({callee_ty:?}) — typecheck seal violation",
        );
    };
    let callee_ir_type = resolved_type_to_ir_type(callee_ty, registry, &mut output.instantiations);
    let return_ty = resolved_type_to_ir_type(ret, registry, &mut output.instantiations);

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
        let (value, next) = lower_expr(&arg.value, ctx, current, registry, output)?;
        lowered_args.push(value);
        current = next;
    }

    let dest = ctx.fresh_value(return_ty.clone());
    ctx.cfg.append(
        current,
        IRInstruction::CallClosure {
            args: lowered_args,
            callee: callee_value,
            dest,
            result_ty: return_ty,
        },
    );
    Ok((dest, current))
}

/// Bundle of "what's being called" for [`lower_method_call`]: the
/// method name, positional args, and any method-level type args
/// (`recv.m::<U>(arg)`). Splitting these from the lowering-machinery
/// args (frame ctx, block, registry, output) keeps the entry point
/// at clippy's seven-arg threshold without dropping any of the
/// values.
pub(super) struct MethodCallShape<'a> {
    pub(super) method: &'a str,
    pub(super) args: &'a [Arg],
    pub(super) method_type_args: &'a [ResolvedType],
}

/// Lower `ExprKind::MethodCall`. Static dispatch (`Type.method(...)`)
/// reads the struct id off the receiver's `Resolution::Global`;
/// instance dispatch (`recv.method(...)`) lowers the receiver to a
/// `ValueId`, derives the struct id from its resolved value type,
/// and prepends the receiver to fill `params[0]` (`self`).
///
/// Methods on generic structs/enums mangle the call symbol with the
/// receiver's type-args plus any method-level type-args via
/// [`mangled_method_name`]. The receiver's struct instantiation is
/// auto-recorded by [`resolved_type_to_ir_type`]; method-level args
/// (`ExprKind::MethodCall.type_args`) drive a fresh `Instantiation`
/// pinned to the method template so [`crate::generics::instantiate`]
/// produces a specialized body.
pub(super) fn lower_method_call(
    receiver: &Expr,
    shape: MethodCallShape<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let MethodCallShape {
        method,
        args,
        method_type_args,
    } = shape;
    let dispatch = method_dispatch_kind(receiver, registry);
    let (prepend, current_block) = match dispatch {
        Dispatch::Static => (None, block),
        Dispatch::Instance => {
            let (recv_id, next_block) = lower_expr(receiver, ctx, block, registry, output)?;
            (Some(recv_id), next_block)
        }
    };
    let struct_id = receiver_struct_id(receiver, dispatch);

    let struct_entry = registry.get(struct_id).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: method call receiver id {struct_id} not present in the registry — \
             seal invariant violation",
        )
    });
    let receiver_type_args = receiver_type_args(receiver, dispatch);
    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let (method_id, method_entry) = registry.lookup(&method_identifier).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: method `{method_identifier}` missing from registry — \
             seal invariant violation",
        )
    });
    let signature = function_signature_from_entry(method_entry);

    let template_symbol = IRSymbol::from_identifier(&method_entry.identifier);
    let (callee_symbol, return_ty) = if receiver_type_args.is_empty() && method_type_args.is_empty()
    {
        let return_ty =
            resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
        (template_symbol, return_ty)
    } else {
        let receiver_arg_ir: Vec<IRType> = receiver_type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
            .collect();
        let method_arg_ir: Vec<IRType> = method_type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
            .collect();
        let receiver_template = IRSymbol::from_identifier(&struct_entry.identifier);
        let callee =
            mangled_method_name(&receiver_template, &receiver_arg_ir, method, &method_arg_ir);
        if !method_type_args.is_empty() {
            output.instantiations.push(Instantiation {
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
            resolved_type_to_ir_type(&with_method, registry, &mut output.instantiations);
        (callee, return_ty)
    };
    let site = CallSite {
        callee_symbol,
        return_ty,
        args,
        prepend,
    };
    emit_call(site, ctx, current_block, registry, output)
}

/// Pull the receiver's type-args off a method-call site. For
/// instance dispatch they live on `receiver.resolution.type_args`;
/// for static dispatch the receiver is a bare type name with no
/// type-args attached at the AST layer (alpha doesn't support
/// turbofish-style invocation), so this is currently always empty.
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

fn function_signature_from_entry(entry: &RegistryEntry) -> &FunctionSignature {
    match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!(
            "alpha IR lower: callee `{}` resolved to non-function entry ({}) — \
             typecheck seal violation",
            entry.identifier,
            other.label(),
        ),
    }
}

/// A bare `Ident` resolving to a struct or enum names the type
/// itself (static dispatch); anything else is a value receiver
/// (instance dispatch).
fn method_dispatch_kind(receiver: &Expr, registry: &GlobalRegistry) -> Dispatch {
    if let ExprKind::Ident {
        resolution: Resolution::Global(id),
        ..
    } = &receiver.kind
        && let Some(entry) = registry.get(*id)
        && matches!(entry.kind, GlobalKind::Enum(_) | GlobalKind::Struct(_))
    {
        return Dispatch::Static;
    }
    Dispatch::Instance
}

/// Pull the struct's `GlobalRegistryId` off a method-call receiver.
/// Static reads from `receiver.kind`'s `Resolution::Global`; instance
/// reads from `receiver.resolution`'s resolved value type.
fn receiver_struct_id(receiver: &Expr, dispatch: Dispatch) -> GlobalRegistryId {
    match dispatch {
        Dispatch::Static => {
            let ExprKind::Ident {
                resolution, name, ..
            } = &receiver.kind
            else {
                panic!(
                    "alpha IR lower: static method call receiver must be a bare Ident after \
                     typecheck seal (got {:?})",
                    receiver.kind,
                );
            };
            let Resolution::Global(struct_id) = resolution else {
                panic!(
                    "alpha IR lower: static method call receiver `{name}` has Unresolved \
                     resolution after typecheck seal",
                );
            };
            *struct_id
        }
        Dispatch::Instance => {
            let resolution = &receiver.resolution;
            let ResolvedType::Named {
                resolution: Resolution::Global(struct_id),
                ..
            } = resolution
            else {
                panic!(
                    "alpha IR lower: instance method receiver resolved to non-Global type \
                     ({resolution:?}) — typecheck seal must have rejected this",
                );
            };
            *struct_id
        }
    }
}

/// Per-call inputs to [`emit_call`] — bundled so the emitter
/// signature stays narrow regardless of how many derived fields the
/// caller computed. `prepend` is the receiver [`ValueId`] for
/// instance dispatch (filling `params[0]` / `self`), `None` for
/// bare calls and static method dispatch. `callee_symbol` is
/// already mangled if the callee is a generic instantiation;
/// `return_ty` is already substituted.
struct CallSite<'a> {
    callee_symbol: IRSymbol,
    return_ty: IRType,
    args: &'a [Arg],
    prepend: Option<ValueId>,
}

/// Shared tail of [`lower_call`] / [`lower_method_call`]: lower
/// each arg in sequence, then emit the [`IRInstruction::Call`] in
/// the final block.
fn emit_call(
    site: CallSite<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let CallSite {
        callee_symbol,
        return_ty,
        args,
        prepend,
    } = site;
    let mut lowered_args = Vec::with_capacity(args.len() + usize::from(prepend.is_some()));
    if let Some(receiver) = prepend {
        lowered_args.push(receiver);
    }
    let mut current = block;
    for arg in args {
        let (value, next) = lower_expr(&arg.value, ctx, current, registry, output)?;
        lowered_args.push(value);
        current = next;
    }

    let dest = ctx.fresh_value(return_ty);
    ctx.cfg.append(
        current,
        IRInstruction::Call {
            dest,
            callee: callee_symbol,
            args: lowered_args,
        },
    );
    Ok((dest, current))
}
