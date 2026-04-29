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
//! - FieldAccess -- typed [`IRInstruction::FieldChain`] when
//!   [`crate::lower::fields::resolve_chain_steps`] succeeds (chains
//!   rooted at a named local), else [`IRInstruction::FieldLoad`].
//! - Ident -- typed [`IRInstruction::LoadLocal`] /
//!   [`IRInstruction::LoadConst`] / [`IRInstruction::MakeFnRef`]
//!   based on the same precedence `compile_expr` uses (locals first,
//!   then module constants, then function-as-value).
//! - MethodCall -- typed [`IRInstruction::MethodCall`] via
//!   [`crate::lower::methods`] when the receiver has a static type
//!   and the resolved callee is registered; tail-recursive,
//!   pending-monomorphization, and field-as-closure paths fall
//!   through to Stub.
//! - Self_ -- typed [`IRInstruction::LoadLocal`] for the implicit
//!   `"self"` binding bound by impl-method entry.
//! - Anything else -- mint a fresh [`crate::values::IRValueId`], push
//!   an [`IRInstruction::Stub`] onto the caller-supplied instruction
//!   sequence, and return [`IROperand::Local`] referencing the new id.
//!
//! Centralizing the dispatch here keeps the bridging contract uniform
//! across constructs as the IR vocabulary grows: each new
//! [`expo_ast::ast::ExprKind`] that learns to lower retires its
//! [`IRInstruction::Stub`] site by adding a branch above.

use expo_ast::ast::{Expr, ExprKind};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::Type;

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
        self.lower_expr_to_operand_with_tail(instructions, expr, false)
    }

    /// Lower `expr` to an [`IROperand`] in *tail context*: if `expr`
    /// is (transparently through `Group`) a direct
    /// [`ExprKind::Call`] / [`ExprKind::MethodCall`], the emitted
    /// [`IRInstruction::Call`] / [`IRInstruction::MethodCall`] gets
    /// `tail = true`. Every other expression kind defers to the
    /// non-tail [`Self::lower_expr_to_operand`].
    ///
    /// Replaces the legacy ambient `FnLowerState::tail_position` flag
    /// (Slice 6 Wave 25). Tail context is now first-class IR data
    /// rather than a per-function global; only the immediately-emitted
    /// call carries it. Subexpressions of the call (its receiver and
    /// arguments) are non-tail by definition (they evaluate before
    /// the call returns), so they always go through the non-tail
    /// path.
    ///
    /// Use from the codegen seam at the two source sites that mark
    /// tail position: [`Statement::Return`] and the
    /// last-statement-implicit-return in `compile_function_body`.
    pub fn lower_tail_expr_to_operand(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        expr: &Expr,
    ) -> IROperand {
        self.lower_expr_to_operand_with_tail(instructions, expr, true)
    }

    fn lower_expr_to_operand_with_tail(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        expr: &Expr,
        tail: bool,
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
                    && let Some((operand, _)) =
                        self.lower_call_or_stub(instructions, name, args, tail)
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
                return self.lower_expr_to_operand_with_tail(instructions, inner, tail);
            }
            ExprKind::Ident { name } => {
                if let Some(operand) = self.lower_ident_or_stub(instructions, name) {
                    return operand;
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                if let Some((operand, _)) =
                    self.lower_method_call_or_stub(instructions, receiver, method, args, tail)
                {
                    return operand;
                }
            }
            ExprKind::Self_ => {
                if let Some(operand) = self.lower_local_load_or_stub(instructions, "self") {
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

    /// Lower an [`expo_ast::ast::ExprKind::Ident`] to a typed
    /// instruction matching `compile_expr`'s precedence: in-scope
    /// local binding -> module constant -> function-as-value
    /// (closure-compatible fat pointer). Returns `None` when the
    /// name resolves to none of the three (well-typed code never
    /// reaches that branch, but defensively keep the Stub bridge).
    fn lower_ident_or_stub(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        name: &str,
    ) -> Option<IROperand> {
        let (local_ty, const_ty, fn_type) = {
            let ctx = self.ctx();
            let local_ty = ctx.locals.type_of(name);
            let const_ty = ctx.type_ctx.constants.get(name).cloned();
            let fn_type = ctx.type_ctx.functions.get(name).map(|sig| Type::Function {
                params: sig.params.iter().map(FnParam::from).collect(),
                return_type: Box::new(sig.return_type.clone()),
            });
            (local_ty, const_ty, fn_type)
        };

        if let Some(ty) = local_ty {
            let dest = self.next_value_id();
            instructions.push(IRInstruction::LoadLocal {
                dest,
                name: name.to_string(),
                ty,
            });
            return Some(IROperand::Local(dest));
        }

        if let Some(ty) = const_ty {
            let dest = self.next_value_id();
            instructions.push(IRInstruction::LoadConst {
                dest,
                name: name.to_string(),
                ty,
            });
            return Some(IROperand::Local(dest));
        }

        if let Some(fn_type) = fn_type {
            let dest = self.next_value_id();
            instructions.push(IRInstruction::MakeFnRef {
                dest,
                name: name.to_string(),
                fn_type,
            });
            return Some(IROperand::Local(dest));
        }

        None
    }

    /// Lower a known-local binding to an [`IRInstruction::LoadLocal`].
    /// Used for [`expo_ast::ast::ExprKind::Self_`] (always with
    /// `name = "self"`); shares the local-resolution path with
    /// [`Self::lower_ident_or_stub`].
    fn lower_local_load_or_stub(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        name: &str,
    ) -> Option<IROperand> {
        let ty = self.ctx().locals.type_of(name)?;
        let dest = self.next_value_id();
        instructions.push(IRInstruction::LoadLocal {
            dest,
            name: name.to_string(),
            ty,
        });
        Some(IROperand::Local(dest))
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
