//! Operator and literal translation helpers. Pure functions over
//! AST nodes — no [`super::ctx::FnLowerCtx`] needed; callers in
//! [`super::expr`] handle block / value bookkeeping after these
//! return.
//!
//! Three concerns live here together because they form the "AST
//! vocabulary → IR vocabulary" border for non-control-flow constructs:
//!
//! - [`lower_literal`] / [`lower_bin_op`] / [`lower_unary_op`] —
//!   surface-syntax → IR-enum mapping, with diagnostics on feature
//!   gaps (Float / String literals, `<>` concat).
//! - [`const_value_type`] — `ConstValue` variant → `IRType` width.
//! - [`bin_op_result_type`] / [`unary_op_result_type`] — typed-result
//!   inference: comparisons / boolean logic always produce `Bool`,
//!   arithmetic and `Neg` preserve operand width.

use expo_ast::ast::{BinOp, Diagnostic, Literal, UnaryOp};
use expo_ast::span::Span;

use crate::types::{ConstValue, IRBinOp, IRType, IRUnaryOp};

pub(super) fn lower_literal(
    value: &Literal,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ConstValue, ()> {
    match value {
        Literal::Bool(b) => Ok(ConstValue::Bool(*b)),
        // Slice scope: every Float literal lowers to the 64-bit
        // variant (matches v1's `Float == Float64` alias). Width
        // inference for `f: Float32 = 3.14`-style coercion lands
        // with the annotation pass.
        Literal::Float(text) => match text.parse::<f64>() {
            Ok(parsed) => Ok(ConstValue::Float64(parsed)),
            Err(err) => {
                diagnostics.push(Diagnostic::error(
                    format!("invalid Float literal `{text}`: {err}"),
                    span,
                ));
                Err(())
            }
        },
        // Slice scope: every Int literal lowers to the 64-bit signed
        // variant. Once stdlib stubs grow `Int8`..`UInt64` and literal
        // width inference lands, this match grows arms (or threads
        // expected width through from typecheck).
        Literal::Int(text) => match parse_int_literal(text) {
            Ok(parsed) => Ok(ConstValue::Int64(parsed)),
            Err(detail) => {
                diagnostics.push(Diagnostic::error(
                    format!("invalid Int literal `{text}`: {detail}"),
                    span,
                ));
                Err(())
            }
        },
        // Used by `match`-arm literal patterns; expression-position
        // strings still flow through `ExprKind::String`'s own lower.
        Literal::String(text) => Ok(ConstValue::String(text.clone())),
        Literal::Unit => Ok(ConstValue::Unit),
    }
}

/// Parse an `IntLit` token's raw text into `i64`. The lexer
/// preserves prefixes (`0x` / `0b`) and underscore separators
/// verbatim, but `i64::from_str` is decimal-only and rejects both —
/// strip underscores first, then dispatch to the right radix based
/// on the prefix. `0X` / `0B` are accepted to match the lexer,
/// which treats them identically to the lowercase forms.
fn parse_int_literal(text: &str) -> Result<i64, String> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else if let Some(bin) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        i64::from_str_radix(bin, 2).map_err(|e| e.to_string())
    } else {
        cleaned.parse::<i64>().map_err(|e| e.to_string())
    }
}

pub(super) fn lower_bin_op(
    op: BinOp,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<IRBinOp, ()> {
    match op {
        BinOp::Add => Ok(IRBinOp::Add),
        BinOp::And => Ok(IRBinOp::And),
        BinOp::Div => Ok(IRBinOp::Div),
        BinOp::Eq => Ok(IRBinOp::Eq),
        BinOp::Gt => Ok(IRBinOp::Gt),
        BinOp::GtEq => Ok(IRBinOp::GtEq),
        BinOp::Lt => Ok(IRBinOp::Lt),
        BinOp::LtEq => Ok(IRBinOp::LtEq),
        BinOp::Mod => Ok(IRBinOp::Mod),
        BinOp::Mul => Ok(IRBinOp::Mul),
        BinOp::NotEq => Ok(IRBinOp::NotEq),
        BinOp::Or => Ok(IRBinOp::Or),
        BinOp::Sub => Ok(IRBinOp::Sub),
        // `<>` concat doesn't reach this helper — the expression
        // lowerer intercepts `BinOp::Concat` and emits
        // [`IRInstruction::Concat`] directly. If we land here, the
        // dispatcher branched incorrectly; surface a hard error so
        // the caller fails fast rather than silently miscompiling.
        BinOp::Concat => {
            diagnostics.push(Diagnostic::error(
                "alpha IR lower: `<>` concat must route through `IRInstruction::Concat`, \
                 not `lower_bin_op` — caller dispatch bug",
                span,
            ));
            Err(())
        }
    }
}

pub(super) fn lower_unary_op(op: UnaryOp) -> IRUnaryOp {
    match op {
        UnaryOp::Neg => IRUnaryOp::Neg,
        UnaryOp::Not => IRUnaryOp::Not,
    }
}

/// Map a [`ConstValue`] variant to its [`IRType`]. Pure
/// transliteration — each integer / float width gets its mirroring
/// type, and `Bool` / `String` / `Unit` round-trip directly.
pub(super) fn const_value_type(value: &ConstValue) -> IRType {
    match value {
        ConstValue::Binary(_) => IRType::Binary,
        ConstValue::Bits { .. } => IRType::Bits,
        ConstValue::Bool(_) => IRType::Bool,
        ConstValue::Float32(_) => IRType::Float32,
        ConstValue::Float64(_) => IRType::Float64,
        ConstValue::Int8(_) => IRType::Int8,
        ConstValue::Int16(_) => IRType::Int16,
        ConstValue::Int32(_) => IRType::Int32,
        ConstValue::Int64(_) => IRType::Int64,
        ConstValue::String(_) => IRType::String,
        ConstValue::UInt8(_) => IRType::UInt8,
        ConstValue::UInt16(_) => IRType::UInt16,
        ConstValue::UInt32(_) => IRType::UInt32,
        ConstValue::UInt64(_) => IRType::UInt64,
        ConstValue::Unit => IRType::Unit,
    }
}

/// The result type of a [`IRBinOp`] given the operand type.
/// Comparisons and boolean logic always produce `Bool`; arithmetic
/// preserves the operand width (typecheck guarantees both operands
/// share a width).
pub(super) fn bin_op_result_type(op: IRBinOp, operand_ty: IRType) -> IRType {
    match op {
        IRBinOp::Add | IRBinOp::Sub | IRBinOp::Mul | IRBinOp::Div | IRBinOp::Mod => operand_ty,
        IRBinOp::And
        | IRBinOp::Or
        | IRBinOp::Eq
        | IRBinOp::NotEq
        | IRBinOp::Gt
        | IRBinOp::GtEq
        | IRBinOp::Lt
        | IRBinOp::LtEq => IRType::Bool,
    }
}

/// The result type of a [`IRUnaryOp`] given the operand type. `Neg`
/// preserves the operand width; `Not` is always `Bool`.
pub(super) fn unary_op_result_type(op: IRUnaryOp, operand_ty: IRType) -> IRType {
    match op {
        IRUnaryOp::Neg => operand_ty,
        IRUnaryOp::Not => IRType::Bool,
    }
}
