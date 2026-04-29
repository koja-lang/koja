//! Statement-level lowering: turns AST [`Statement`] values into the
//! [`IRInstruction`] sequences and optional [`IRTerminator`]s that
//! drive the executor seam.
//!
//! Phase 4g Slice 1 introduces this module as the single statement
//! lowering surface. The codegen-side [`crate::lower::statements::Lowerer`]'s
//! [`Lowerer::lower_statement`] / [`Lowerer::lower_statements`]
//! methods replace the per-construct AST walks the legacy
//! `compile_assignment` / `compile_compound_assign` /
//! `compile_field_assignment` family did directly against LLVM.
//!
//! Per-statement breakdown:
//!
//! - [`Statement::Expr`] -- lower the inner expression, discard its
//!   operand. Side-effecting calls remain side-effecting through
//!   [`Lowerer::lower_expr_to_operand`].
//! - [`Statement::Assignment`] -- emit either an
//!   [`IRInstruction::StoreLocal`] (single-segment lvalue / pattern
//!   binding) or an [`IRInstruction::StoreField`] (multi-segment
//!   chain), preceded by an optional [`IRInstruction::UnionWrap`]
//!   when typecheck recorded a [`Coercion::UnionWiden`] for the RHS
//!   span.
//! - [`Statement::CompoundAssign`] -- lower `target op= value` into
//!   load-current + binary-op + store-back, sharing the existing
//!   [`IRInstruction::BinaryOp`] machinery via the
//!   [`compound_to_binary`] mapper.
//! - [`Statement::Return`] -- lower the return value through the
//!   tail-context expr lifter (so direct calls in tail position get
//!   `tail = true`), wrap it through [`IRInstruction::UnionWrap`]
//!   when widening, and finish with an [`IRTerminator::Return`].
//! - [`Statement::Break`] -- finish with an [`IRTerminator::Branch`]
//!   targeting the innermost loop's exit (read from
//!   [`crate::FnLowerState::loop_exit`]).
//!
//! ## Deferred: list-literal protocol coercion
//!
//! Assignment of an [`expo_ast::ast::ExprKind::List`] literal to a
//! non-`List` target type that implements `ListLiteral<T>` (e.g.
//! `Set<Int> = [1, 2, 3]`) needs `target.from_list(value)` to be
//! monomorphized on demand. The codegen-side `monomorphize_impl_method`
//! pulls in LLVM declaration as a side effect, so triggering it from
//! lowering would mean lowering reaches back into codegen -- the
//! opposite of the Phase 4g end-state where codegen is a pure
//! consumer of `IRProgram`. Solving this properly needs typecheck to
//! record the coercion structurally and a pre-codegen elaboration
//! pass to grow `IRProgram` to a closed set; both land alongside the
//! function-body lift in Slice 3.
//!
//! Until then the [`crate::compile_statement`] shim intercepts
//! `Statement::Assignment` whose `value.kind == ExprKind::List` and
//! routes it through the legacy `compile_assignment` path. Lowering
//! treats list-literal assignments as ordinary stores; if one slips
//! past the shim it stores the raw `List<T>` value into the target
//! slot, which fails LLVM type validation -- a loud signal that the
//! shim's intercept is missing a case.

use expo_ast::ast::{
    AssignTarget, CompoundOp, Expr, ExprKind, LValue, Pattern, Statement, TypeExpr,
};
use expo_typecheck::context::Coercion;
use expo_typecheck::types::{Primitive, Type};

use crate::Lowerer;
use crate::blocks::IRTerminator;
use crate::lower::inference::infer_type_from_expr;
use crate::lower::ownership::ownership_for_expr;
use crate::lower::stmt::{resolve_coercion, resolve_field_path, resolve_final_annotation_type};
use crate::resolved::ops::{
    OperandShape, ResolvedBinaryOp, ResolvedCompoundOp, resolve_compound_op,
};
use crate::values::{IRInstruction, IROperand};

/// Output of lowering a single [`Statement`]: the instruction
/// sequence to append to the current basic block's body, plus an
/// optional terminator (set for `Return` and `Break`).
type LoweredStatement = (Vec<IRInstruction>, Option<IRTerminator>);

/// Output of [`Lowerer::compound_assign_target`]: the target's
/// resolved Expo type, the SSA operand carrying the loaded current
/// value, and a thunk that consumes the post-op result + its type
/// to produce the matching `Store{Local,Field}` instruction. The
/// thunk lets the caller stage the binary-op emission between the
/// load and the store without re-resolving the target.
type CompoundAssignTarget = (
    Type,
    IROperand,
    Box<dyn FnOnce(IROperand, Type) -> IRInstruction>,
);

