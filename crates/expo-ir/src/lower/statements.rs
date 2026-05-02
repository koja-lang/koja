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
use expo_typecheck::types::{Primitive, Type, mangle_name};

use crate::Lowerer;
use crate::blocks::{IRBlockId, IRTerminator};
use crate::cfg::CFGBuilder;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::inference::infer_type_from_expr;
use crate::lower::ownership::ownership_for_expr;
use crate::lower::stmt::{
    resolve_annotation_subst, resolve_field_path, resolve_final_annotation_type,
};
use crate::resolved::ops::{
    OperandShape, ResolvedBinaryOp, ResolvedCompoundOp, resolve_compound_op,
};
use crate::values::{IRInstruction, IROperand};

/// Output of [`Lowerer::lower_statements_for_value`]: the new open
/// block (or `None` if all paths terminated) plus the trailing
/// expression's operand and Expo type when the body ends in a
/// [`Statement::Expr`] without an early terminator. Used by value-
/// producing conditional constructs (`if`/`else`, `cond`, `match`)
/// to pre-stage the merge phi at lowering time.
pub type LoweredStatementsForValue = (Option<IRBlockId>, Option<(IROperand, Type)>);

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
    /// Lower one [`Statement`] into `builder` at `open`. Returns the
    /// new open block (or `None` if the statement terminated all paths
    /// via `Return` / `Break`).
    pub fn lower_statement(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        stmt: &Statement,
    ) -> Result<Option<IRBlockId>, String> {
        match stmt {
            Statement::Assignment {
                target,
                type_annotation,
                value,
                ..
            } => self.lower_assignment_stmt(builder, open, target, type_annotation.as_ref(), value),
            Statement::Break { .. } => self.lower_break_stmt(builder, open),
            Statement::CompoundAssign {
                target, op, value, ..
            } => self.lower_compound_assign_stmt(builder, open, target, op, value),
            Statement::Expr(expr) => self.lower_expr_stmt(builder, open, expr),
            Statement::Return { value, .. } => {
                self.lower_return_stmt(builder, open, value.as_ref())
            }
        }
    }

    /// Lower a sequence of statements. Threads `open` through each
    /// statement's lowering. Returns `None` as soon as any statement
    /// terminates (defensively, since `Return` / `Break` must
    /// syntactically be last in well-typed code).
    pub fn lower_statements(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        stmts: &[Statement],
    ) -> Result<Option<IRBlockId>, String> {
        let mut current = open;
        for stmt in stmts {
            let Some(next) = self.lower_statement(builder, current, stmt)? else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some(current))
    }

    /// Lower an [`Statement::Expr`]: lower the inner expression and
    /// discard its operand. Returns the new open block (control flow
    /// inside the expression may have advanced the cursor through a
    /// merge block).
    fn lower_expr_stmt(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
    ) -> Result<Option<IRBlockId>, String> {
        let (next, _op, _ty) = self.lower_expr_to_operand(builder, open, expr)?;
        Ok(next)
    }

    /// Lower `stmts` into a fresh block with id `id` and the given
    /// `label`. Adds the block to `builder` first so subsequent
    /// statement lowering targets it. If lowering does not terminate
    /// the body (no early `Return` / `Break`), closes the block with
    /// `Branch(default_target)`. Returns the id passed in (callers
    /// rarely need it; the block has been added to the builder either
    /// way).
    pub fn lower_body_block(
        &mut self,
        builder: &mut CFGBuilder,
        id: IRBlockId,
        label: impl Into<String>,
        stmts: &[Statement],
        default_target: IRBlockId,
    ) -> Result<(), String> {
        builder.add_block(id, label);
        let exit = self.lower_statements(builder, id, stmts)?;
        if let Some(exit) = exit {
            builder.set_terminator(exit, IRTerminator::Branch(default_target));
        }
        Ok(())
    }

    /// Lower a full function body into a flat
    /// [`Vec<crate::IRBasicBlock>`].
    ///
    /// Builds a fresh [`CFGBuilder`], opens an `entry` block, walks
    /// the body via [`Self::lower_statements`] (control-flow
    /// expressions inside statements mint their own blocks via the
    /// recursive lowering), and synthesizes the implicit-return
    /// terminator for the trailing slot:
    ///
    /// - Trailing [`Statement::Expr`] in a non-`Unit` return: lowers
    ///   the value through [`Self::lower_tail_expr_to_operand`] (so a
    ///   direct call in tail position picks up `tail = true`),
    ///   optionally union-wraps it, and closes with
    ///   `Return { value: Some(op), drop_skip }`.
    /// - Trailing [`Statement::Expr`] in a `Unit` return: discards the
    ///   value; closes with `Return { value: None, .. }`.
    /// - No trailing expression: closes with `Return(None)` for
    ///   `Unit` returns or [`IRTerminator::Unreachable`] otherwise
    ///   (matches the legacy `compile_function_body` fallthrough).
    ///
    /// Returns the [`Vec<crate::IRBasicBlock>`] for the planner to
    /// store on [`crate::IRFunctionKind::Free`] /
    /// [`crate::IRFunctionKind::Method`]'s `blocks` field.
    pub fn lower_function_body(
        &mut self,
        body: &[Statement],
        return_type: &Type,
    ) -> Result<Vec<crate::blocks::IRBasicBlock>, String> {
        let saved_hint = std::mem::replace(
            &mut self.fn_state.return_type_hint,
            (*return_type != Type::Unit).then(|| return_type.clone()),
        );

        let mut builder = CFGBuilder::new();
        let entry_id = self.next_block_id();
        builder.add_block(entry_id, "entry");

        let mut current = Some(entry_id);
        let body_len = body.len();
        for (i, stmt) in body.iter().enumerate() {
            let Some(open) = current else {
                break;
            };
            let is_last = i == body_len - 1;
            if is_last && let Statement::Expr(expr) = stmt {
                self.close_with_implicit_return(&mut builder, open, expr, return_type)?;
                current = None;
                continue;
            }
            current = self.lower_statement(&mut builder, open, stmt)?;
        }

        if let Some(open) = current {
            let term = if *return_type == Type::Unit {
                IRTerminator::Return {
                    value: None,
                    drop_skip: None,
                }
            } else {
                IRTerminator::Unreachable
            };
            builder.set_terminator(open, term);
        }

        self.fn_state.return_type_hint = saved_hint;
        Ok(builder.into_blocks())
    }

    /// Lower a function-body trailing [`Statement::Expr`] into the
    /// implicit-return terminator. Splits out for the ≤40-LOC budget
    /// in [`Self::lower_function_body`].
    fn close_with_implicit_return(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
        return_type: &Type,
    ) -> Result<(), String> {
        let (next, mut operand, _ty) = self.lower_tail_expr_to_operand(builder, open, expr)?;
        let Some(open) = next else {
            return Ok(());
        };
        operand = self.maybe_emit_union_wrap(builder, open, expr, operand);
        let term = if *return_type == Type::Unit {
            IRTerminator::Return {
                value: None,
                drop_skip: None,
            }
        } else {
            let drop_skip = match &expr.kind {
                ExprKind::Ident { name, .. } => Some(name.clone()),
                _ => None,
            };
            IRTerminator::Return {
                value: Some(operand),
                drop_skip,
            }
        };
        builder.set_terminator(open, term);
        Ok(())
    }

    /// Lower a statement sequence into `open`, capturing the trailing
    /// expression's operand + Expo type when the body ends in a
    /// [`Statement::Expr`] without an early terminator. Used by
    /// value-producing conditional constructs to feed the merge phi.
    ///
    /// Returns `(new_open, trailing_value)`:
    ///
    /// - `new_open` -- `Some(...)` if execution continues after the
    ///   body (cursor advanced through any internal control flow);
    ///   `None` if all paths terminated.
    /// - `trailing_value` -- `Some((operand, type))` iff the body ends
    ///   in a `Statement::Expr` and lowering didn't terminate; `None`
    ///   otherwise (body is statement-shaped or short-circuited).
    pub fn lower_statements_for_value(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        stmts: &[Statement],
    ) -> Result<LoweredStatementsForValue, String> {
        if stmts.is_empty() {
            return Ok((Some(open), None));
        }
        let mut current = open;
        for stmt in &stmts[..stmts.len() - 1] {
            let Some(next) = self.lower_statement(builder, current, stmt)? else {
                return Ok((None, None));
            };
            current = next;
        }
        let last = stmts.last().expect("non-empty body");
        if let Statement::Expr(expr) = last {
            let (next, trailing, lowered_ty) =
                self.lower_expr_to_operand(builder, current, expr)?;
            let Some(next) = next else {
                return Ok((None, None));
            };
            // Prefer the lowerer's published type (the new
            // [`OperandResult`] `Type` slot) over typecheck's
            // `expr.resolved_type` -- the lowerer's type is the
            // source of truth for value-typed downstream consumers
            // (Slice 3a-bis, Wave 31). Falls back to typecheck's
            // record only when the lowerer published `Unknown`
            // (e.g. an unhandled Stub shape).
            let trailing_ty = if lowered_ty != Type::Unknown {
                Some(lowered_ty)
            } else {
                expr.resolved_type.clone()
            };
            // Unit-typed trailing expressions don't carry a value through
            // the merge phi -- void calls and the unit literal both
            // produce `None` at codegen, leaving their nominal `dest`
            // unregistered in the executor's `value_map`. Treat the body
            // as no-value in that case so callers (e.g. match arms) skip
            // the materialize step entirely.
            let is_unit = matches!(trailing_ty.as_ref(), Some(Type::Unit));
            if is_unit {
                Ok((Some(next), None))
            } else {
                let ty = trailing_ty.unwrap_or(Type::Unknown);
                Ok((Some(next), Some((trailing, ty))))
            }
        } else {
            let next = self.lower_statement(builder, current, last)?;
            Ok((next, None))
        }
    }

    /// Lower an [`Statement::Assignment`]: push the annotation's
    /// type-subst entries into `fn_state.type_subst` for the duration
    /// of RHS lowering, lower the value into the cursor, optionally
    /// emit an [`IRInstruction::UnionWrap`] for a recorded
    /// `UnionWiden` coercion, then emit a [`IRInstruction::StoreLocal`]
    /// (single-segment) or [`IRInstruction::StoreField`]
    /// (multi-segment) sink.
    ///
    /// `Push` / `PopTypeSubst` brackets the RHS lowering so any
    /// [`IRInstruction::Stub`]'s deferred `compile_expr` (e.g.
    /// `List<Int>::new()`'s type-arg inference) sees the entries at
    /// execute time too.
    fn lower_assignment_stmt(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        target: &AssignTarget,
        type_annotation: Option<&TypeExpr>,
        value: &Expr,
    ) -> Result<Option<IRBlockId>, String> {
        let subst_entries: Vec<(String, Type)> = type_annotation
            .map(|te| resolve_annotation_subst(&self.ctx(), te))
            .unwrap_or_default();
        let saved_subst = if subst_entries.is_empty() {
            None
        } else {
            let saved = self.fn_state.type_subst.clone();
            for (name, ty) in &subst_entries {
                self.fn_state.type_subst.insert(name.clone(), ty.clone());
            }
            Some(saved)
        };

        if !subst_entries.is_empty() {
            builder.append(
                open,
                IRInstruction::PushTypeSubst {
                    entries: subst_entries.clone(),
                },
            );
        }

        let (next, mut value_operand, value_type) =
            self.lower_expr_to_operand(builder, open, value)?;
        let assigned_type = self.resolve_assigned_type(type_annotation, value, &value_type);
        let Some(open) = next else {
            if let Some(saved) = saved_subst {
                self.fn_state.type_subst = saved;
            }
            return Ok(None);
        };
        value_operand = self.maybe_emit_union_wrap(builder, open, value, value_operand);
        value_operand =
            self.maybe_emit_from_list_literal(builder, open, value, &assigned_type, value_operand);

        let store = self.build_store(target, value, &assigned_type, value_operand)?;
        builder.append(open, store);

        if !subst_entries.is_empty() {
            builder.append(
                open,
                IRInstruction::PopTypeSubst {
                    names: subst_entries.iter().map(|(n, _)| n.clone()).collect(),
                },
            );
        }
        if let Some(saved) = saved_subst {
            self.fn_state.type_subst = saved;
        }
        Ok(Some(open))
    }

    /// Lower an [`Statement::CompoundAssign`] (`target op= value`):
    /// load the current value, lower the RHS, derive the operand
    /// shape from the target's Expo type, look up the resolved
    /// compound op, emit a [`IRInstruction::BinaryOp`], and store
    /// the result back into the target.
    fn lower_compound_assign_stmt(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        target: &LValue,
        op: &CompoundOp,
        value: &Expr,
    ) -> Result<Option<IRBlockId>, String> {
        let (target_type, load_op, sink) = self.compound_assign_target(builder, open, target)?;
        let (next, rhs_op, _rhs_ty) = self.lower_expr_to_operand(builder, open, value)?;
        let Some(open) = next else {
            return Ok(None);
        };

        let shape = operand_shape_for_type(&target_type)
            .ok_or("compound assignment requires matching numeric types")?;
        let resolved = resolve_compound_op(op, &shape)?;
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::BinaryOp {
                dest,
                op: compound_to_binary(&resolved),
                lhs: load_op,
                rhs: rhs_op,
            },
        );
        builder.append(open, sink(IROperand::Local(dest), target_type));
        Ok(Some(open))
    }

    /// Lower a [`Statement::Return`]: in the `Some(expr)` case lift
    /// the value through the tail-context lifter (so direct calls
    /// inherit `tail = true`), optionally wrap into a widening
    /// union, and capture the binding name to skip in the pre-
    /// return drop pass when the expression is a bare ident.
    /// Closes `open` with [`IRTerminator::Return`] and returns
    /// `Ok(None)`.
    fn lower_return_stmt(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        value: Option<&Expr>,
    ) -> Result<Option<IRBlockId>, String> {
        let Some(expr) = value else {
            builder.set_terminator(
                open,
                IRTerminator::Return {
                    value: None,
                    drop_skip: None,
                },
            );
            return Ok(None);
        };
        let (next, mut operand, _ty) = self.lower_tail_expr_to_operand(builder, open, expr)?;
        let Some(open) = next else {
            return Ok(None);
        };
        operand = self.maybe_emit_union_wrap(builder, open, expr, operand);
        let drop_skip = match &expr.kind {
            ExprKind::Ident { name, .. } => Some(name.clone()),
            _ => None,
        };
        builder.set_terminator(
            open,
            IRTerminator::Return {
                value: Some(operand),
                drop_skip,
            },
        );
        Ok(None)
    }

    /// Lower a [`Statement::Break`]: close `open` with an
    /// unconditional branch to the innermost enclosing loop's exit
    /// id, read from [`crate::FnLowerState::current_loop_exit`].
    fn lower_break_stmt(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
    ) -> Result<Option<IRBlockId>, String> {
        let exit = self
            .fn_state
            .current_loop_exit()
            .ok_or("break outside of loop")?;
        builder.set_terminator(open, IRTerminator::Branch(exit));
        Ok(None)
    }

    /// Resolve the post-RHS assigned type. Source precedence:
    ///
    /// 1. Annotation (when present) -- pinned by the source.
    /// 2. Lowerer's published `value_type` (from
    ///    [`crate::Lowerer::lower_expr_to_operand`]'s [`OperandResult`]
    ///    `Type` slot) -- the source of truth for the operand's
    ///    runtime type, computed by the same lowering that emitted
    ///    the value's instructions. Skips when the lowerer published
    ///    `Unknown` (e.g. an unhandled Stub shape with no typecheck
    ///    record).
    /// 3. Typecheck's `value.resolved_type` -- fallback for the
    ///    Stub-fallthrough case.
    /// 4. Static expression-kind inference
    ///    ([`infer_type_from_expr`]) -- last-resort for AST shapes
    ///    typecheck didn't record a type for.
    /// 5. [`Type::Unknown`] -- defensive.
    ///
    /// Slice 3a-bis (Wave 31) added precedence step 2, eliminating
    /// the fragile chain through `infer_type_from_expr`'s ad-hoc
    /// per-shape estimators (chained method calls, field-typed
    /// calls, match arm join etc.). The lowerer's published type is
    /// the authoritative answer.
    fn resolve_assigned_type(
        &self,
        type_annotation: Option<&TypeExpr>,
        value: &Expr,
        value_type: &Type,
    ) -> Type {
        if let Some(te) = type_annotation {
            return resolve_final_annotation_type(&self.ctx(), te);
        }
        if *value_type != Type::Unknown {
            return value_type.clone();
        }
        if let Some(ty) = value.resolved_type.as_ref()
            && *ty != Type::Unknown
        {
            return ty.clone();
        }
        let var_type = |name: &str| self.ctx().locals.type_of(name);
        infer_type_from_expr(&self.ctx(), &var_type, value).unwrap_or(Type::Unknown)
    }

    /// Emit a [`IRInstruction::FromListLiteral`] when the RHS of an
    /// assignment is an [`ExprKind::List`] literal and the target
    /// type is a non-`List` named type with type-args (e.g.
    /// `Set<Int> = [1, 2, 3]`). The pre-codegen elaboration pass
    /// rewrites the instruction into a typed
    /// [`IRInstruction::MethodCall`] on `target.from_list` after
    /// monomorphizing the impl method into [`crate::IRProgram`].
    /// Pass-through for any other shape (target is `List`, RHS is
    /// not a list literal, target lacks type-args, etc.).
    fn maybe_emit_from_list_literal(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        value: &Expr,
        assigned_type: &Type,
        operand: IROperand,
    ) -> IROperand {
        if !matches!(value.kind, ExprKind::List { .. }) {
            return operand;
        }
        let Type::Named {
            identifier,
            type_args,
        } = assigned_type
        else {
            return operand;
        };
        if identifier.name == "List" || type_args.is_empty() {
            return operand;
        }
        let target_mangled = MonomorphizedTypeIdentifier::new(mangle_name(identifier, type_args));
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::FromListLiteral {
                dest,
                value: operand,
                target_ty: assigned_type.clone(),
                target_mangled,
            },
        );
        IROperand::Local(dest)
    }

    /// Emit a [`IRInstruction::UnionWrap`] if typecheck recorded a
    /// [`Coercion::UnionWiden`] for the expression's span, returning
    /// the wrapped operand. Pass-through otherwise.
    ///
    /// Thin convenience wrapper around
    /// [`Lowerer::stage_union_widen`]: keeps the existing
    /// assignment-RHS / return-value call-sites readable while the
    /// new method-call coercion seam (Slice 1) calls the span-keyed
    /// helper directly.
    pub(crate) fn maybe_emit_union_wrap(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
        value: IROperand,
    ) -> IROperand {
        self.stage_union_widen(builder, open, expr.span, value)
    }

    /// Build the storage sink instruction for an [`AssignTarget`]:
    /// `LValue` with one segment or pattern binding -> `StoreLocal`,
    /// multi-segment lvalue -> `StoreField`. Errors on destructuring
    /// patterns (typecheck rejects most cases; codegen never
    /// supported them).
    fn build_store(
        &mut self,
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
    /// Fresh-decl bindings register their (name, type) pair in
    /// [`crate::FnLowerState::local_types`] so subsequent statements
    /// can resolve `Ident` references via
    /// [`crate::Lowerer::lower_ident_or_stub`]'s typed-local lookup.
    fn store_local(
        &mut self,
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
        if is_decl {
            self.fn_state.local_types.insert(name.clone(), ty.clone());
        }
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
    fn store_field(
        &mut self,
        segments: &[String],
        value: IROperand,
    ) -> Result<IRInstruction, String> {
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
        builder: &mut CFGBuilder,
        open: IRBlockId,
        target: &LValue,
    ) -> Result<CompoundAssignTarget, String> {
        if target.segments.len() == 1 {
            let name = target.segments[0].clone();
            let target_ty = self
                .ctx()
                .locals
                .type_of(&name)
                .ok_or_else(|| format!("undefined variable: {name}"))?;
            let dest = self.next_value_id();
            builder.append(
                open,
                IRInstruction::LoadLocal {
                    dest,
                    name: name.clone(),
                    ty: target_ty.clone(),
                },
            );
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
        builder.append(
            open,
            IRInstruction::FieldChain {
                dest: load_dest,
                base_name: base_name.clone(),
                base_type: base_type.clone(),
                steps: steps.clone(),
            },
        );
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
