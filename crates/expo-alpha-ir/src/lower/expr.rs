//! Expression-level lowering: dispatch on [`ExprKind`], lower each
//! supported variant into a `(ValueId, IRBlockId)` (the produced
//! value plus the block it sits in), and surface a feature-gap
//! diagnostic for any unsupported variant.
//!
//! [`lower_call`] lives here too because it's the only expression
//! variant that interacts with the [`GlobalRegistry`] beyond the
//! type-side adapters in [`super::package`].

use expo_alpha_typecheck::{Dispatch, GlobalKind, GlobalRegistry, RegistryEntry};
use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind, StringPart};
use expo_ast::identifier::{Identifier, LocalId, Resolution};
use expo_ast::labels::expr_kind_label;
use expo_ast::span::Span;

use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::local::IRLocalId;
use crate::types::{ConstValue, IRType, ValueId};

use super::control_flow::{lower_if, lower_unless};
use super::ctx::FnLowerCtx;
use super::enums::lower_enum_construction;
use super::ops::{
    bin_op_result_type, const_value_type, lower_bin_op, lower_literal, lower_unary_op,
    unary_op_result_type,
};
use super::package::resolved_type_to_ir_type;
use super::structs::{lower_field_access, lower_struct_construction};

pub(super) fn lower_expr(
    expr: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let (lhs, block) = lower_expr(left, ctx, block, registry, diagnostics)?;
            let (rhs, block) = lower_expr(right, ctx, block, registry, diagnostics)?;
            let ir_op = lower_bin_op(*op, expr.span, diagnostics)?;
            let result_ty = bin_op_result_type(ir_op, ctx.type_of(lhs));
            let dest = ctx.fresh_value(result_ty);
            ctx.cfg.append(
                block,
                IRInstruction::BinaryOp {
                    dest,
                    lhs,
                    op: ir_op,
                    rhs,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, ctx, block, registry, diagnostics)
        }
        ExprKind::EnumConstruction { variant, data, .. } => lower_enum_construction(
            variant,
            data,
            &expr.resolution,
            ctx,
            block,
            registry,
            diagnostics,
        ),
        ExprKind::FieldAccess { receiver, field } => lower_field_access(
            receiver,
            field,
            &expr.resolution,
            ctx,
            block,
            registry,
            diagnostics,
        ),
        ExprKind::Group { expr: inner } => lower_expr(inner, ctx, block, registry, diagnostics),
        ExprKind::Ident { resolution, name } => {
            let Resolution::Local(local_id) = resolution else {
                panic!(
                    "alpha IR lower: bare `Ident` `{name}` reaches lower with non-Local \
                     resolution {resolution:?} — typecheck seal must have rejected this",
                );
            };
            Ok(lower_local_read(
                *local_id,
                &expr.resolution,
                ctx,
                block,
                registry,
            ))
        }
        ExprKind::Self_ { local_id } => {
            let local_id = local_id.unwrap_or_else(|| {
                panic!(
                    "alpha IR lower: `self` reaches lower without a stamped LocalId — \
                     typecheck resolve invariant violation",
                );
            });
            Ok(lower_local_read(
                local_id,
                &expr.resolution,
                ctx,
                block,
                registry,
            ))
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            if else_body.is_some() {
                diagnostics.push(Diagnostic::error(
                    "alpha IR does not yet lower `else` branches",
                    expr.span,
                ));
                return Err(());
            }
            lower_if(condition, then_body, ctx, block, registry, diagnostics)
        }
        ExprKind::Literal { value } => {
            let const_value = lower_literal(value, expr.span, diagnostics)?;
            let ty = const_value_type(&const_value);
            let dest = ctx.fresh_value(ty);
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest,
                    value: const_value,
                },
            );
            Ok((dest, block))
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => lower_method_call(receiver, method, args, ctx, block, registry, diagnostics),
        ExprKind::String { parts, .. } => lower_string(parts, expr.span, ctx, block, diagnostics),
        ExprKind::StructConstruction { fields, .. } => {
            lower_struct_construction(fields, &expr.resolution, ctx, block, registry, diagnostics)
        }
        ExprKind::Unary { op, operand } => {
            let (operand, block) = lower_expr(operand, ctx, block, registry, diagnostics)?;
            let ir_op = lower_unary_op(*op);
            let result_ty = unary_op_result_type(ir_op, ctx.type_of(operand));
            let dest = ctx.fresh_value(result_ty);
            ctx.cfg.append(
                block,
                IRInstruction::UnaryOp {
                    dest,
                    op: ir_op,
                    operand,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Unless { condition, body } => {
            lower_unless(condition, body, ctx, block, registry, diagnostics)
        }
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha IR does not yet lower this expression kind ({})",
                    expr_kind_label(other),
                ),
                expr.span,
            ));
            Err(())
        }
    }
}

