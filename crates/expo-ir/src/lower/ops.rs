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
use crate::lower::ctx::LocalBindings;
use crate::lower::strings::resolve_concat_kind;
use crate::resolved::ops::{OperandShape, ResolvedBinaryOp, resolve_binary_op, resolve_unary_op};
use crate::resolved::strings::ResolvedConcatKind;
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
    /// - [`ResolvedBinaryOp::EnumStructEqual`] -- multi-block
    ///   per-variant equality; awaits its own dedicated instruction.
    /// - Operands whose resolved type doesn't map to a supported
    ///   shape (parameters, named types, etc.).
    ///
    /// [`BinOp::Concat`] is handled separately and lifts to
    /// [`IRInstruction::Concat`] (not `BinaryOp`) -- the operand kind
    /// is decided up-front via [`resolve_concat_kind`] so the
    /// codegen executor and the interpreter don't need to re-derive
    /// it from runtime LLVM-value shapes.
    pub(super) fn lower_binary_op_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<Option<(Option<IRBlockId>, IROperand, Type)>, String> {
        if matches!(op, BinOp::Concat) {
            return self.lower_concat(builder, open, left, right).map(Some);
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
        let (open, lhs, lhs_ty) = self.lower_expr_to_operand(builder, open, left)?;
        let Some(open) = open else {
            return Ok(Some((None, IROperand::Unit, Type::Unit)));
        };
        let (open, rhs, _rhs_ty) = self.lower_expr_to_operand(builder, open, right)?;
        let Some(open) = open else {
            return Ok(Some((None, IROperand::Unit, Type::Unit)));
        };
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::BinaryOp {
                dest,
                op: resolved.clone(),
                lhs,
                rhs,
            },
        );
        Ok(Some((
            Some(open),
            IROperand::Local(dest),
            binary_op_result_type(&resolved, &lhs_ty),
        )))
    }

    /// Lift [`BinOp::Concat`] (`a <> b`) into an
    /// [`IRInstruction::Concat`]. The operand kind is decided up-front
    /// from the left operand's resolved type via [`resolve_concat_kind`]
    /// (mirroring what `compile_concat` does), then both operands are
    /// lowered through the universal `lower_expr_to_operand` recursion.
    ///
    /// Concat always lifts -- there is no Stub fallback -- so this
    /// helper drops the `Option` wrapper that the `_or_stub` cousins
    /// (e.g. [`Self::lower_binary_op_or_stub`]) return for cases the
    /// IR vocabulary doesn't yet cover. The inner `Option<IRBlockId>`
    /// is `None` if a sub-expression's CFG terminates (`return`,
    /// `panic`, etc.), at which point the operand is conventionally
    /// [`IROperand::Unit`] and unused by the caller.
    fn lower_concat(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        left: &Expr,
        right: &Expr,
    ) -> Result<(Option<IRBlockId>, IROperand, Type), String> {
        let kind = resolve_concat_kind(&self.ctx(), left, |name| self.fn_state.type_of(name));
        let result_ty = match kind {
            ResolvedConcatKind::Binary => Type::Primitive(Primitive::Binary),
            ResolvedConcatKind::String => Type::Primitive(Primitive::String),
        };
        let (open, lhs, _lhs_ty) = self.lower_expr_to_operand(builder, open, left)?;
        let Some(open) = open else {
            return Ok((None, IROperand::Unit, Type::Unit));
        };
        let (open, rhs, _rhs_ty) = self.lower_expr_to_operand(builder, open, right)?;
        let Some(open) = open else {
            return Ok((None, IROperand::Unit, Type::Unit));
        };
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::Concat {
                dest,
                kind,
                parts: vec![lhs, rhs],
            },
        );
        Ok((Some(open), IROperand::Local(dest), result_ty))
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
    ) -> Result<Option<(Option<IRBlockId>, IROperand, Type)>, String> {
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
        let (open, operand, operand_ty) =
            self.lower_expr_to_operand(builder, open, operand_expr)?;
        let Some(open) = open else {
            return Ok(Some((None, IROperand::Unit, Type::Unit)));
        };
        let dest = self.next_value_id();
        let result_ty = unary_op_result_type(op, &operand_ty);
        builder.append(
            open,
            IRInstruction::UnaryOp {
                dest,
                op: resolved,
                operand,
            },
        );
        Ok(Some((Some(open), IROperand::Local(dest), result_ty)))
    }
}

/// Result type of a [`ResolvedBinaryOp`]: comparisons / logical ops
/// produce [`Primitive::Bool`]; arithmetic ops preserve the LHS
/// operand's type (mirrors LLVM int/float promotion rules and the
/// codegen-side `compile_binary` behavior).
fn binary_op_result_type(op: &ResolvedBinaryOp, lhs_ty: &Type) -> Type {
    match op {
        ResolvedBinaryOp::BoolAnd
        | ResolvedBinaryOp::BoolOr
        | ResolvedBinaryOp::EnumStructEqual { .. }
        | ResolvedBinaryOp::FloatEqual
        | ResolvedBinaryOp::FloatGreater
        | ResolvedBinaryOp::FloatGreaterEqual
        | ResolvedBinaryOp::FloatLess
        | ResolvedBinaryOp::FloatLessEqual
        | ResolvedBinaryOp::FloatNotEqual
        | ResolvedBinaryOp::IntEqual
        | ResolvedBinaryOp::IntGreater
        | ResolvedBinaryOp::IntGreaterEqual
        | ResolvedBinaryOp::IntLess
        | ResolvedBinaryOp::IntLessEqual
        | ResolvedBinaryOp::IntNotEqual
        | ResolvedBinaryOp::StringEqual
        | ResolvedBinaryOp::StringNotEqual => Type::Primitive(Primitive::Bool),
        _ => lhs_ty.clone(),
    }
}

/// Result type of a unary operator: `not` -> [`Primitive::Bool`];
/// numeric negation preserves the operand type.
fn unary_op_result_type(op: &UnaryOp, operand_ty: &Type) -> Type {
    match op {
        UnaryOp::Not => Type::Primitive(Primitive::Bool),
        UnaryOp::Neg => operand_ty.clone(),
    }
}
