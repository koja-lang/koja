//! Call-site lowering: bare calls (`f(args)`) and method-style
//! calls (`recv.m(args)`). Splits out from the expression dispatcher
//! ([`super::expr::lower_expr`]) because both flavors share the
//! same registry-driven mangling / instantiation-recording shape and
//! benefit from a single emitter ([`emit_call`]).

use expo_alpha_typecheck::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry,
};
use expo_ast::ast::{Arg, Expr, ExprKind};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::package::resolved_type_to_ir_type;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::{Instantiation, substitute_resolved_type};
use crate::mangling::{mangled_function_name, mangled_type_name};
use crate::types::{IRType, ValueId};

/// Lower a `ExprKind::Call`. Seal guarantees the callee is a bare
/// `Ident` resolving to `Global(id)`; anything else panics. Generic
/// callees use the typecheck-stamped `type_args` to mangle the call
/// symbol and record a function instantiation for the worklist.
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
        (template_symbol, return_ty)
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

/// Lower `ExprKind::MethodCall`. Static dispatch (`Type.method(...)`)
/// reads the struct id off the receiver's `Resolution::Global`;
/// instance dispatch (`recv.method(...)`) lowers the receiver to a
/// `ValueId`, derives the struct id from its resolved value type,
/// and prepends the receiver to fill `params[0]` (`self`).
///
/// Methods on generic structs/enums mangle the call symbol with the
/// receiver's type-args (struct mangled prefix derived via
/// [`IRSymbol::derived`]). The receiver's struct instantiation is
/// auto-recorded by [`resolved_type_to_ir_type`]; struct
/// monomorphization in [`crate::generics::instantiate`] picks up
/// every inline `fn` on the struct decl and produces specialized
/// IRFunctions keyed at the same mangled prefix. The method's own
/// generic args (`ExprKind::MethodCall.type_args`) are checked at
/// the dispatch site in [`super::expr::lower_expr`] — generic
/// methods are a follow-up slice, so reaching this helper means
/// `type_args` is already empty.
pub(super) fn lower_method_call(
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
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
    let (_, method_entry) = registry.lookup(&method_identifier).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: method `{method_identifier}` missing from registry — \
             seal invariant violation",
        )
    });
    let signature = function_signature_from_entry(method_entry);

    let template_symbol = IRSymbol::from_identifier(&method_entry.identifier);
    let (callee_symbol, return_ty) = if receiver_type_args.is_empty() {
        let return_ty =
            resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
        (template_symbol, return_ty)
    } else {
        let receiver_arg_ir: Vec<IRType> = receiver_type_args
            .iter()
            .map(|ty| resolved_type_to_ir_type(ty, registry, &mut output.instantiations))
            .collect();
        let receiver_template = IRSymbol::from_identifier(&struct_entry.identifier);
        let mangled_struct = mangled_type_name(&receiver_template, &receiver_arg_ir);
        let mangled_method = mangled_struct.derived(&format!(".{method}"));
        let substituted_return =
            substitute_resolved_type(&signature.return_type, &receiver_type_args, struct_id);
        let return_ty =
            resolved_type_to_ir_type(&substituted_return, registry, &mut output.instantiations);
        (mangled_method, return_ty)
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
fn receiver_type_args(receiver: &Expr, dispatch: Dispatch) -> Vec<ResolvedType> {
    match dispatch {
        Dispatch::Static => Vec::new(),
        Dispatch::Instance => match &receiver.resolution {
            ResolvedType::Named { type_args, .. } => type_args.clone(),
            _ => Vec::new(),
        },
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