impl<'a> Lowerer<'a> {
    /// Lower one [`Statement`] into instructions plus an optional
    /// terminator. The terminator is `Some` only for `Return` and
    /// `Break`; the other variants leave the basic block open for
    /// the next statement.
    pub fn lower_statement(&mut self, stmt: &Statement) -> Result<LoweredStatement, String> {
        match stmt {
            Statement::Assignment {
                target,
                type_annotation,
                value,
                ..
            } => self.lower_assignment_stmt(target, type_annotation.as_ref(), value),
            Statement::Break { .. } => self.lower_break_stmt(),
            Statement::CompoundAssign {
                target, op, value, ..
            } => self.lower_compound_assign_stmt(target, op, value),
            Statement::Expr(expr) => Ok(self.lower_expr_stmt(expr)),
            Statement::Return { value, .. } => Ok(self.lower_return_stmt(value.as_ref())),
        }
    }

    /// Lower a sequence of statements into a single instruction
    /// stream plus an optional trailing terminator. The terminator
    /// only ever comes from the last statement; if any earlier
    /// statement produces one (defensively, since `Return` / `Break`
    /// must syntactically be last in well-typed code), iteration
    /// halts and the terminator is returned.
    pub fn lower_statements(&mut self, stmts: &[Statement]) -> Result<LoweredStatement, String> {
        let mut instructions = Vec::new();
        for stmt in stmts {
            let (mut stmt_instructions, terminator) = self.lower_statement(stmt)?;
            instructions.append(&mut stmt_instructions);
            if terminator.is_some() {
                return Ok((instructions, terminator));
            }
        }
        Ok((instructions, None))
    }

    /// Lower an [`Statement::Expr`]: append whatever instructions
    /// the expression's lowering needs (typically a single direct
    /// instruction or a Stub bridge) and discard the resulting
    /// operand.
    fn lower_expr_stmt(&mut self, expr: &Expr) -> LoweredStatement {
        let mut instructions = Vec::new();
        let _ = self.lower_expr_to_operand(&mut instructions, expr);
        (instructions, None)
    }

    /// Lower an [`Statement::Assignment`]: optionally push the
    /// annotation's type-subst entries into `fn_lower.type_subst`
    /// for the duration of RHS lowering, lower the value, optionally
    /// emit an [`IRInstruction::UnionWrap`] for a recorded
    /// `UnionWiden` coercion, then emit a [`IRInstruction::StoreLocal`]
    /// (single-segment) or [`IRInstruction::StoreField`]
    /// (multi-segment) sink.
    fn lower_assignment_stmt(
        &mut self,
        target: &AssignTarget,
        type_annotation: Option<&TypeExpr>,
        value: &Expr,
    ) -> Result<LoweredStatement, String> {
        // Annotation-driven type-subst push/pop happens at the
        // [`crate::compile_statement`] shim layer rather than here.
        // The shim wraps both lowering and execution in the
        // pushed-subst window so any [`IRInstruction::Stub`] emitted
        // for unsupported sub-expressions still sees the entries when
        // its deferred `compile_expr` runs (e.g. `List<Int>::new()`'s
        // type-arg inference reads from `fn_lower.type_subst`).
        let mut instructions = Vec::new();
        let mut value_operand = self.lower_expr_to_operand(&mut instructions, value);
        let assigned_type = self.resolve_assigned_type(type_annotation, value);
        value_operand = self.maybe_emit_union_wrap(&mut instructions, value, value_operand);

        let store = self.build_store(target, value, &assigned_type, value_operand)?;
        instructions.push(store);
        Ok((instructions, None))
    }

    /// Lower an [`Statement::CompoundAssign`] (`target op= value`):
    /// load the current value, lower the RHS, derive the operand
    /// shape from the target's Expo type, look up the resolved
    /// compound op, emit a [`IRInstruction::BinaryOp`], and store
    /// the result back into the target.
    fn lower_compound_assign_stmt(
        &mut self,
        target: &LValue,
        op: &CompoundOp,
        value: &Expr,
    ) -> Result<LoweredStatement, String> {
        let mut instructions = Vec::new();
        let (target_type, load_op, sink) =
            self.compound_assign_target(target, &mut instructions)?;
        let rhs_op = self.lower_expr_to_operand(&mut instructions, value);

        let shape = operand_shape_for_type(&target_type)
            .ok_or("compound assignment requires matching numeric types")?;
        let resolved = resolve_compound_op(op, &shape)?;
        let dest = self.next_value_id();
        instructions.push(IRInstruction::BinaryOp {
            dest,
            op: compound_to_binary(&resolved),
            lhs: load_op,
            rhs: rhs_op,
        });
        instructions.push(sink(IROperand::Local(dest), target_type));
        Ok((instructions, None))
    }

