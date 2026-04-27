//! Construct-agnostic lowering of an [`Expr`] to an [`IROperand`].
//!
//! Every construct that needs to thread an expression-shaped value
//! into the IR (terminator conds, instruction operands, etc.) calls
//! [`lower_expr_to_operand`]. The helper recognizes literal-shaped
//! expressions and returns an inline [`IROperand`] constant; for any
//! other shape it mints a fresh [`crate::values::IRValueId`], pushes
//! an [`IRInstruction::Stub`] onto the caller-supplied instruction
//! sequence, and returns [`IROperand::Local`] referencing the new id.
//!
//! Centralizing the literal-fast-path / Stub-fallback choice here
//! keeps the bridging contract uniform across constructs as the
//! lowering ladder progresses (slice 2's `lower_if` will call into
//! this helper unchanged).

use expo_ast::ast::Expr;

use crate::FnLowerState;
use crate::lower::constants::resolve_const;
use crate::resolved::constants::ResolvedConst;
use crate::values::{IRInstruction, IROperand};

/// Lower `expr` to an [`IROperand`].
///
/// Literal expressions (`true`, `42`, `3.14`, `"hi"`) become inline
/// [`IROperand`] constants and emit no instructions. Any other
/// expression mints a fresh value id, pushes an
/// [`IRInstruction::Stub`] onto `instructions`, and returns
/// [`IROperand::Local(dest)`].
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
/// literals are construction operations, not operands -- they fall
/// through to the Stub bridge until their construction instructions
/// land).
fn operand_from_const(constant: ResolvedConst) -> Option<IROperand> {
    match constant {
        ResolvedConst::Bool(b) => Some(IROperand::ConstBool(b)),
        ResolvedConst::Float(v) => Some(IROperand::ConstFloat(v)),
        ResolvedConst::Int(v) => Some(IROperand::ConstInt(v)),
        ResolvedConst::String(s) => Some(IROperand::ConstStr(s)),
        ResolvedConst::EnumVariant { .. } | ResolvedConst::Struct { .. } => None,
    }
}
