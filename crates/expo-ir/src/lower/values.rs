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

use expo_ast::ast::{Expr, ExprKind, Literal};
use expo_typecheck::context::FnParam;
use expo_typecheck::types::{Primitive, Type};

use crate::Lowerer;
use crate::blocks::IRBlockId;
use crate::cfg::CFGBuilder;
use crate::lower::constants::resolve_const;
use crate::resolved::constants::ResolvedConst;
use crate::values::{IRInstruction, IROperand};

/// Outcome of lowering an expression to an operand.
///
/// - `Ok((Some(open), op, ty))`: execution continues at `open`. For
///   pure expressions `open` equals the input; for control-flow
///   expressions it's the merge block. `ty` is the lowerer's
///   published type for the resulting value -- the source of truth
///   for downstream value-typed consumers (notably
///   [`crate::Lowerer::lower_assignment_stmt`]'s
///   `resolve_assigned_type`).
/// - `Ok((None, op, ty))`: every path through this expression
///   terminates (e.g. a `match` whose arms all `return`). The
///   operand is conventionally [`IROperand::Unit`] and unused by the
///   caller; `ty` is conventionally [`Type::Unit`].
/// - `Err(_)`: lowering failure (semantic error).
///
/// Slice 3a-bis (Wave 31) added the `Type` slot. Half the
/// surface ([`Lowerer::lower_call_or_stub`],
/// [`Lowerer::lower_method_call_or_stub`],
/// [`Lowerer::lower_field_access_or_stub`]) was already publishing
/// the operand's type internally -- this contract makes the type
/// part of the universal `lower_expr_to_operand` return so
/// unannotated assignments (`i = self.length() - 1`,
/// `addr = addrs.get(0).unwrap()`, `result = self.work()`) can read
/// the type without falling back to typecheck's often-`Unit`
/// `expr.resolved_type` or the `infer_type_from_expr` static
/// estimator.
pub type OperandResult = Result<(Option<IRBlockId>, IROperand, Type), String>;

impl<'a> Lowerer<'a> {
    /// Lower `expr` into `builder` at `open` and return the new open
    /// block (if any), the produced operand, and the operand's
    /// resolved [`Type`]. See [`OperandResult`] for the contract.
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
        if let Some(constant) = resolve_const(&expr.kind)
            && let Some(operand) = operand_from_const(&constant)
        {
            let ty = const_operand_type(&constant, expr);
            return Ok((Some(open), operand, ty));
        }

        match &expr.kind {
            ExprKind::Binary { op, left, right } => {
                if let Some((open, operand, ty)) =
                    self.lower_binary_op_or_stub(builder, open, op, left, right)?
                {
                    return Ok((open, operand, ty));
                }
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident { name } = &callee.kind
                    && let Some((open, operand, ty)) =
                        self.lower_call_or_stub(builder, open, name, args, tail)?
                {
                    return Ok((open, operand, ty));
                }
            }
            ExprKind::FieldAccess { receiver, field } => {
                if let Some((open, operand, ty)) =
                    self.lower_field_access_or_stub(builder, open, receiver, field)?
                {
                    return Ok((open, operand, ty));
                }
            }
            ExprKind::Group { expr: inner } => {
                return self.lower_expr_to_operand_with_tail(builder, open, inner, tail);
            }
            ExprKind::Ident { name } => {
                if let Some((operand, ty)) = self.lower_ident_or_stub(builder, open, name) {
                    return Ok((Some(open), operand, ty));
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                if let Some((open, operand, ty)) =
                    self.lower_method_call_or_stub(builder, open, receiver, method, args, tail)?
                {
                    return Ok((open, operand, ty));
                }
            }
            ExprKind::Self_ => {
                if let Some((operand, ty)) = self.lower_local_load_or_stub(builder, open, "self") {
                    return Ok((Some(open), operand, ty));
                }
            }
            ExprKind::Unary { op, operand } => {
                if let Some((open, o, ty)) =
                    self.lower_unary_op_or_stub(builder, open, op, operand)?
                {
                    return Ok((open, o, ty));
                }
            }
            _ => {}
        }

        let dest = self.next_value_id();
        let result_type = expr.resolved_type.clone().unwrap_or(Type::Unknown);
        builder.append(
            open,
            IRInstruction::Stub {
                dest,
                expr: Box::new(expr.clone()),
                result_type: result_type.clone(),
            },
        );
        Ok((Some(open), IROperand::Local(dest), result_type))
    }

