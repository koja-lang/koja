//! Resolved arithmetic, comparison, logical, and unary operations.
//!
//! These decision types separate "what operation to emit" from "how to emit it
//! in LLVM." Each variant maps to exactly one backend instruction with no
//! further decision logic required.

use expo_ast::ast::{BinOp, CompoundOp, UnaryOp};

/// The shape of an operand as seen by operator resolution. Derived from the
/// compiled value, this carries just enough information for the pure decision
/// function without any backend dependency.
pub enum OperandShape {
    Float,
    Integer { bit_width: u32 },
    Pointer,
    Struct { is_enum: bool },
}

/// The resolved binary operation to emit. Each variant maps to exactly one
/// backend builder call, with no further decision logic required.
pub enum ResolvedBinaryOp {
    BoolAnd,
    BoolOr,
    EnumStructEqual { negated: bool },
    FloatAdd,
    FloatDiv,
    FloatEqual,
    FloatGreater,
    FloatGreaterEqual,
    FloatLess,
    FloatLessEqual,
    FloatMul,
    FloatNotEqual,
    FloatRem,
    FloatSub,
    IntAdd,
    IntDiv,
    IntEqual,
    IntGreater,
    IntGreaterEqual,
    IntLess,
    IntLessEqual,
    IntMul,
    IntNotEqual,
    IntRem,
    IntSub,
    StringEqual,
    StringNotEqual,
}

/// Pure decision function: given an AST binary operator and the operand shape,
/// returns which concrete operation to emit. No backend types involved.
pub fn resolve_binary_op(op: &BinOp, shape: &OperandShape) -> Result<ResolvedBinaryOp, String> {
    match shape {
        OperandShape::Float => match op {
            BinOp::Add => Ok(ResolvedBinaryOp::FloatAdd),
            BinOp::Div => Ok(ResolvedBinaryOp::FloatDiv),
            BinOp::Eq => Ok(ResolvedBinaryOp::FloatEqual),
            BinOp::Gt => Ok(ResolvedBinaryOp::FloatGreater),
            BinOp::GtEq => Ok(ResolvedBinaryOp::FloatGreaterEqual),
            BinOp::Lt => Ok(ResolvedBinaryOp::FloatLess),
            BinOp::LtEq => Ok(ResolvedBinaryOp::FloatLessEqual),
            BinOp::Mod => Ok(ResolvedBinaryOp::FloatRem),
            BinOp::Mul => Ok(ResolvedBinaryOp::FloatMul),
            BinOp::NotEq => Ok(ResolvedBinaryOp::FloatNotEqual),
            BinOp::Sub => Ok(ResolvedBinaryOp::FloatSub),
            BinOp::And | BinOp::Concat | BinOp::Or => {
                Err(format!("unsupported float binary op: {op:?}"))
            }
        },
        OperandShape::Integer { bit_width } => {
            let is_bool = *bit_width == 1;
            match op {
                BinOp::Add => Ok(ResolvedBinaryOp::IntAdd),
                BinOp::And if is_bool => Ok(ResolvedBinaryOp::BoolAnd),
                BinOp::Div => Ok(ResolvedBinaryOp::IntDiv),
                BinOp::Eq => Ok(ResolvedBinaryOp::IntEqual),
                BinOp::Gt => Ok(ResolvedBinaryOp::IntGreater),
                BinOp::GtEq => Ok(ResolvedBinaryOp::IntGreaterEqual),
                BinOp::Lt => Ok(ResolvedBinaryOp::IntLess),
                BinOp::LtEq => Ok(ResolvedBinaryOp::IntLessEqual),
                BinOp::Mod => Ok(ResolvedBinaryOp::IntRem),
                BinOp::Mul => Ok(ResolvedBinaryOp::IntMul),
                BinOp::NotEq => Ok(ResolvedBinaryOp::IntNotEqual),
                BinOp::Or if is_bool => Ok(ResolvedBinaryOp::BoolOr),
                BinOp::Sub => Ok(ResolvedBinaryOp::IntSub),
                BinOp::And | BinOp::Concat | BinOp::Or => {
                    Err("logical operators require bool operands".to_string())
                }
            }
        }
        OperandShape::Pointer => match op {
            BinOp::Eq => Ok(ResolvedBinaryOp::StringEqual),
            BinOp::NotEq => Ok(ResolvedBinaryOp::StringNotEqual),
            _ => Err(format!("unsupported string binary op: {op:?}")),
        },
        OperandShape::Struct { is_enum } => {
            if !is_enum {
                return Err("mismatched types in binary operation".to_string());
            }
            match op {
                BinOp::Eq => Ok(ResolvedBinaryOp::EnumStructEqual { negated: false }),
                BinOp::NotEq => Ok(ResolvedBinaryOp::EnumStructEqual { negated: true }),
                _ => Err("mismatched types in binary operation".to_string()),
            }
        }
    }
}

/// The resolved unary operation to emit.
pub enum ResolvedUnaryOp {
    FloatNeg,
    IntNeg,
    IntNot,
}

/// Pure decision function: given an AST unary operator and the operand shape,
/// returns which concrete operation to emit.
pub fn resolve_unary_op(op: &UnaryOp, shape: &OperandShape) -> Result<ResolvedUnaryOp, String> {
    match (op, shape) {
        (UnaryOp::Neg, OperandShape::Float) => Ok(ResolvedUnaryOp::FloatNeg),
        (UnaryOp::Neg, OperandShape::Integer { .. }) => Ok(ResolvedUnaryOp::IntNeg),
        (UnaryOp::Neg, _) => Err("cannot negate non-numeric value".to_string()),
        (UnaryOp::Not, OperandShape::Integer { .. }) => Ok(ResolvedUnaryOp::IntNot),
        (UnaryOp::Not, _) => Err("cannot apply 'not' to non-integer value".to_string()),
    }
}

/// The resolved compound-assignment operation to emit.
pub enum ResolvedCompoundOp {
    FloatAdd,
    FloatDiv,
    FloatMul,
    FloatSub,
    IntAdd,
    IntDiv,
    IntMul,
    IntSub,
}

/// Pure decision function: given an AST compound operator and the operand
/// shape, returns which concrete operation to emit.
pub fn resolve_compound_op(
    op: &CompoundOp,
    shape: &OperandShape,
) -> Result<ResolvedCompoundOp, String> {
    match shape {
        OperandShape::Float => match op {
            CompoundOp::Add => Ok(ResolvedCompoundOp::FloatAdd),
            CompoundOp::Div => Ok(ResolvedCompoundOp::FloatDiv),
            CompoundOp::Mul => Ok(ResolvedCompoundOp::FloatMul),
            CompoundOp::Sub => Ok(ResolvedCompoundOp::FloatSub),
        },
        OperandShape::Integer { .. } => match op {
            CompoundOp::Add => Ok(ResolvedCompoundOp::IntAdd),
            CompoundOp::Div => Ok(ResolvedCompoundOp::IntDiv),
            CompoundOp::Mul => Ok(ResolvedCompoundOp::IntMul),
            CompoundOp::Sub => Ok(ResolvedCompoundOp::IntSub),
        },
        _ => Err("compound assignment requires matching numeric types".to_string()),
    }
}
