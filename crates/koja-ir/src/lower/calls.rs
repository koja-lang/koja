//! Call-site lowering: bare calls (`f(args)`) and method-style
//! calls (`recv.m(args)`). Splits out from the expression dispatcher
//! ([`super::expr::lower_expr`]) because both flavors share the
//! same registry-driven mangling / instantiation-recording shape and
//! benefit from a single emitter ([`emit_call`]).

use koja_ast::ast::{Arg, Expr, ExprKind};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};
use koja_typecheck::{Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::package::resolved_type_to_ir_type;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::{Instantiation, substitute_resolved_type};
use crate::local::IRLocalId;
use crate::mangling::{mangled_function_name, mangled_method_name};
use crate::types::{ConstValue, IRType, ValueId};

/// Lower a `ExprKind::Call`. Seal guarantees the callee is one of:
/// - Bare `Ident { Global(id) }` — direct [`IRInstruction::Call`]
///   (mangling applied for generic callees).
/// - Bare `Ident { Local(local_id) }` — indirect
///   [`IRInstruction::CallClosure`] through the local's
///   `IRType::Function` slot.
/// - `FieldAccess` with an `AnonymousKind::Function` resolution
///   (produced by the field-as-callable rewrite in typecheck)
///   — lower the callee expression to a fn-typed value, then emit
///   [`IRInstruction::CallClosure`].
pub(super) fn lower_call(
    callee: &Expr,
    args: &[Arg],
    type_args: &[ResolvedType],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    if matches!(callee.kind, ExprKind::FieldAccess { .. }) {
        return lower_closure_expr_call(callee, args, ctx, block, registry, output);
    }
    let ExprKind::Ident { resolution, name } = &callee.kind else {
        panic!(
            "IR lower: call callee must be a bare Ident or FieldAccess after typecheck seal \
             (got {:?})",
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
        panic!("IR lower: callee `{name}` has Unresolved resolution after typecheck seal",);
    };
    let entry = registry.get(*id).unwrap_or_else(|| {
        panic!(
            "IR lower: callee id {id} not present in the registry — \
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
        "IR lower: impl_args expects an owner-qualified identifier (got `{identifier}`)",
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
            "IR lower: local closure call callee resolved to non-function type \
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
    ctx.mark_owned(dest);
    Ok((dest, current))
}

/// Lower a call whose callee is a non-Ident expression of fn type
/// (today: a `FieldAccess` produced by the field-as-callable
/// rewrite). Lowers the callee to a fn-typed value, lowers args in
/// order, then emits [`IRInstruction::CallClosure`].
fn lower_closure_expr_call(
    callee: &Expr,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let ResolvedType::Anonymous(AnonymousKind::Function { ret, .. }) = &callee.resolution else {
        panic!(
            "IR lower: closure-expr call callee resolved to non-function type ({:?}) — \
             typecheck seal violation",
            callee.resolution,
        );
    };
    let return_ty = resolved_type_to_ir_type(ret, registry, &mut output.instantiations);

    let (callee_value, mut current) = lower_expr(callee, ctx, block, registry, output)?;
    let mut lowered_args = Vec::with_capacity(args.len());
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
    ctx.mark_owned(dest);
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
    if let Some(opaque) = opaque_debug_method(method, &receiver.resolution) {
        return lower_opaque_debug_call(opaque, receiver, ctx, block, registry, output);
    }
    let dispatch = method_dispatch_kind(receiver, registry);
    let (prepend, current_block) = match dispatch {
        Dispatch::Static => (None, block),
        Dispatch::Instance => {
            let (recv_id, next_block) = lower_expr(receiver, ctx, block, registry, output)?;
            (Some(recv_id), next_block)
        }
    };
    let struct_id = canonical_receiver_id(receiver_struct_id(receiver, dispatch), registry);

    let struct_entry = registry.get(struct_id).unwrap_or_else(|| {
        panic!(
            "IR lower: method call receiver id {struct_id} not present in the registry — \
             seal invariant violation",
        )
    });
    let receiver_type_args = receiver_type_args(receiver, dispatch);
    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let (method_id, method_entry) = registry.lookup(&method_identifier).unwrap_or_else(|| {
        panic!(
            "IR lower: method `{method_identifier}` missing from registry — \
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
        // Enqueue the specific method we're calling so the mono
        // worklist sees the call's `(method_id, receiver_args,
        // method_args)` triple. Static dispatch on a generic type
        // (`Task.async(...)`) never lowers the receiver expression,
        // so without this push the call symbol mangled above would
        // have no matching `IRFunction` and `seal_program_calls`
        // would panic.
        if !receiver_type_args.is_empty() || !method_type_args.is_empty() {
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
/// type-args attached at the AST layer (the pipeline does not yet support
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
            "IR lower: callee `{}` resolved to non-function entry ({}) — \
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

/// Collapse `Global.Int64` / `Global.Float64` onto `Global.Int` /
/// `Global.Float` for method lookup. The typecheck pass treats these
/// pairs as alias-equivalent (see
/// [`koja_typecheck::pipeline::resolve::types::types_equivalent`])
/// — until `Int` and `Float` become proper unions over their sized
/// variants, methods registered on the unsized canonical (e.g.
/// `Debug.format`, `Equality.eq`, `Hash.hash`) need to be reachable
/// through an `Int64`-resolved receiver too. Other primitive widths
/// (`Int8`, `UInt32`, etc.) keep their own ids — they're distinct
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
    let canonical_ident = Identifier::new("Global", vec![canonical.to_string()]);
    registry
        .lookup(&canonical_ident)
        .map(|(id, _)| id)
        .unwrap_or(id)
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
            let ResolvedType::Named {
                resolution: Resolution::Global(struct_id),
                ..
            } = resolution
            else {
                panic!(
                    "IR lower: instance method receiver resolved to non-Global type \
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
/// the final block. Under value semantics every argument is passed
/// by value; the caller retains its slots and frees them at scope
/// exit.
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
    ctx.mark_owned(dest);
    Ok((dest, current))
}

/// `Debug` protocol methods that get the opaque-receiver shortcut.
/// Mirrors the AST-layer opaque-field rule in
/// [`koja_typecheck::pipeline::synthesize::derive_debug`] (the
/// `is_opaque_type` helper there). Keep the two layers in sync: if
/// you add a new opaque shape to one, add it to the other.
#[derive(Clone, Copy)]
enum OpaqueDebugMethod {
    Format,
    Inspect,
    Print,
}

/// Recognize a bounded `Debug.{format, print, inspect}` call whose
/// receiver, after monomorphic substitution, resolved to a type that
/// the pipeline treats as opaque to `format` (a union or a function/closure
/// type). Returning `Some` short-circuits the regular instance-method
/// path in [`lower_method_call`] — `receiver_struct_id` would
/// otherwise panic because anonymous types have no `Named { Global }`
/// receiver to look a method up against.
///
/// Today's opaque set mirrors `derive_debug`'s `is_opaque_type` for
/// struct/enum field rendering: `TypeExpr::Function` and
/// `TypeExpr::Union`. `TypeExpr::Self_` / `TypeExpr::Unit` map to a
/// `Named { Global }` receiver at this layer (`Self` is the enclosing
/// type, `()` is `Global.Unit`), so they don't need a sibling arm.
///
/// Aliases peel earlier in typecheck, so a direct `ResolvedType::Union`
/// or `ResolvedType::Anonymous(Function)` is what reaches here — no
/// alias-walking needed.
fn opaque_debug_method(method: &str, ty: &ResolvedType) -> Option<OpaqueDebugMethod> {
    let kind = match method {
        "format" => OpaqueDebugMethod::Format,
        "inspect" => OpaqueDebugMethod::Inspect,
        "print" => OpaqueDebugMethod::Print,
        _ => return None,
    };
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { .. }) | ResolvedType::Union(_) => {
            Some(kind)
        }
        _ => None,
    }
}

/// Emit the opaque-receiver shortcut for the `Debug` protocol. The
/// behavioral contract matches what `derive_debug` already emits at
/// the AST layer for opaque struct fields:
///
/// * `format(self) -> String` returns the literal `"..."`.
/// * `print(self) -> Unit` writes `"..."` to stdout via `IO.puts` and
///   returns `Unit`.
/// * `inspect(self) -> Self` writes `"..."` via `IO.puts` and returns
///   the receiver value unchanged, so call chains preserve the value.
///
/// The receiver is lowered exactly once: the `Inspect` arm passes it
/// through as the result; the `Format` and `Print` arms still lower
/// it for side effects (closure captures, owner reads) even though
/// they discard the value.
fn lower_opaque_debug_call(
    method: OpaqueDebugMethod,
    receiver: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let (receiver_value, mut current) = lower_expr(receiver, ctx, block, registry, output)?;
    let placeholder = emit_opaque_placeholder(ctx, current);
    match method {
        OpaqueDebugMethod::Format => Ok((placeholder, current)),
        OpaqueDebugMethod::Print => {
            current = emit_io_puts(placeholder, ctx, current);
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
        OpaqueDebugMethod::Inspect => {
            current = emit_io_puts(placeholder, ctx, current);
            Ok((receiver_value, current))
        }
    }
}

/// Allocate a fresh `String`-typed value holding the literal `"..."`.
fn emit_opaque_placeholder(ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let dest = ctx.fresh_value(IRType::String);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest,
            value: ConstValue::String("...".to_string()),
        },
    );
    dest
}

/// Emit `Global.IO.puts(<message>)` and return the block the call
/// landed in. The callee symbol matches the one stamped by lift for
/// the `IO.puts` function in [`koja/lib/global/src/io.koja`], so the
/// regular function registration in `lower_function_inner` resolves
/// it at link time.
fn emit_io_puts(message: ValueId, ctx: &mut FnLowerCtx, block: IRBlockId) -> IRBlockId {
    let callee = IRSymbol::from_identifier(&Identifier::new(
        "Global",
        vec!["IO".to_string(), "puts".to_string()],
    ));
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
