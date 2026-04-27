//! Construct-agnostic lowering of an [`Expr`] to an [`IROperand`].
//!
//! Every construct that needs to thread an expression-shaped value
//! into the IR (terminator conds, instruction operands, etc.) calls
//! [`lower_expr_to_operand`]. The helper dispatches on the expression
//! kind:
//!
//! - Literal -- inline [`IROperand`] constant, no instruction emitted.
//! - Group -- transparent unwrap, recurse on the inner expression.
//! - Binary / Unary -- typed [`IRInstruction::BinaryOp`] /
//!   [`IRInstruction::UnaryOp`] via [`crate::lower::ops`] when the
//!   operator and operand shapes are within the IR vocabulary.
//! - Anything else -- mint a fresh [`crate::values::IRValueId`], push
//!   an [`IRInstruction::Stub`] onto the caller-supplied instruction
//!   sequence, and return [`IROperand::Local`] referencing the new id.
//!
//! Centralizing the dispatch here keeps the bridging contract uniform
//! across constructs as the IR vocabulary grows: each new
//! [`expo_ast::ast::ExprKind`] that learns to lower retires its
//! [`IRInstruction::Stub`] site by adding a branch above.

use expo_ast::ast::{Expr, ExprKind};

use crate::FnLowerState;
use crate::lower::constants::resolve_const;
use crate::lower::ops::{lower_binary_op_or_stub, lower_unary_op_or_stub};
use crate::resolved::constants::ResolvedConst;
use crate::values::{IRInstruction, IROperand};

/// Lower `expr` to an [`IROperand`].
///
/// Dispatches on [`expo_ast::ast::ExprKind`]: literals -> inline
/// constants; `Group` -> recurse; `Binary` / `Unary` -> typed
/// instructions when shapes are supported; otherwise -> fresh value
/// id and an [`IRInstruction::Stub`] bridge.
///
/// The Stub variant is transitional: as each [`expo_ast::ast::ExprKind`]
/// learns to lower into a typed instruction, that kind's branch
/// replaces the Stub fallback at this site.
pub fn lower_expr_to_operand(
    state: &mut FnLowerState,
    instructions: &mut Vec<IRInstruction>,
    expr: &Expr,
) -> IROperand {
    if let Some(operand) = resolve_const(&expr.kind).and_then(operand_from_const) {
        return operand;
    }

    match &expr.kind {
        ExprKind::Group { expr: inner } => {
            return lower_expr_to_operand(state, instructions, inner);
        }
        ExprKind::Binary { op, left, right } => {
            if let Some(operand) = lower_binary_op_or_stub(state, instructions, op, left, right) {
                return operand;
            }
        }
        ExprKind::Unary { op, operand } => {
            if let Some(o) = lower_unary_op_or_stub(state, instructions, op, operand) {
                return o;
            }
        }
        _ => {}
    }

    let dest = state.next_value_id();
    instructions.push(IRInstruction::Stub {
        dest,
        expr: Box::new(expr.clone()),
    });
    IROperand::Local(dest)
}

/// Maps a [`ResolvedConst`] to the corresponding inline [`IROperand`]
/// constant. Returns `None` for resolved kinds that aren't pure
/// operand-shaped values (enum variant constructors and struct
/// literals are construction operations, not operands), and for
/// kinds whose materialization seam isn't wired up yet:
///
/// - `ResolvedConst::String` -- string materialization requires
///   runtime allocation (`Compiler::compile_string`'s lifecycle); the
///   `materialize_operand` seam doesn't carry that today. String
///   literals fall through to the `IRInstruction::Stub` bridge,
///   which routes through `compile_expr`'s established string path.
fn operand_from_const(constant: ResolvedConst) -> Option<IROperand> {
    match constant {
        ResolvedConst::Bool(b) => Some(IROperand::ConstBool(b)),
        ResolvedConst::Float(v) => Some(IROperand::ConstFloat(v)),
        ResolvedConst::Int(v) => Some(IROperand::ConstInt(v)),
        ResolvedConst::EnumVariant { .. }
        | ResolvedConst::String(_)
        | ResolvedConst::Struct { .. } => None,
    }
}