    /// Lower a [`Statement::Return`]: in the `Some(expr)` case lift
    /// the value through the tail-context lifter (so direct calls
    /// inherit `tail = true`), optionally wrap into a widening
    /// union, and capture the binding name to skip in the pre-
    /// return drop pass when the expression is a bare ident.
    fn lower_return_stmt(&mut self, value: Option<&Expr>) -> LoweredStatement {
        let Some(expr) = value else {
            return (
                Vec::new(),
                Some(IRTerminator::Return {
                    value: None,
                    drop_skip: None,
                }),
            );
        };
        let mut instructions = Vec::new();
        let mut operand = self.lower_tail_expr_to_operand(&mut instructions, expr);
        operand = self.maybe_emit_union_wrap(&mut instructions, expr, operand);
        let drop_skip = match &expr.kind {
            ExprKind::Ident { name, .. } => Some(name.clone()),
            _ => None,
        };
        (
            instructions,
            Some(IRTerminator::Return {
                value: Some(operand),
                drop_skip,
            }),
        )
    }

    /// Lower a [`Statement::Break`]: finish the block with an
    /// unconditional branch to the innermost enclosing loop's exit
    /// id, read from [`crate::FnLowerState::current_loop_exit`].
    fn lower_break_stmt(&mut self) -> Result<LoweredStatement, String> {
        let exit = self
            .fn_state
            .current_loop_exit()
            .ok_or("break outside of loop")?;
        Ok((Vec::new(), Some(IRTerminator::Branch(exit))))
    }

    /// Resolve the post-RHS assigned type: annotation > inferred
    /// from typecheck > inferred from expression kind > Unknown.
    fn resolve_assigned_type(&self, type_annotation: Option<&TypeExpr>, value: &Expr) -> Type {
        if let Some(te) = type_annotation {
            return resolve_final_annotation_type(&self.ctx(), te);
        }
        if let Some(ty) = value.resolved_type.as_ref()
            && *ty != Type::Unknown
        {
            return ty.clone();
        }
        let var_type = |name: &str| self.ctx().locals.type_of(name);
        infer_type_from_expr(&self.ctx(), &var_type, value).unwrap_or(Type::Unknown)
    }

    /// Emit a [`IRInstruction::UnionWrap`] if typecheck recorded a
    /// [`Coercion::UnionWiden`] for the expression's span, returning
    /// the wrapped operand. Pass-through otherwise.
    fn maybe_emit_union_wrap(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        expr: &Expr,
        value: IROperand,
    ) -> IROperand {
        let Some(Coercion::UnionWiden { source, target }) =
            resolve_coercion(&self.ctx(), expr.span)
        else {
            return value;
        };
        let dest = self.next_value_id();
        instructions.push(IRInstruction::UnionWrap {
            dest,
            value,
            source_ty: source,
            target_union: target,
        });
        IROperand::Local(dest)
    }

    /// Build the storage sink instruction for an [`AssignTarget`]:
    /// `LValue` with one segment or pattern binding -> `StoreLocal`,
    /// multi-segment lvalue -> `StoreField`. Errors on destructuring
    /// patterns (typecheck rejects most cases; codegen never
    /// supported them).
    fn build_store(
        &self,
        target: &AssignTarget,
        value_expr: &Expr,
        assigned_type: &Type,
        value: IROperand,
    ) -> Result<IRInstruction, String> {
        match target {
            AssignTarget::LValue(lvalue) if lvalue.segments.len() == 1 => {
                let name = lvalue.segments[0].clone();
                Ok(self.store_local(name, value_expr, assigned_type, value))
            }
            AssignTarget::LValue(lvalue) => self.store_field(&lvalue.segments, value),
            AssignTarget::Pattern(Pattern::Binding { name, .. }) => {
                Ok(self.store_local(name.clone(), value_expr, assigned_type, value))
            }
            AssignTarget::Pattern(_) => {
                Err("destructuring patterns not yet supported in compilation".to_string())
            }
        }
    }

    /// Build a [`IRInstruction::StoreLocal`] for a single-segment
    /// lvalue or pattern binding. Looks up the binding to decide
    /// `is_decl`: existing -> reassignment; absent -> fresh let.
    fn store_local(
        &self,
        name: String,
        value_expr: &Expr,
        assigned_type: &Type,
        value: IROperand,
    ) -> IRInstruction {
        let existing = self.ctx().locals.type_of(&name);
        let (ty, is_decl, ownership) = if let Some(existing_ty) = existing {
            (existing_ty, false, None)
        } else {
            (
                assigned_type.clone(),
                true,
                Some(ownership_for_expr(value_expr, assigned_type)),
            )
        };
        IRInstruction::StoreLocal {
            name,
            value,
            ty,
            is_decl,
            ownership,
        }
    }