/// Materialize a local-slot read. Used for both bare-`Ident` and
/// `self` references — both flow through the same per-function slot
/// table.
fn lower_local_read(
    local_id: LocalId,
    resolution: &expo_ast::identifier::ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
) -> (ValueId, IRBlockId) {
    let ir_local = IRLocalId::from_local_id(local_id);
    let ty = resolved_type_to_ir_type(resolution, registry);
    let dest = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::LocalRead {
            dest,
            local: ir_local,
            ty,
        },
    );
    (dest, block)
}

/// Lower a `ExprKind::Call`. Seal guarantees the callee is a bare
/// `Ident` resolving to `Global(id)`; anything else panics.
fn lower_call(
    callee: &Expr,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
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
    emit_call(entry, args, None, ctx, block, registry, diagnostics)
}

/// Lower `ExprKind::MethodCall`. Static dispatch (`Type.method(...)`)
/// reads the struct id off the receiver's `Resolution::Global`;
/// instance dispatch (`recv.method(...)`) lowers the receiver to a
/// `ValueId`, derives the struct id from its resolved value type,
/// and prepends the receiver to fill `params[0]` (`self`).
fn lower_method_call(
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let dispatch = method_dispatch_kind(receiver, registry);
    let (prepend, current_block) = match dispatch {
        Dispatch::Static => (None, block),
        Dispatch::Instance => {
            let (recv_id, next_block) = lower_expr(receiver, ctx, block, registry, diagnostics)?;
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
    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let (_, method_entry) = registry.lookup(&method_identifier).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: method `{method_identifier}` missing from registry — \
             seal invariant violation",
        )
    });
    emit_call(
        method_entry,
        args,
        prepend,
        ctx,
        current_block,
        registry,
        diagnostics,
    )
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
fn receiver_struct_id(
    receiver: &Expr,
    dispatch: Dispatch,
) -> expo_ast::identifier::GlobalRegistryId {
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
            let Resolution::Global(struct_id) = resolution.resolution else {
                panic!(
                    "alpha IR lower: instance method receiver resolved to non-Global type \
                     ({resolution:?}) — typecheck seal must have rejected this",
                );
            };
            struct_id
        }
    }
}

/// Shared tail of `lower_call` / `lower_method_call`. `prepend` is
/// the receiver `ValueId` for instance dispatch (filling `params[0]` /
/// `self`); `None` for bare calls and static method dispatch.
fn emit_call(
    entry: &RegistryEntry,
    args: &[Arg],
    prepend: Option<ValueId>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let signature = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!(
            "alpha IR lower: callee `{}` resolved to non-function entry ({}) — \
             typecheck seal violation",
            entry.identifier,
            other.label(),
        ),
    };
    let return_ty = resolved_type_to_ir_type(&signature.return_type, registry);
    let callee_symbol = IRSymbol::from_identifier(&entry.identifier);

    let mut lowered_args = Vec::with_capacity(args.len() + usize::from(prepend.is_some()));
    if let Some(receiver) = prepend {
        lowered_args.push(receiver);
    }
    let mut current = block;
    for arg in args {
        let (value, next) = lower_expr(&arg.value, ctx, current, registry, diagnostics)?;
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

fn lower_string(
    parts: &[StringPart],
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let [StringPart::Literal { value, .. }] = parts else {
        diagnostics.push(Diagnostic::error(
            "alpha IR does not yet lower string interpolation",
            span,
        ));
        return Err(());
    };
    let dest = ctx.fresh_value(IRType::String);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest,
            value: ConstValue::String(value.clone()),
        },
    );
    Ok((dest, block))
}
