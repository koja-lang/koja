//! Lowering helpers for [`expo_ast::ast::ExprKind::Binary`] and
//! [`expo_ast::ast::ExprKind::Unary`].
//!
//! These helpers take a typed AST operator expression, resolve the
//! operation against typecheck-derived operand shapes, and push a
//! typed [`IRInstruction::BinaryOp`] / [`IRInstruction::UnaryOp`]
//! onto the caller-supplied instruction sequence. Returns
//! `Some(IROperand::Local(dest))` on success or `None` to signal the
//! caller should fall back to the [`IRInstruction::Stub`] bridge --
//! used for cases the IR vocabulary doesn't yet cover (Concat,
//! enum/struct equality, parameter-typed operands, etc.).
//!
//! Operand shapes are derived from the typecheck-resolved
//! [`expr.resolved_type`] via [`operand_shape_for_type`], not from
//! compiled LLVM values. This is the lowering-time analogue of the
//! shape derivation `expo-codegen`'s `compile_binary` does at
//! emission time.

use expo_ast::ast::{BinOp, Expr, UnaryOp};
use expo_ast::types::{Primitive, Type};

use crate::FnLowerState;
use crate::lower::values::lower_expr_to_operand;
use crate::resolved::ops::{OperandShape, ResolvedBinaryOp, resolve_binary_op, resolve_unary_op};
use crate::values::{IRInstruction, IROperand};

/// Derive the [`OperandShape`] for a typecheck-resolved [`Type`].
///
/// Returns `None` for shapes the IR vocabulary doesn't yet cover --
/// `Type::Named` (enum/struct equality), `Type::Parameter`,
/// `Type::Function`, etc. Callers fall back to the [`IRInstruction::Stub`]
/// bridge when this returns `None`.
pub(super) fn operand_shape_for_type(ty: &Type) -> Option<OperandShape> {
    match ty {
        Type::Primitive(prim) => primitive_shape(*prim),
        Type::Indirect(inner) => operand_shape_for_type(inner),
        _ => None,
    }
}

fn primitive_shape(prim: Primitive) -> Option<OperandShape> {
    match prim {
        Primitive::Bool => Some(OperandShape::Integer { bit_width: 1 }),
        Primitive::I8 | Primitive::U8 => Some(OperandShape::Integer { bit_width: 8 }),
        Primitive::I16 | Primitive::U16 => Some(OperandShape::Integer { bit_width: 16 }),
        Primitive::I32 | Primitive::U32 => Some(OperandShape::Integer { bit_width: 32 }),
        Primitive::I64 | Primitive::U64 => Some(OperandShape::Integer { bit_width: 64 }),
        Primitive::F32 | Primitive::F64 => Some(OperandShape::Float),
        Primitive::String => Some(OperandShape::Pointer),
        Primitive::Binary | Primitive::Bits => None,
    }
}

/// Lower an [`expo_ast::ast::ExprKind::Binary`] to an
/// [`IRInstruction::BinaryOp`] when the operator and operand shapes
/// are within the IR vocabulary. Returns `None` for cases that fall
/// through to the [`IRInstruction::Stub`] bridge:
///
/// - [`BinOp::Concat`] -- runs through `compile_concat`'s multi-block
///   memcpy sequence; awaits its own dedicated instruction.
/// - [`ResolvedBinaryOp::EnumStructEqual`] -- multi-block per-variant
///   equality; awaits its own dedicated instruction.
/// - Operands whose resolved type doesn't map to a supported shape
///   (parameters, named types, etc.).
pub(super) fn lower_binary_op_or_stub(
    state: &mut FnLowerState,
    instructions: &mut Vec<IRInstruction>,
    op: &BinOp,
    left: &Expr,
    right: &Expr,
) -> Option<IROperand> {
    if matches!(op, BinOp::Concat) {
        return None;
    }
    let shape = operand_shape_for_type(left.resolved_type.as_ref()?)?;
    let resolved = resolve_binary_op(op, &shape).ok()?;
    if matches!(resolved, ResolvedBinaryOp::EnumStructEqual { .. }) {
        return None;
    }
    let lhs = lower_expr_to_operand(state, instructions, left);
    let rhs = lower_expr_to_operand(state, instructions, right);
    let dest = state.next_value_id();
    instructions.push(IRInstruction::BinaryOp {
        dest,
        op: resolved,
        lhs,
        rhs,
    });
    Some(IROperand::Local(dest))
}

/// Lower an [`expo_ast::ast::ExprKind::Unary`] to an
/// [`IRInstruction::UnaryOp`]. Returns `None` for operands whose
/// resolved type doesn't map to a supported shape, falling back to
/// the [`IRInstruction::Stub`] bridge.
pub(super) fn lower_unary_op_or_stub(
    state: &mut FnLowerState,
    instructions: &mut Vec<IRInstruction>,
    op: &UnaryOp,
    operand_expr: &Expr,
) -> Option<IROperand> {
    let shape = operand_shape_for_type(operand_expr.resolved_type.as_ref()?)?;
    let resolved = resolve_unary_op(op, &shape).ok()?;
    let operand = lower_expr_to_operand(state, instructions, operand_expr);
    let dest = state.next_value_id();
    instructions.push(IRInstruction::UnaryOp {
        dest,
        op: resolved,
        operand,
    });
    Some(IROperand::Local(dest))
}
