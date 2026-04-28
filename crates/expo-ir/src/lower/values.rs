//! Construct-agnostic lowering of an [`Expr`] to an [`IROperand`].
//!
//! Every construct that needs to thread an expression-shaped value
//! into the IR (terminator conds, instruction operands, etc.) calls
//! [`Lowerer::lower_expr_to_operand`]. The helper dispatches on the
//! expression kind:
//!
//! - Literal -- inline [`IROperand`] constant, no instruction emitted.
//! - Group -- transparent unwrap, recurse on the inner expression.
//! - Binary / Unary -- typed [`IRInstruction::BinaryOp`] /
//!   [`IRInstruction::UnaryOp`] via [`crate::lower::ops`] when the
//!   operator and operand shapes are within the IR vocabulary.
//! - Call -- typed [`IRInstruction::Call`] via
//!   [`crate::lower::calls`] when the callee resolves to a
//!   registered direct symbol; builtins / closures / generics /
//!   struct constructors fall through to Stub.
//! - FieldAccess -- typed [`IRInstruction::FieldLoad`] via
//!   [`crate::lower::fields`] when the receiver type resolves to a
//!   known struct layout.
//! - MethodCall -- typed [`IRInstruction::MethodCall`] via
//!   [`crate::lower::methods`] when the receiver has a static type
//!   and the resolved callee is registered; tail-recursive,
//!   pending-monomorphization, and field-as-closure paths fall
//!   through to Stub.
//! - Anything else -- mint a fresh [`crate::values::IRValueId`], push
//!   an [`IRInstruction::Stub`] onto the caller-supplied instruction
//!   sequence, and return [`IROperand::Local`] referencing the new id.
//!
//! Centralizing the dispatch here keeps the bridging contract uniform
//! across constructs as the IR vocabulary grows: each new
//! [`expo_ast::ast::ExprKind`] that learns to lower retires its
//! [`IRInstruction::Stub`] site by adding a branch above.

use expo_ast::ast::{Expr, ExprKind};

use crate::Lowerer;
use crate::lower::constants::resolve_const;
use crate::resolved::constants::ResolvedConst;
use crate::values::{IRInstruction, IROperand};

impl<'a> Lowerer<'a> {
    /// Lower `expr` to an [`IROperand`].
    ///
    /// Dispatches on [`expo_ast::ast::ExprKind`]: literals -> inline
    /// constants; `Group` -> recurse; `Binary` / `Unary` ->
    /// typed instructions when shapes are supported; `FieldAccess` ->
    /// typed [`IRInstruction::FieldLoad`] when the receiver type
    /// resolves; otherwise -> fresh value id and an
    /// [`IRInstruction::Stub`] bridge.
    ///
    /// The Stub variant is transitional: as each
    /// [`expo_ast::ast::ExprKind`] learns to lower into a typed
    /// instruction, that kind's branch replaces the Stub fallback at
    /// this site.
    pub fn lower_expr_to_operand(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        expr: &Expr,
    ) -> IROperand {
        if let Some(operand) = resolve_const(&expr.kind).and_then(operand_from_const) {
            return operand;
        }

        match &expr.kind {
            ExprKind::Binary { op, left, right } => {
                if let Some(operand) = self.lower_binary_op_or_stub(instructions, op, left, right) {
                    return operand;
                }
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident { name } = &callee.kind
                    && let Some((operand, _)) = self.lower_call_or_stub(instructions, name, args)
                {
                    return operand;
                }
            }
            ExprKind::FieldAccess { receiver, field } => {
                if let Some(operand) =
                    self.lower_field_access_or_stub(instructions, receiver, field)
                {
                    return operand;
                }
            }
            ExprKind::Group { expr: inner } => {
                return self.lower_expr_to_operand(instructions, inner);
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                if let Some((operand, _)) =
                    self.lower_method_call_or_stub(instructions, receiver, method, args)
                {
                    return operand;
                }
            }
            ExprKind::Unary { op, operand } => {
                if let Some(o) = self.lower_unary_op_or_stub(instructions, op, operand) {
                    return o;
                }
            }
            _ => {}
        }

        let dest = self.next_value_id();
        instructions.push(IRInstruction::Stub {
            dest,
            expr: Box::new(expr.clone()),
        });
        IROperand::Local(dest)
    }
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
