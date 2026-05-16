//! Expression-level lowering: dispatch on [`ExprKind`], lower each
//! supported variant into a `(ValueId, IRBlockId)` (the produced
//! value plus the block it sits in), and surface a feature-gap
//! diagnostic for any unsupported variant.
//!
//! Call-site lowering (`f(args)` / `recv.m(args)`) lives in
//! [`super::calls`] — only the dispatcher entries here, which fan
//! out to it.

use expo_ast::ast::{BinOp, Diagnostic, Expr, ExprKind, Literal, StringPart, UnaryOp};
use expo_ast::coercion::Coercion;
use expo_ast::identifier::{GlobalRegistryId, LocalId, Resolution, ResolvedType};
use expo_ast::labels::expr_kind_label;
use expo_ast::span::Span;
use expo_typecheck::{GlobalKind, GlobalRegistry, LiteralCoercion, NumericLiteralWidth};

use crate::constant::IRConstantValue;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::types::{ConcatKind, ConstValue, IRType, ValueId};

use super::arms::lower_result_ty;
use super::binary_literal::lower_binary_literal;
use super::calls::{MethodCallShape, lower_call, lower_method_call};
use super::closures::{lower_block_closure, lower_short_closure, synthesize_fn_as_closure_wrapper};
use super::constants::{constant_value_from_registry, pools_in_constant_pool};
use super::control_flow::{
    CondLowering, IfLowering, TernaryLowering, lower_cond, lower_if, lower_ternary, lower_unless,
};
use super::ctx::{FnLowerCtx, LowerOutput};
use super::enums::lower_enum_construction;
use super::list_literal::lower_list_literal;
use super::loops::{lower_loop, lower_while};
use super::map_literal::lower_map_literal;
use super::match_expr::{MatchLowering, lower_match};
use super::ops::{
    bin_op_result_type, const_value_type, int_const_at_width, lower_bin_op, lower_literal,
    lower_unary_op, parse_int_literal, unary_op_result_type,
};
use super::package::resolved_type_to_ir_type;
use super::process::{lower_receive, lower_spawn};
use super::structs::{lower_field_access, lower_struct_construction};

pub(super) fn lower_expr(
    expr: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let (value, block) = lower_expr_inner(expr, ctx, block, registry, output)?;
    Ok(apply_value_coercion(
        expr, value, ctx, block, registry, output,
    ))
}

/// Apply `expr.coercion` (if any) to a freshly lowered value. Each
/// [`Coercion`] variant pairs 1:1 with an `IRInstruction::*`
/// emission per the northstar coercion contract; today the only
/// variant is [`Coercion::UnionWiden`] → [`IRInstruction::UnionWrap`].
fn apply_value_coercion(
    expr: &Expr,
    value: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let Some(coercion) = &expr.coercion else {
        return (value, block);
    };
    match coercion {
        Coercion::UnionWiden(target) => {
            let target_ir = resolved_type_to_ir_type(target, registry, &mut output.instantiations);
            let IRType::Union { members, .. } = &target_ir else {
                panic!(
                    "IR lower: Coercion::UnionWiden target lowered to non-Union \
                     `{target_ir:?}` — typecheck invariant violation",
                );
            };
            let member_type = ctx.type_of(value).clone();
            let member_index = members
                .iter()
                .position(|m| m == &member_type)
                .unwrap_or_else(|| {
                    panic!(
                        "IR lower: Coercion::UnionWiden source type `{member_type:?}` \
                         is not a member of target union `{target_ir:?}` — typecheck \
                         invariant violation",
                    )
                }) as u8;
            let dest = ctx.fresh_value(target_ir.clone());
            ctx.cfg.append(
                block,
                IRInstruction::UnionWrap {
                    dest,
                    member_index,
                    member_type,
                    ty: target_ir,
                    value,
                },
            );
            (dest, block)
        }
    }
}