    /// Build a [`IRInstruction::StoreField`] for a multi-segment
    /// lvalue chain. Mirrors [`IRInstruction::FieldChain`]'s shape
    /// so the executor can share its GEP-walking helper.
    fn store_field(&self, segments: &[String], value: IROperand) -> Result<IRInstruction, String> {
        let var_type = |name: &str| self.ctx().locals.type_of(name);
        let (base_type, steps) = resolve_field_path(&self.ctx(), segments, var_type)?;
        let ty = steps
            .last()
            .map(|step| step.field_type.clone())
            .unwrap_or(Type::Unknown);
        Ok(IRInstruction::StoreField {
            base_name: segments[0].clone(),
            base_type,
            steps,
            value,
            ty,
        })
    }

    /// Materialize the load + sink-builder pair for a compound
    /// assignment target. Single-segment lvalues lower as
    /// [`IRInstruction::LoadLocal`] feeding an
    /// [`IRInstruction::StoreLocal`] sink; multi-segment chains lower
    /// as [`IRInstruction::FieldChain`] feeding an
    /// [`IRInstruction::StoreField`] sink.
    fn compound_assign_target(
        &mut self,
        target: &LValue,
        instructions: &mut Vec<IRInstruction>,
    ) -> Result<CompoundAssignTarget, String> {
        if target.segments.len() == 1 {
            let name = target.segments[0].clone();
            let target_ty = self
                .ctx()
                .locals
                .type_of(&name)
                .ok_or_else(|| format!("undefined variable: {name}"))?;
            let dest = self.next_value_id();
            instructions.push(IRInstruction::LoadLocal {
                dest,
                name: name.clone(),
                ty: target_ty.clone(),
            });
            let sink: Box<dyn FnOnce(IROperand, Type) -> IRInstruction> =
                Box::new(move |value, ty| IRInstruction::StoreLocal {
                    name,
                    value,
                    ty,
                    is_decl: false,
                    ownership: None,
                });
            return Ok((target_ty, IROperand::Local(dest), sink));
        }

        let var_type = |name: &str| self.ctx().locals.type_of(name);
        let (base_type, steps) = resolve_field_path(&self.ctx(), &target.segments, var_type)?;
        let target_ty = steps
            .last()
            .map(|step| step.field_type.clone())
            .unwrap_or(Type::Unknown);
        let load_dest = self.next_value_id();
        let base_name = target.segments[0].clone();
        instructions.push(IRInstruction::FieldChain {
            dest: load_dest,
            base_name: base_name.clone(),
            base_type: base_type.clone(),
            steps: steps.clone(),
        });
        let sink: Box<dyn FnOnce(IROperand, Type) -> IRInstruction> =
            Box::new(move |value, ty| IRInstruction::StoreField {
                base_name,
                base_type,
                steps,
                value,
                ty,
            });
        Ok((target_ty, IROperand::Local(load_dest), sink))
    }
}

/// Map a [`ResolvedCompoundOp`] to the matching [`ResolvedBinaryOp`]
/// so [`IRInstruction::BinaryOp`] can carry the addition / subtract
/// / multiply / divide variant the executor already knows how to
/// emit.
fn compound_to_binary(op: &ResolvedCompoundOp) -> ResolvedBinaryOp {
    match op {
        ResolvedCompoundOp::FloatAdd => ResolvedBinaryOp::FloatAdd,
        ResolvedCompoundOp::FloatDiv => ResolvedBinaryOp::FloatDiv,
        ResolvedCompoundOp::FloatMul => ResolvedBinaryOp::FloatMul,
        ResolvedCompoundOp::FloatSub => ResolvedBinaryOp::FloatSub,
        ResolvedCompoundOp::IntAdd => ResolvedBinaryOp::IntAdd,
        ResolvedCompoundOp::IntDiv => ResolvedBinaryOp::IntDiv,
        ResolvedCompoundOp::IntMul => ResolvedBinaryOp::IntMul,
        ResolvedCompoundOp::IntSub => ResolvedBinaryOp::IntSub,
    }
}

/// Derive the [`OperandShape`] for compound-op resolution from an
/// Expo target type. Numeric primitives carry directly; everything
/// else returns `None` so [`Lowerer::lower_compound_assign_stmt`]
/// can surface the legacy "matching numeric types" diagnostic.
fn operand_shape_for_type(ty: &Type) -> Option<OperandShape> {
    let Type::Primitive(primitive) = ty else {
        return None;
    };
    if primitive.is_float() {
        return Some(OperandShape::Float);
    }
    if primitive.is_integer() {
        return Some(OperandShape::Integer {
            bit_width: int_bit_width(primitive),
        });
    }
    None
}

/// LLVM bit-width for an integer primitive, matching codegen's
/// `int_bit_width` table. Non-integer primitives return `0` (the
/// caller has already guarded with `is_integer`).
fn int_bit_width(primitive: &Primitive) -> u32 {
    match primitive {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 => 64,
        _ => 0,
    }
}
