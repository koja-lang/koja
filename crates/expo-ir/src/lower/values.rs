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
//!   an [`IRInstruction::Stub`] onto the open block, and return
//!   [`IROperand::Local`] referencing the new id.
//!
//! ## Recursive `CFGBuilder` shape (Slice 3)
//!
//! Every operand-producing helper takes `(&mut CFGBuilder, IRBlockId)`
//! and returns [`OperandResult`]: an `Option<IRBlockId>` (the block
//! to continue lowering into, or `None` if all paths terminated) plus
//! the produced [`IROperand`]. Pure expressions return the same
//! `open` they were given; control-flow expressions return the merge
//! block they minted.
//!
//! Centralizing the dispatch here keeps the bridging contract uniform
//! across constructs as the IR vocabulary grows: each new
//! [`expo_ast::ast::ExprKind`] that learns to lower retires its
//! [`IRInstruction::Stub`] site by adding a branch above.

use expo_ast::ast::{Expr, ExprKind};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::Type;

use crate::Lowerer;
use crate::blocks::IRBlockId;
use crate::cfg::CFGBuilder;
use crate::lower::constants::resolve_const;
use crate::resolved::constants::ResolvedConst;
use crate::values::{IRInstruction, IROperand};

/// Outcome of lowering an expression to an operand.
///
/// - `Ok((Some(open), op))`: execution continues at `open`. For pure
///   expressions `open` equals the input; for control-flow expressions
///   it's the merge block.
/// - `Ok((None, op))`: every path through this expression terminates
///   (e.g. a `match` whose arms all `return`). The operand is
///   conventionally [`IROperand::Unit`] and unused by the caller.
/// - `Err(_)`: lowering failure (semantic error).
pub type OperandResult = Result<(Option<IRBlockId>, IROperand), String>;

impl<'a> Lowerer<'a> {
    /// Lower `expr` into `builder` at `open` and return the new open
    /// block (if any) plus the produced operand. See [`OperandResult`]
    /// for the contract.
    pub fn lower_expr_to_operand(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
    ) -> OperandResult {
        self.lower_expr_to_operand_with_tail(builder, open, expr, false)
    }

    /// Lower `expr` in *tail context*: if `expr` is (transparently
    /// through `Group`) a direct [`ExprKind::Call`] /
    /// [`ExprKind::MethodCall`], the emitted [`IRInstruction::Call`] /
    /// [`IRInstruction::MethodCall`] gets `tail = true`. Every other
    /// expression kind defers to the non-tail variant.
    ///
    /// Use from the source sites that mark tail position:
    /// [`expo_ast::ast::Statement::Return`] and the
    /// last-statement-implicit-return in
    /// [`Self::lower_function_body`].
    pub fn lower_tail_expr_to_operand(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
    ) -> OperandResult {
        self.lower_expr_to_operand_with_tail(builder, open, expr, true)
    }

    fn lower_expr_to_operand_with_tail(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
        tail: bool,
    ) -> OperandResult {
        if let Some(operand) = resolve_const(&expr.kind).and_then(operand_from_const) {
            return Ok((Some(open), operand));
        }

        match &expr.kind {
            ExprKind::Binary { op, left, right } => {
                if let Some((open, operand)) =
                    self.lower_binary_op_or_stub(builder, open, op, left, right)?
                {
                    return Ok((open, operand));
                }
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident { name } = &callee.kind
                    && let Some((open, operand, _)) =
                        self.lower_call_or_stub(builder, open, name, args, tail)?
                {
                    return Ok((open, operand));
                }
            }
            ExprKind::FieldAccess { receiver, field } => {
                if let Some((open, operand)) =
                    self.lower_field_access_or_stub(builder, open, receiver, field)?
                {
                    return Ok((open, operand));
                }
            }
            ExprKind::Group { expr: inner } => {
                return self.lower_expr_to_operand_with_tail(builder, open, inner, tail);
            }
            ExprKind::Ident { name } => {
                if let Some(operand) = self.lower_ident_or_stub(builder, open, name) {
                    return Ok((Some(open), operand));
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                if let Some((open, operand, _)) =
                    self.lower_method_call_or_stub(builder, open, receiver, method, args, tail)?
                {
                    return Ok((open, operand));
                }
            }
            ExprKind::Self_ => {
                if let Some(operand) = self.lower_local_load_or_stub(builder, open, "self") {
                    return Ok((Some(open), operand));
                }
            }
            ExprKind::Unary { op, operand } => {
                if let Some((open, o)) = self.lower_unary_op_or_stub(builder, open, op, operand)? {
                    return Ok((open, o));
                }
            }
            _ => {}
        }

        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::Stub {
                dest,
                expr: Box::new(expr.clone()),
            },
        );
        Ok((Some(open), IROperand::Local(dest)))
    }

    /// Lower a sequence of sub-expressions into the same builder,
    /// threading the open block through each call. Bails (returns
    /// `Ok((None, partial_ops))`) as soon as any sub-expression
    /// terminates, leaving the caller free to stop without emitting
    /// the consuming instruction.
    pub fn lower_expr_sequence<'b>(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        exprs: impl IntoIterator<Item = &'b Expr>,
    ) -> Result<(Option<IRBlockId>, Vec<IROperand>), String> {
        let mut current = open;
        let mut ops = Vec::new();
        for expr in exprs {
            let (next, op) = self.lower_expr_to_operand(builder, current, expr)?;
            ops.push(op);
            let Some(next) = next else {
                return Ok((None, ops));
            };
            current = next;
        }
        Ok((Some(current), ops))
    }

    /// Lower an [`expo_ast::ast::ExprKind::Ident`] to a typed
    /// instruction matching `compile_expr`'s precedence: in-scope
    /// local binding -> module constant -> function-as-value
    /// (closure-compatible fat pointer). Returns `None` when the
    /// name resolves to none of the three (well-typed code never
    /// reaches that branch, but defensively keep the Stub bridge).
    ///
    /// Pure-expression: same `open` block on the way out, so the
    /// caller doesn't need to handle a re-opened cursor.
    fn lower_ident_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
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
            builder.append(
                open,
                IRInstruction::LoadLocal {
                    dest,
                    name: name.to_string(),
                    ty,
                },
            );
            return Some(IROperand::Local(dest));
        }

        if let Some(ty) = const_ty {
            let dest = self.next_value_id();
            builder.append(
                open,
                IRInstruction::LoadConst {
                    dest,
                    name: name.to_string(),
                    ty,
                },
            );
            return Some(IROperand::Local(dest));
        }

        if let Some(fn_type) = fn_type {
            let dest = self.next_value_id();
            builder.append(
                open,
                IRInstruction::MakeFnRef {
                    dest,
                    name: name.to_string(),
                    fn_type,
                },
            );
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
        builder: &mut CFGBuilder,
        open: IRBlockId,
        name: &str,
    ) -> Option<IROperand> {
        let ty = self.ctx().locals.type_of(name)?;
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::LoadLocal {
                dest,
                name: name.to_string(),
                ty,
            },
        );
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