fn lower_expr_inner(
    expr: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let (lhs, block) = lower_expr(left, ctx, block, registry, output)?;
            let (rhs, block) = lower_expr(right, ctx, block, registry, output)?;
            if matches!(op, BinOp::Concat) {
                let kind = concat_kind_from_operand(ctx.type_of(lhs)).ok_or_else(|| {
                    output.diagnostics.push(Diagnostic::error(
                        format!(
                            "IR lower: `<>` operands must be String / Binary / Bits, got `{:?}`",
                            ctx.type_of(lhs),
                        ),
                        expr.span,
                    ));
                })?;
                let dest = ctx.fresh_value(kind.ir_type());
                ctx.cfg.append(
                    block,
                    IRInstruction::Concat {
                        dest,
                        kind,
                        lhs,
                        rhs,
                    },
                );
                return Ok((dest, block));
            }
            let ir_op = lower_bin_op(*op, expr.span, &mut output.diagnostics)?;
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
        ExprKind::BinaryLiteral { segments } => {
            lower_binary_literal(segments, expr.span, ctx, block, registry, output)
        }
        ExprKind::Call {
            callee,
            args,
            type_args,
        } => lower_call(callee, args, type_args, ctx, block, registry, output),
        ExprKind::Closure {
            params,
            body,
            return_type: _,
        } => lower_block_closure(params, body, &expr.resolution, ctx, block, registry, output),
        ExprKind::EnumConstruction { variant, data, .. } => lower_enum_construction(
            variant,
            data,
            &expr.resolution,
            ctx,
            block,
            registry,
            output,
        ),
        ExprKind::FieldAccess { receiver, field } => lower_field_access(
            receiver,
            field,
            &expr.resolution,
            ctx,
            block,
            registry,
            output,
        ),
        ExprKind::Group { expr: inner } => lower_expr(inner, ctx, block, registry, output),
        ExprKind::Ident { resolution, name } => match resolution {
            Resolution::Local(local_id) => Ok(lower_local_read(
                *local_id,
                &expr.resolution,
                ctx,
                block,
                registry,
                &mut output.instantiations,
            )),
            Resolution::Global(global_id) => Ok(lower_global_ident(
                *global_id,
                name,
                &expr.resolution,
                expr.span,
                ctx,
                block,
                registry,
                output,
            )),
            other => panic!(
                "IR lower: bare `Ident` `{name}` reaches lower with non-Local/Global \
                 resolution {other:?} — typecheck seal must have rejected this",
            ),
        },
        ExprKind::ShortClosure { params, body } => {
            lower_short_closure(params, body, &expr.resolution, ctx, block, registry, output)
        }
        ExprKind::Self_ { local_id } => {
            let local_id = local_id.unwrap_or_else(|| {
                panic!(
                    "IR lower: `self` reaches lower without a stamped LocalId — \
                     typecheck resolve invariant violation",
                );
            });
            Ok(lower_local_read(
                local_id,
                &expr.resolution,
                ctx,
                block,
                registry,
                &mut output.instantiations,
            ))
        }
        ExprKind::Cond { arms, else_body } => {
            let result_ty = lower_result_ty(&expr.resolution, registry, output);
            lower_cond(
                CondLowering {
                    arms,
                    else_body: else_body.as_deref(),
                    result_ty,
                },
                ctx,
                block,
                registry,
                output,
            )
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            let result_ty = lower_result_ty(&expr.resolution, registry, output);
            lower_if(
                IfLowering {
                    condition,
                    else_body: else_body.as_deref(),
                    result_ty,
                    then_body,
                },
                ctx,
                block,
                registry,
                output,
            )
        }
        ExprKind::Literal { value } => {
            let target = literal_width(expr);
            let const_value = lower_literal(value, expr.span, target, &mut output.diagnostics)?;
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
        ExprKind::Match { subject, arms } => {
            let result_ty = lower_result_ty(&expr.resolution, registry, output);
            lower_match(
                MatchLowering {
                    subject,
                    arms,
                    result_ty,
                },
                ctx,
                block,
                registry,
                output,
            )
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            type_args,
        } => lower_method_call(
            receiver,
            MethodCallShape {
                method,
                args,
                method_type_args: type_args,
            },
            ctx,
            block,
            registry,
            output,
        ),
        ExprKind::String { parts, .. } => lower_string(parts, ctx, block, registry, output),
        ExprKind::StructConstruction { fields, .. } => {
            lower_struct_construction(fields, &expr.resolution, ctx, block, registry, output)
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            let result_ty = lower_result_ty(&expr.resolution, registry, output);
            lower_ternary(
                TernaryLowering {
                    condition,
                    then_expr,
                    else_expr,
                    result_ty,
                },
                ctx,
                block,
                registry,
                output,
            )
        }
        ExprKind::Unary { op, operand } => {
            // `-N` against a narrow target folds to a single typed
            // `Const` at the recorded width — the typecheck pass
            // stamps a coercion on the *outer* `Unary`'s span when
            // the negated literal flows into a sized slot. Without
            // a coercion record (or against a non-literal operand)
            // we fall through to the regular UnaryOp emission.
            if matches!(op, UnaryOp::Neg)
                && let Some(target) = literal_width(expr)
                && let Some(folded) = fold_negated_literal_const(operand, target)
            {
                let ty = const_value_type(&folded);
                let dest = ctx.fresh_value(ty);
                ctx.cfg.append(
                    block,
                    IRInstruction::Const {
                        dest,
                        value: folded,
                    },
                );
                return Ok((dest, block));
            }
            let (operand, block) = lower_expr(operand, ctx, block, registry, output)?;
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
            lower_unless(condition, body, ctx, block, registry, output)
        }
        ExprKind::While { condition, body } => {
            lower_while(condition, body, ctx, block, registry, output)
        }
        ExprKind::List { elements } => lower_list_literal(
            elements,
            &expr.resolution,
            expr.span,
            ctx,
            block,
            registry,
            output,
        ),
        ExprKind::Loop { body } => lower_loop(body, ctx, block, registry, output),
        ExprKind::Map { entries } => lower_map_literal(
            entries,
            &expr.resolution,
            expr.span,
            ctx,
            block,
            registry,
            output,
        ),
        ExprKind::Spawn { expr: inner } => lower_spawn(
            inner,
            expr.span,
            &expr.resolution,
            ctx,
            block,
            registry,
            output,
        ),
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => lower_receive(
            arms,
            after_timeout.as_deref(),
            after_body,
            &expr.resolution,
            expr.span,
            ctx,
            block,
            registry,
            output,
        ),
        other => {
            output.diagnostics.push(Diagnostic::error(
                format!(
                    "IR does not yet lower this expression kind ({})",
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
/// table. Closure-body ctxs intercept captured-outer-local ids and
/// emit a [`IRInstruction::LoadCapture`] indexed into the enclosing
/// closure's env layout.
fn lower_local_read(
    local_id: LocalId,
    resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> (ValueId, IRBlockId) {
    let ty = resolved_type_to_ir_type(resolution, registry, instantiations);
    if let Some(capture_index) = ctx.closures().capture_index(local_id) {
        let dest = ctx.fresh_value(ty.clone());
        ctx.cfg.append(
            block,
            IRInstruction::LoadCapture {
                capture_index,
                dest,
                ty,
            },
        );
        return (dest, block);
    }
    let ir_local = IRLocalId::from_local_id(local_id);
    let dest = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::LocalRead {
            dest,
            local: ir_local,
            ty,
        },
    );
    ctx.record_value_source(dest, ir_local);
    (dest, block)
}

/// Dispatch a bare ident with [`Resolution::Global`] on the
/// resolved entry's kind: constants flow through
/// [`lower_constant_ident`]; non-generic functions used as values
/// flow through [`lower_fn_as_value`] (synthesizing a captureless
/// closure wrapper and emitting [`IRInstruction::MakeClosure`]).
#[allow(clippy::too_many_arguments)]
fn lower_global_ident(
    global_id: GlobalRegistryId,
    name: &str,
    expr_resolution: &ResolvedType,
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let entry = registry.get(global_id).unwrap_or_else(|| {
        panic!("IR lower: global id {global_id} missing from registry — seal violation",)
    });
    match &entry.kind {
        GlobalKind::Constant(_) => {
            lower_constant_ident(global_id, name, span, ctx, block, registry, output)
        }
        GlobalKind::Function(_) => lower_fn_as_value(
            global_id,
            name,
            expr_resolution,
            ctx,
            block,
            registry,
            output,
        ),
        other => panic!(
            "IR lower: bare `Ident` `{name}` (id {global_id}) registers as {} — \
             typecheck seal violation",
            other.label(),
        ),
    }
}

/// Lower a bare ident that resolves to a package-level constant.
/// Primitives inline as [`IRInstruction::Const`]; compounds emit a
/// [`IRInstruction::LoadConst`] against the pool entry minted in
/// [`super::package::lower_package`].
fn lower_constant_ident(
    constant_id: GlobalRegistryId,
    name: &str,
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let value = constant_value_from_registry(constant_id, registry).unwrap_or_else(|| {
        panic!(
            "IR lower: constant `{name}` (id {constant_id}) reaches lower \
                 without a stamped definition or with an unsupported RHS shape — \
                 typecheck seal must have rejected this",
        );
    });
    let entry = registry.get(constant_id).unwrap_or_else(|| {
        panic!("IR lower: constant id {constant_id} missing from registry — seal violation",)
    });
    let GlobalKind::Constant(Some(def)) = &entry.kind else {
        panic!(
            "IR lower: constant id {constant_id} ({name}) registers as {} — seal violation",
            entry.kind.label(),
        );
    };
    let _ = span;
    let ty = resolved_type_to_ir_type(&def.ty, registry, &mut output.instantiations);
    if pools_in_constant_pool(&value) {
        let const_id = IRSymbol::from_identifier(&entry.identifier);
        let dest = ctx.fresh_value(ty.clone());
        ctx.cfg
            .append(block, IRInstruction::LoadConst { const_id, dest, ty });
        (dest, block)
    } else {
        let IRConstantValue::Primitive(value) = value else {
            unreachable!("non-pooling IRConstantValue must be Primitive — pool admission rule");
        };
        let dest = ctx.fresh_value(const_value_type(&value));
        ctx.cfg.append(block, IRInstruction::Const { dest, value });
        (dest, block)
    }
}

/// Lower a bare ident that resolves to a named function used as a
/// value (the resolver lifts every non-generic function ident to
/// [`expo_ast::identifier::AnonymousKind::Function`]). Synthesizes
/// a captureless wrapper closure and emits
/// [`IRInstruction::MakeClosure`] with no captures.
fn lower_fn_as_value(
    function_id: GlobalRegistryId,
    name: &str,
    expr_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let entry = registry.get(function_id).unwrap_or_else(|| {
        panic!("IR lower: fn-as-value id {function_id} missing from registry — seal violation",)
    });
    let GlobalKind::Function(Some(sig)) = &entry.kind else {
        panic!(
            "IR lower: fn-as-value `{name}` (id {function_id}) registers as {} — \
             typecheck seal violation",
            entry.kind.label(),
        );
    };
    if !entry.type_params.is_empty() {
        panic!(
            "IR lower: fn-as-value `{name}` (id {function_id}) is generic — typecheck \
             must have diagnosed this before lowering",
        );
    }
    let target_symbol = IRSymbol::from_identifier(&entry.identifier);
    let wrapper_symbol = synthesize_fn_as_closure_wrapper(&target_symbol, sig, registry, output);
    let ty = resolved_type_to_ir_type(expr_resolution, registry, &mut output.instantiations);
    let dest = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::MakeClosure {
            body: wrapper_symbol,
            captures: Vec::new(),
            dest,
            ty,
        },
    );
    (dest, block)
}

/// Fold a literal-arg to `UnaryOp::Neg` directly into a typed
/// `ConstValue` at the recorded coercion width. Returns `None` for
/// shapes the typecheck pass would never have stamped a coercion
/// on — non-literal operand, group-wrapped non-literal, etc. —
/// letting the caller fall back to the regular runtime negate.
/// Hex / binary literals reach this helper through `parse_int_literal`
/// for the unsigned escape hatch (`-1: UInt8` is rejected at
/// typecheck so it never reaches here, but `0xFF: Int8` does).
/// Pull the typecheck-stamped numeric width off `expr`'s
/// `literal_coercion` slot, when present. Reserved for the leaf
/// sites that emit a typed `Const` opcode (literal, negated-literal
/// fold, pattern-equality) — every other position ignores the
/// annotation.
fn literal_width(expr: &Expr) -> Option<NumericLiteralWidth> {
    expr.literal_coercion
        .as_ref()
        .and_then(LiteralCoercion::numeric_width)
}

fn fold_negated_literal_const(operand: &Expr, target: NumericLiteralWidth) -> Option<ConstValue> {
    match &operand.kind {
        ExprKind::Group { expr } => fold_negated_literal_const(expr, target),
        ExprKind::Literal {
            value: Literal::Int(text),
        } => parse_int_literal(text)
            .ok()
            .and_then(|n| (n as i128).checked_neg())
            .map(|neg| int_const_at_width(neg, Some(target))),
        ExprKind::Literal {
            value: Literal::Float(text),
        } => text.parse::<f64>().ok().map(|f| match target {
            NumericLiteralWidth::Float32 => ConstValue::Float32(-f as f32),
            _ => ConstValue::Float64(-f),
        }),
        _ => None,
    }
}

/// Pick the [`ConcatKind`] that matches a `<>` operand's IR type.
/// Typecheck guarantees both operands share a heap-payload type
/// (`String`, `Binary`, `Bits`); the lowerer just transcribes that
/// into an [`IRInstruction::Concat`]'s `kind`. Any other type
/// surfaces `None` so the call site can emit a clear lower-layer
/// diagnostic (defense-in-depth — the only path that reaches here
/// is a typecheck-passed `BinOp::Concat`).
fn concat_kind_from_operand(ty: IRType) -> Option<ConcatKind> {
    match ty {
        IRType::String => Some(ConcatKind::String),
        IRType::Binary => Some(ConcatKind::Binary),
        IRType::Bits => Some(ConcatKind::Bits),
        _ => None,
    }
}

/// Lower a (possibly interpolated) string literal into a single
/// `String`-typed value.
///
/// Strategy: each part lowers to its own `String` value
/// ([`emit_string_const`] for literals, recursive [`lower_expr`] for
/// interpolations — the typecheck synthesizer wraps every
/// interpolated expression in `.format()` so it's already
/// `String`-typed by the time we see it). N parts then fold into
/// N-1 chained binary [`IRInstruction::Concat`] instructions; empty
/// parts produces a single empty-string const.
///
/// Single-part fast paths preserve byte-for-byte the prior shape:
/// a lone literal emits one `Const`, a lone interpolation emits no
/// `Concat` at all.
fn lower_string(
    parts: &[StringPart],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    if parts.is_empty() {
        return Ok((emit_string_const(String::new(), ctx, block), block));
    }
    let mut iter = parts.iter();
    let first = iter.next().expect("non-empty parts");
    let (mut acc, mut block) = lower_string_part(first, ctx, block, registry, output)?;
    for part in iter {
        let (next_value, next_block) = lower_string_part(part, ctx, block, registry, output)?;
        block = next_block;
        let dest = ctx.fresh_value(IRType::String);
        ctx.cfg.append(
            block,
            IRInstruction::Concat {
                dest,
                kind: ConcatKind::String,
                lhs: acc,
                rhs: next_value,
            },
        );
        acc = dest;
    }
    Ok((acc, block))
}

fn lower_string_part(
    part: &StringPart,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    match part {
        StringPart::Literal { value, .. } => {
            Ok((emit_string_const(value.clone(), ctx, block), block))
        }
        StringPart::Interpolation { expr, .. } => lower_expr(expr, ctx, block, registry, output),
    }
}

fn emit_string_const(value: String, ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let dest = ctx.fresh_value(IRType::String);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest,
            value: ConstValue::String(value),
        },
    );
    dest
}
