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

use crate::Lowerer;
use crate::blocks::IRBlockId;
use crate::cfg::CFGBuilder;
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

impl<'a> Lowerer<'a> {
    /// Lower an [`expo_ast::ast::ExprKind::Binary`] to an
    /// [`IRInstruction::BinaryOp`] when the operator and operand
    /// shapes are within the IR vocabulary. Returns `None` for cases
    /// that fall through to the [`IRInstruction::Stub`] bridge:
    ///
    /// - [`BinOp::Concat`] -- runs through `compile_concat`'s
    ///   multi-block memcpy sequence; awaits its own dedicated
    ///   instruction.
    /// - [`ResolvedBinaryOp::EnumStructEqual`] -- multi-block
    ///   per-variant equality; awaits its own dedicated instruction.
    /// - Operands whose resolved type doesn't map to a supported
    ///   shape (parameters, named types, etc.).
    pub(super) fn lower_binary_op_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<Option<(Option<IRBlockId>, IROperand)>, String> {
        if matches!(op, BinOp::Concat) {
            return Ok(None);
        }
        let Some(shape) = left.resolved_type.as_ref().and_then(operand_shape_for_type) else {
            return Ok(None);
        };
        let Ok(resolved) = resolve_binary_op(op, &shape) else {
            return Ok(None);
        };
        if matches!(resolved, ResolvedBinaryOp::EnumStructEqual { .. }) {
            return Ok(None);
        }
        let (open, lhs) = self.lower_expr_to_operand(builder, open, left)?;
        let Some(open) = open else {
            return Ok(Some((None, IROperand::Unit)));
        };
        let (open, rhs) = self.lower_expr_to_operand(builder, open, right)?;
        let Some(open) = open else {
            return Ok(Some((None, IROperand::Unit)));
        };
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::BinaryOp {
                dest,
                op: resolved,
                lhs,
                rhs,
            },
        );
        Ok(Some((Some(open), IROperand::Local(dest))))
    }

    /// Lower an [`expo_ast::ast::ExprKind::Unary`] to an
    /// [`IRInstruction::UnaryOp`]. Returns `Ok(None)` for operands
    /// whose resolved type doesn't map to a supported shape, falling
    /// back to the [`IRInstruction::Stub`] bridge.
    pub(super) fn lower_unary_op_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        op: &UnaryOp,
        operand_expr: &Expr,
    ) -> Result<Option<(Option<IRBlockId>, IROperand)>, String> {
        let Some(shape) = operand_expr
            .resolved_type
            .as_ref()
            .and_then(operand_shape_for_type)
        else {
            return Ok(None);
        };
        let Ok(resolved) = resolve_unary_op(op, &shape) else {
            return Ok(None);
        };
        let (open, operand) = self.lower_expr_to_operand(builder, open, operand_expr)?;
        let Some(open) = open else {
            return Ok(Some((None, IROperand::Unit)));
        };
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::UnaryOp {
                dest,
                op: resolved,
                operand,
            },
        );
        Ok(Some((Some(open), IROperand::Local(dest))))
    }
}