    /// Lower a sequence of sub-expressions into the same builder,
    /// threading the open block through each call. Bails (returns
    /// `Ok((None, partial_ops))`) as soon as any sub-expression
    /// terminates, leaving the caller free to stop without emitting
    /// the consuming instruction.
    ///
    /// Consumes [`OperandResult`]'s `Type` slot but discards it --
    /// callers (the `lower_call_or_stub` argument-list lift, etc.)
    /// only need the operands. Use the per-expression
    /// [`Self::lower_expr_to_operand`] directly when the type is
    /// also needed.
    pub fn lower_expr_sequence<'b>(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        exprs: impl IntoIterator<Item = &'b Expr>,
    ) -> Result<(Option<IRBlockId>, Vec<IROperand>), String> {
        let mut current = open;
        let mut ops = Vec::new();
        for expr in exprs {
            let (next, op, _ty) = self.lower_expr_to_operand(builder, current, expr)?;
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
    /// caller doesn't need to handle a re-opened cursor. Returns
    /// the operand alongside the binding's resolved [`Type`] so the
    /// universal [`Self::lower_expr_to_operand`] contract can
    /// publish it.
    fn lower_ident_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        name: &str,
    ) -> Option<(IROperand, Type)> {
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
                    ty: ty.clone(),
                },
            );
            return Some((IROperand::Local(dest), ty));
        }

        if let Some(ty) = const_ty {
            let dest = self.next_value_id();
            builder.append(
                open,
                IRInstruction::LoadConst {
                    dest,
                    name: name.to_string(),
                    ty: ty.clone(),
                },
            );
            return Some((IROperand::Local(dest), ty));
        }

        if let Some(fn_type) = fn_type {
            let dest = self.next_value_id();
            builder.append(
                open,
                IRInstruction::MakeFnRef {
                    dest,
                    name: name.to_string(),
                    fn_type: fn_type.clone(),
                },
            );
            return Some((IROperand::Local(dest), fn_type));
        }

        None
    }

    /// Lower a known-local binding to an [`IRInstruction::LoadLocal`].
    /// Used for [`expo_ast::ast::ExprKind::Self_`] (always with
    /// `name = "self"`); shares the local-resolution path with
    /// [`Self::lower_ident_or_stub`]. Returns the operand alongside
    /// the binding's resolved [`Type`].
    fn lower_local_load_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        name: &str,
    ) -> Option<(IROperand, Type)> {
        let ty = self.ctx().locals.type_of(name)?;
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::LoadLocal {
                dest,
                name: name.to_string(),
                ty: ty.clone(),
            },
        );
        Some((IROperand::Local(dest), ty))
    }
}

/// Map a resolved compile-time constant to the [`Type`] it produces
/// at runtime, for [`OperandResult`]'s `Type` slot. Falls back to
/// `expr.resolved_type` when the constant kind doesn't pin a
/// specific primitive (mainly `String` literals, which resolve to
/// `Type::Primitive(String)` either way once typecheck records it).
fn const_operand_type(constant: &ResolvedConst, expr: &Expr) -> Type {
    match constant {
        ResolvedConst::Bool(_) => Type::Primitive(Primitive::Bool),
        ResolvedConst::Float(_) => Type::Primitive(Primitive::F64),
        ResolvedConst::Int(_) => Type::Primitive(Primitive::I64),
        _ => expr.resolved_type.clone().unwrap_or(Type::Unknown),
    }
}

/// Map an [`expo_ast::ast::Literal`] to its inline operand-only
/// [`Type`]. Mirrors [`const_operand_type`] but for the bare-literal
/// case (no [`ResolvedConst`] involved). Currently unused -- kept
/// here so future inline-literal lowerings can call it directly.
#[allow(dead_code)]
fn literal_type(value: &Literal) -> Type {
    match value {
        Literal::Bool(_) => Type::Primitive(Primitive::Bool),
        Literal::Float(_) => Type::Primitive(Primitive::F64),
        Literal::Int(_) => Type::Primitive(Primitive::I64),
        Literal::String(_) => Type::Primitive(Primitive::String),
        Literal::Unit => Type::Unit,
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
fn operand_from_const(constant: &ResolvedConst) -> Option<IROperand> {
    match constant {
        ResolvedConst::Bool(b) => Some(IROperand::ConstBool(*b)),
        ResolvedConst::Float(v) => Some(IROperand::ConstFloat(*v)),
        ResolvedConst::Int(v) => Some(IROperand::ConstInt(*v)),
        ResolvedConst::EnumVariant { .. }
        | ResolvedConst::String(_)
        | ResolvedConst::Struct { .. } => None,
    }
}
