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
//!   then package constants, then function-as-value).
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

use expo_ast::ast::{
    BinarySegment, EnumConstructionData, Expr, ExprKind, FieldInit, Literal, StringPart,
};
use expo_typecheck::context::{FnParam, VariantData};
use expo_typecheck::types::{Primitive, Type, TypeIdentifier, mangle_name};

use crate::Lowerer;
use crate::blocks::IRBlockId;
use crate::cfg::CFGBuilder;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::binary::resolve_binary_segments;
use crate::lower::constants::{resolve_const_inline, resolved_to_operand};
use crate::lower::diag::{
    HELPER_CALL, HELPER_IDENT, REASON_CALL_NON_IDENT_CALLEE, REASON_IDENT_NO_BINDING,
    log_helper_bail, log_stub_fallthrough,
};
use crate::lower::enums::lower_concrete_enum;
use crate::lower::patterns::arms_are_minimal;
use crate::lower::structs::lower_concrete_struct;
use crate::resolved::constants::ResolvedConst;
use crate::resolved::construction::{ResolvedEnumConstruction, ResolvedStructConstruction};
use crate::resolved::enums::ResolvedVariantFields;
use crate::resolved::fields::ResolvedStructField;
use crate::values::{
    EnumPayload, EnumTupleFieldInit, IRInstruction, IROperand, LoweredBinarySegment,
    StringFormatPart, StructFieldInit,
};

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
        if let Some(constant) = resolve_const_inline(&expr.kind) {
            let ty = const_operand_type(&constant, expr);
            return Ok((Some(open), resolved_to_operand(&constant), ty));
        }

        match &expr.kind {
            ExprKind::Binary { op, left, right } => {
                if let Some((open, operand, ty)) =
                    self.lower_binary_op_or_stub(builder, open, op, left, right)?
                {
                    return Ok((open, operand, ty));
                }
            }
            ExprKind::BinaryLiteral { segments } => {
                let (out, operand) = self.lower_binary_literal(builder, open, segments)?;
                return Ok((out, operand, Type::Primitive(Primitive::Binary)));
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident { name, .. } = &callee.kind {
                    if let Some((open, operand, ty)) =
                        self.lower_call_or_stub(builder, open, name, args, tail, expr.span)?
                    {
                        return Ok((open, operand, ty));
                    }
                } else {
                    log_helper_bail(
                        HELPER_CALL,
                        REASON_CALL_NON_IDENT_CALLEE,
                        self.fn_state.current_fn(),
                        &expr.span,
                    );
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
            ExprKind::Ident { name, .. } => {
                if let Some((operand, ty)) = self.lower_ident_or_stub(builder, open, name) {
                    return Ok((Some(open), operand, ty));
                }
                log_helper_bail(
                    HELPER_IDENT,
                    REASON_IDENT_NO_BINDING,
                    self.fn_state.current_fn(),
                    &expr.span,
                );
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
            ExprKind::String { parts, .. } => {
                let (out, operand) = self.lower_string_format(builder, open, parts)?;
                return Ok((out, operand, Type::Primitive(Primitive::String)));
            }
            ExprKind::EnumConstruction {
                type_path,
                variant,
                data,
            } => {
                if let Some(out) = self.lower_enum_construction_or_stub(
                    builder, open, expr, type_path, variant, data,
                )? {
                    return Ok(out);
                }
            }
            ExprKind::StructConstruction { type_path, fields } => {
                if let Some(out) =
                    self.lower_struct_construction_or_stub(builder, open, expr, type_path, fields)?
                {
                    return Ok(out);
                }
            }
            ExprKind::Unary { op, operand } => {
                if let Some((open, o, ty)) =
                    self.lower_unary_op_or_stub(builder, open, op, operand)?
                {
                    return Ok((open, o, ty));
                }
            }
            ExprKind::If {
                condition,
                then_body,
                else_body: Some(else_stmts),
            } => {
                let result_ty = expr.resolved_type.clone().unwrap_or(Type::Unknown);
                let (out, op) = self.lower_if_else(
                    builder,
                    open,
                    condition,
                    then_body,
                    else_stmts,
                    result_ty.clone(),
                )?;
                return Ok((out, op, result_ty));
            }
            ExprKind::If {
                condition,
                then_body,
                else_body: None,
            } => {
                let (out, op) = self.lower_if_no_else(builder, open, condition, then_body)?;
                return Ok((out, op, Type::Unit));
            }
            ExprKind::Unless { condition, body } => {
                let (out, op) = self.lower_unless(builder, open, condition, body)?;
                return Ok((out, op, Type::Unit));
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                let result_ty = expr.resolved_type.clone().unwrap_or(Type::Unknown);
                let (out, op) = self.lower_ternary(
                    builder,
                    open,
                    condition,
                    then_expr,
                    else_expr,
                    result_ty.clone(),
                )?;
                return Ok((out, op, result_ty));
            }
            ExprKind::Cond {
                arms,
                else_body: Some(else_stmts),
            } => {
                let result_ty = expr.resolved_type.clone().unwrap_or(Type::Unknown);
                let (out, op) = self.lower_cond(
                    builder,
                    open,
                    arms,
                    Some(else_stmts.as_slice()),
                    result_ty.clone(),
                )?;
                return Ok((out, op, result_ty));
            }
            ExprKind::Cond {
                arms,
                else_body: None,
            } if !arms.is_empty() => {
                // No-else `cond` is statement-shaped: when no arm matches,
                // control falls through to merge with no value, so
                // `try_stage_cond_phi` returns Unit.
                let (out, op) = self.lower_cond(builder, open, arms, None, Type::Unit)?;
                return Ok((out, op, Type::Unit));
            }
            ExprKind::Loop { body } => {
                let (out, op) = self.lower_loop(builder, open, body)?;
                return Ok((out, op, Type::Unit));
            }
            ExprKind::While { condition, body } => {
                let (out, op) = self.lower_while(builder, open, condition, body)?;
                return Ok((out, op, Type::Unit));
            }
            ExprKind::Match { subject, arms } if arms_are_minimal(arms) => {
                return self.lower_match_arm(builder, open, subject, arms);
            }
            _ => {}
        }

        log_stub_fallthrough(
            crate::program::expr_kind_name(&expr.kind),
            self.fn_state.current_fn(),
            &expr.span,
        );
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

    /// Lower an [`expo_ast::ast::ExprKind::BinaryLiteral`]
    /// (`<<seg1, seg2, ...>>`) into an [`IRInstruction::BinaryConstruct`].
    /// Reuses [`resolve_binary_segments`] for the layout (per-segment
    /// width / kind, total bit count, byte-alignment validation),
    /// then lowers each segment's value expression to an
    /// [`IROperand`] in-place. Returns `None` for the open block if
    /// any inner expression's CFG terminates.
    fn lower_binary_literal(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        segments: &[BinarySegment],
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let layout = resolve_binary_segments(segments)?;
        debug_assert_eq!(layout.segments.len(), segments.len());
        let mut current = open;
        let mut lowered_segments = Vec::with_capacity(segments.len());
        for (seg, resolved) in segments.iter().zip(&layout.segments) {
            let (next, value, _ty) = self.lower_expr_to_operand(builder, current, &seg.value)?;
            let Some(next) = next else {
                return Ok((None, IROperand::Unit));
            };
            current = next;
            lowered_segments.push(LoweredBinarySegment {
                bit_width: resolved.bit_width,
                kind: resolved.kind,
                value,
            });
        }
        let dest = self.next_value_id();
        builder.append(
            current,
            IRInstruction::BinaryConstruct {
                dest,
                layout,
                segments: lowered_segments,
            },
        );
        Ok((Some(current), IROperand::Local(dest)))
    }

    /// Lower an [`expo_ast::ast::ExprKind::String`] (interpolated form)
    /// into an [`IRInstruction::StringFormat`]. Each
    /// [`StringPart::Literal`] becomes a [`StringFormatPart::Literal`];
    /// each [`StringPart::Interpolation`] lowers its inner expression
    /// via [`Self::lower_expr_to_operand`] and packages the resulting
    /// operand with its resolved type (the codegen executor reads
    /// `ty` to choose the printf format specifier).
    ///
    /// Pure-literal strings short-circuit through `resolve_const` ->
    /// [`IROperand::ConstStr`] before this helper runs, so we can
    /// assume at least one `Interpolation` is present here. Returning
    /// the new open block (or `None` if any inner expression's CFG
    /// terminated) keeps this helper composable with the universal
    /// `lower_expr_to_operand` recursion.
    fn lower_string_format(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        parts: &[StringPart],
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let mut current = open;
        let mut lowered_parts = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                StringPart::Literal { value, .. } => {
                    lowered_parts.push(StringFormatPart::Literal(value.clone()));
                }
                StringPart::Interpolation { expr, format, .. } => {
                    let (next, operand, ty) = self.lower_expr_to_operand(builder, current, expr)?;
                    let Some(next) = next else {
                        return Ok((None, IROperand::Unit));
                    };
                    current = next;
                    lowered_parts.push(StringFormatPart::Interpolated {
                        value: operand,
                        ty,
                        format: format.clone(),
                    });
                }
            }
        }
        let dest = self.next_value_id();
        builder.append(
            current,
            IRInstruction::StringFormat {
                dest,
                parts: lowered_parts,
            },
        );
        Ok((Some(current), IROperand::Local(dest)))
    }

    /// Lower an [`expo_ast::ast::ExprKind::Ident`] to a typed
    /// instruction matching `compile_expr`'s precedence: in-scope
    /// local binding -> package constant -> function-as-value
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
        let const_id = self.package.map(|pkg| TypeIdentifier {
            package: pkg.clone(),
            name: name.to_string(),
        });
        let (local_ty, const_ty, fn_type) = {
            let ctx = self.ctx();
            let local_ty = ctx.locals.type_of(name);
            let const_ty = const_id
                .as_ref()
                .and_then(|id| ctx.type_ctx.constants.get(id).cloned());
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

        if let (Some(ty), Some(const_id)) = (const_ty, const_id.as_ref()) {
            if let Some(operand) = self.const_tables.primitives.get(const_id) {
                return Some((operand.clone(), ty));
            }
            if let Some(id) = self.const_tables.compounds.get(const_id).copied() {
                let dest = self.next_value_id();
                builder.append(
                    open,
                    IRInstruction::LoadConst {
                        dest,
                        id,
                        ty: ty.clone(),
                    },
                );
                return Some((IROperand::Local(dest), ty));
            }
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

    /// Lower a [`expo_ast::ast::ExprKind::StructConstruction`] to an
    /// [`IRInstruction::StructConstruct`]. Handles both concrete
    /// structs (via [`lower_concrete_struct`]) and generic
    /// instantiations (via the [`crate::IRProgram`] lookup populated
    /// by [`crate::closure_program`]). Returns `Ok(None)` only when
    /// the layout truly cannot be resolved (unknown name, missing
    /// resolved id), leaving the caller to fall through to
    /// [`IRInstruction::Stub`].
    fn lower_struct_construction_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
        type_path: &[String],
        field_inits: &[FieldInit],
    ) -> Result<Option<(Option<IRBlockId>, IROperand, Type)>, String> {
        let Some(raw_name) = type_path.first() else {
            return Ok(None);
        };
        let resolved_type = expr.resolved_type.as_ref();
        let Some(resolved) =
            self.resolve_struct_construction(raw_name, field_inits, resolved_type)?
        else {
            return Ok(None);
        };
        let (last_open, fields) =
            match self.lower_struct_field_operands(builder, open, field_inits, &resolved.fields)? {
                Some(parts) => parts,
                None => return Ok(Some((None, IROperand::Unit, Type::Unit))),
            };
        let dest = self.next_value_id();
        let result_type = resolved.result_type.clone();
        builder.append(
            last_open,
            IRInstruction::StructConstruct {
                dest,
                mangled: resolved.mangled_name,
                result_type: result_type.clone(),
                fields,
            },
        );
        Ok(Some((Some(last_open), IROperand::Local(dest), result_type)))
    }

    /// Build a [`crate::resolved::construction::ResolvedStructConstruction`]
    /// for `raw_name` + `field_inits`. Dispatches on
    /// `resolved_type.type_args`: empty type args use the concrete
    /// helper [`lower_concrete_struct`]; non-empty args look the
    /// monomorphized [`crate::program::IRStruct`] up in `program`
    /// (registered by the closure pass) for the post-substitution
    /// field layout. Returns `Ok(None)` when the lookup fails.
    fn resolve_struct_construction(
        &self,
        raw_name: &str,
        field_inits: &[FieldInit],
        resolved_type: Option<&Type>,
    ) -> Result<Option<ResolvedStructConstruction>, String> {
        let resolved_id = resolved_type.and_then(named_identifier);
        let type_args = resolved_type.and_then(named_type_args).unwrap_or(&[]);
        if type_args.is_empty() {
            let resolved = lower_concrete_struct(&self.ctx(), raw_name, field_inits, resolved_id);
            return Ok(resolved.ok());
        }
        let Some(identifier) = resolved_id else {
            return Ok(None);
        };
        Ok(self.resolve_generic_struct_layout(identifier, type_args, field_inits))
    }

    /// Look up the closure-pass-registered [`crate::program::IRStruct`]
    /// for a generic instantiation and assemble a
    /// [`ResolvedStructConstruction`] from its post-substitution field
    /// layout. Returns `None` when the lookup misses (e.g. the closure
    /// pass hasn't run, or the type isn't a struct).
    fn resolve_generic_struct_layout(
        &self,
        identifier: &TypeIdentifier,
        type_args: &[Type],
        field_inits: &[FieldInit],
    ) -> Option<ResolvedStructConstruction> {
        let mangled = MonomorphizedTypeIdentifier::new(mangle_name(identifier, type_args));
        let decl = self.program.structs.get(&mangled)?;
        let mut fields = Vec::with_capacity(field_inits.len());
        for field_init in field_inits {
            let (index, field_type) = decl
                .fields
                .iter()
                .enumerate()
                .find(|(_, (name, _))| name == &field_init.name)
                .map(|(i, (_, ty))| (i as u32, ty.clone()))?;
            fields.push(ResolvedStructField {
                field_type,
                index,
                name: field_init.name.clone(),
            });
        }
        Some(ResolvedStructConstruction {
            fields,
            is_generic: true,
            mangled_name: mangled,
            result_type: Type::Named {
                identifier: identifier.clone(),
                type_args: type_args.to_vec(),
            },
        })
    }

    /// Lower each struct field's value expression in source order,
    /// threading the open block through each call. Returns the final
    /// open block + parallel [`StructFieldInit`] vector. Returns
    /// `Ok(None)` when any field's expression terminates control flow,
    /// signalling the caller to short-circuit the construction.
    fn lower_struct_field_operands(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        field_inits: &[FieldInit],
        resolved_fields: &[ResolvedStructField],
    ) -> Result<Option<(IRBlockId, Vec<StructFieldInit>)>, String> {
        let mut current = open;
        let mut fields = Vec::with_capacity(field_inits.len());
        for (i, field_init) in field_inits.iter().enumerate() {
            let (next, value, _ty) =
                self.lower_expr_to_operand(builder, current, &field_init.value)?;
            let resolved_field = &resolved_fields[i];
            fields.push(StructFieldInit {
                name: resolved_field.name.clone(),
                index: resolved_field.index,
                field_type: resolved_field.field_type.clone(),
                value,
            });
            let Some(next) = next else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some((current, fields)))
    }

    /// Lower a [`expo_ast::ast::ExprKind::EnumConstruction`] to an
    /// [`IRInstruction::EnumConstruct`]. Handles both concrete enums
    /// (via [`lower_concrete_enum`]) and generic instantiations (via
    /// the [`crate::IRProgram`] lookup populated by
    /// [`crate::closure_program`]). Returns `Ok(None)` only when the
    /// layout truly cannot be resolved, leaving the caller to fall
    /// through to [`IRInstruction::Stub`].
    fn lower_enum_construction_or_stub(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        expr: &Expr,
        type_path: &[String],
        variant: &str,
        data: &EnumConstructionData,
    ) -> Result<Option<(Option<IRBlockId>, IROperand, Type)>, String> {
        let Some(raw_name) = type_path.first() else {
            return Ok(None);
        };
        let resolved_type = expr.resolved_type.as_ref();
        let Some(resolved) =
            self.resolve_enum_construction(raw_name, variant, data, resolved_type)?
        else {
            return Ok(None);
        };
        let (last_open, payload) =
            match self.lower_enum_payload(builder, open, data, &resolved.variant_fields)? {
                Some(parts) => parts,
                None => return Ok(Some((None, IROperand::Unit, Type::Unit))),
            };
        let dest = self.next_value_id();
        let result_type = resolved.result_type.clone();
        builder.append(
            last_open,
            IRInstruction::EnumConstruct {
                dest,
                mangled: resolved.mangled_name,
                result_type: result_type.clone(),
                tag: resolved.tag as u8,
                variant: resolved.variant_name,
                payload,
            },
        );
        Ok(Some((Some(last_open), IROperand::Local(dest), result_type)))
    }

    /// Build a [`crate::resolved::construction::ResolvedEnumConstruction`]
    /// for `raw_name`.`variant` (`+ data`). Concrete enums use
    /// [`lower_concrete_enum`]; generic instantiations look the
    /// monomorphized [`crate::program::IREnum`] up in `program`
    /// (registered by the closure pass) for the post-substitution
    /// variant layout. Returns `Ok(None)` when the lookup fails or
    /// when typecheck couldn't fully infer the type args (e.g.
    /// `Ok(value)` in a `Result<T, E>` returner publishes
    /// `Result<T, Unknown>` because the `E` slot can't be inferred
    /// from the call site alone -- codegen's legacy path resolves
    /// those via return-type hints / `fn_lower.type_subst`, so we
    /// fall through to Stub here and let it handle the case).
    fn resolve_enum_construction(
        &self,
        raw_name: &str,
        variant: &str,
        data: &EnumConstructionData,
        resolved_type: Option<&Type>,
    ) -> Result<Option<ResolvedEnumConstruction>, String> {
        let resolved_id = resolved_type.and_then(named_identifier);
        let type_args = resolved_type.and_then(named_type_args).unwrap_or(&[]);
        if type_args.is_empty() {
            let resolved =
                lower_concrete_enum(&self.ctx(), raw_name, variant, data, resolved_id.cloned());
            return Ok(resolved.ok());
        }
        if type_args.iter().any(|t| matches!(t, Type::Unknown)) {
            return Ok(None);
        }
        let Some(identifier) = resolved_id else {
            return Ok(None);
        };
        Ok(self.resolve_generic_enum_layout(identifier, type_args, variant, data))
    }

    /// Look up the closure-pass-registered [`crate::program::IREnum`]
    /// for a generic instantiation and assemble a
    /// [`ResolvedEnumConstruction`] from its post-substitution variant
    /// list. Returns `None` when the lookup misses or the variant is
    /// shape-mismatched against `data`.
    fn resolve_generic_enum_layout(
        &self,
        identifier: &TypeIdentifier,
        type_args: &[Type],
        variant: &str,
        data: &EnumConstructionData,
    ) -> Option<ResolvedEnumConstruction> {
        let mangled = MonomorphizedTypeIdentifier::new(mangle_name(identifier, type_args));
        let decl = self.program.enums.get(&mangled)?;
        let (tag_index, (_, variant_data)) = decl
            .variants
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == variant)?;
        let variant_fields = generic_variant_fields(variant_data, data)?;
        Some(ResolvedEnumConstruction {
            is_generic: true,
            mangled_name: mangled,
            result_type: Type::Named {
                identifier: identifier.clone(),
                type_args: type_args.to_vec(),
            },
            tag: tag_index as u64,
            variant_fields,
            variant_name: variant.to_string(),
        })
    }

    /// Lower the payload sub-expressions of an
    /// [`ExprKind::EnumConstruction`] to an [`EnumPayload`], threading
    /// the open block through each value expression. Returns
    /// `Ok(None)` when any sub-expression terminates control flow.
    fn lower_enum_payload(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        data: &EnumConstructionData,
        variant_fields: &ResolvedVariantFields,
    ) -> Result<Option<(IRBlockId, EnumPayload)>, String> {
        match (data, variant_fields) {
            (EnumConstructionData::Unit, _) => Ok(Some((open, EnumPayload::Unit))),
            (
                EnumConstructionData::Tuple(exprs),
                ResolvedVariantFields::Tuple { element_types },
            ) => match self.lower_enum_tuple_fields(builder, open, exprs, element_types)? {
                Some((next, lowered)) => Ok(Some((next, EnumPayload::Tuple(lowered)))),
                None => Ok(None),
            },
            (
                EnumConstructionData::Struct(field_inits),
                ResolvedVariantFields::Struct { fields },
            ) => match self.lower_enum_struct_fields(builder, open, field_inits, fields)? {
                Some((next, lowered)) => Ok(Some((next, EnumPayload::Struct(lowered)))),
                None => Ok(None),
            },
            _ => Err("enum construction payload shape does not match variant".to_string()),
        }
    }

    /// Lower each positional-element expression in an
    /// [`EnumConstructionData::Tuple`] payload to an
    /// [`EnumTupleFieldInit`], pairing each operand with the matching
    /// element type from the resolved variant. Threads the open block
    /// through; bails when a sub-expression terminates.
    fn lower_enum_tuple_fields(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        exprs: &[Expr],
        element_types: &[Type],
    ) -> Result<Option<(IRBlockId, Vec<EnumTupleFieldInit>)>, String> {
        let mut current = open;
        let mut lowered = Vec::with_capacity(exprs.len());
        for (i, expr) in exprs.iter().enumerate() {
            let (next, value, _ty) = self.lower_expr_to_operand(builder, current, expr)?;
            let field_type = element_types.get(i).cloned().unwrap_or(Type::Unknown);
            lowered.push(EnumTupleFieldInit { field_type, value });
            let Some(next) = next else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some((current, lowered)))
    }

    /// Lower each named-field initializer in an
    /// [`EnumConstructionData::Struct`] payload to a
    /// [`StructFieldInit`], matching `field_inits` against the
    /// resolved variant's `fields` (name -> (layout index, field
    /// type)) for index + type lookup. Mirrors
    /// [`Self::lower_struct_field_operands`] but resolves the index
    /// by name (variant payload structs aren't necessarily declared
    /// in source order with the construction's field initializers).
    fn lower_enum_struct_fields(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        field_inits: &[FieldInit],
        fields: &[(String, u32, Type)],
    ) -> Result<Option<(IRBlockId, Vec<StructFieldInit>)>, String> {
        let mut current = open;
        let mut lowered = Vec::with_capacity(field_inits.len());
        for field_init in field_inits {
            let (index, field_type) = fields
                .iter()
                .find(|(name, _, _)| name == &field_init.name)
                .map(|(_, idx, ty)| (*idx, ty.clone()))
                .ok_or_else(|| {
                    format!("unknown field `{}` in enum struct payload", field_init.name)
                })?;
            let (next, value, _ty) =
                self.lower_expr_to_operand(builder, current, &field_init.value)?;
            lowered.push(StructFieldInit {
                name: field_init.name.clone(),
                index,
                field_type,
                value,
            });
            let Some(next) = next else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some((current, lowered)))
    }

    /// Build the per-arm widening instruction when `target_ty` is a
    /// [`Type::Union`] and `arm_ty` is a non-union member of it.
    /// Returns `Some((dest, instruction))` when widening is needed,
    /// `None` when the operand can flow through unchanged (target
    /// isn't a union, or the operand already has a union shape).
    /// The caller is responsible for placing the instruction at the
    /// arm's exit block (immediately before its branch terminator).
    ///
    /// Transitional: shared coercion staging for the value-context
    /// control-flow constructs (`if`/`else`, `cond`, `match`,
    /// `ternary`). The future elaboration pass replaces this with a
    /// generic walk of every [`IRInstruction::Phi`] that compares
    /// each incoming operand's type against the phi's `ty` and
    /// prepends [`IRInstruction::UnionWrap`] (or future
    /// `NumericCoerce`) wherever they disagree.
    pub(crate) fn build_arm_union_wrap(
        &mut self,
        arm_op: IROperand,
        arm_ty: &Type,
        target_ty: &Type,
    ) -> Option<(crate::values::IRValueId, IRInstruction)> {
        let Type::Union(_) = target_ty else {
            return None;
        };
        if matches!(arm_ty, Type::Union(_)) {
            return None;
        }
        let dest = self.next_value_id();
        let instruction = IRInstruction::UnionWrap {
            dest,
            value: arm_op,
            source_ty: arm_ty.clone(),
            target_union: target_ty.clone(),
        };
        Some((dest, instruction))
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

/// Project the [`TypeIdentifier`] out of a [`Type::Named`], or `None`
/// for any other type shape. Used by struct-construction lowering to
/// pull the typecheck-published identifier out of `expr.resolved_type`.
fn named_identifier(ty: &Type) -> Option<&TypeIdentifier> {
    match ty {
        Type::Named { identifier, .. } => Some(identifier),
        _ => None,
    }
}

/// Project the `type_args` slice out of a [`Type::Named`], or `None`
/// for any other type shape. Used to discriminate concrete vs generic
/// instantiations at the struct-construction lowering site.
fn named_type_args(ty: &Type) -> Option<&[Type]> {
    match ty {
        Type::Named { type_args, .. } => Some(type_args),
        _ => None,
    }
}

/// Build a [`ResolvedVariantFields`] from the closure-pass-registered
/// post-substitution [`VariantData`] and the AST-side
/// [`EnumConstructionData`]. Returns `None` when the AST shape doesn't
/// match the variant's declared shape (e.g. tuple AST against a
/// struct variant), so the caller can bail to Stub.
fn generic_variant_fields(
    variant_data: &VariantData,
    construction_data: &EnumConstructionData,
) -> Option<ResolvedVariantFields> {
    match (variant_data, construction_data) {
        (VariantData::Unit, EnumConstructionData::Unit) => Some(ResolvedVariantFields::Unit),
        (VariantData::Tuple(types), EnumConstructionData::Tuple(_)) => {
            Some(ResolvedVariantFields::Tuple {
                element_types: types.clone(),
            })
        }
        (VariantData::Struct(declared), EnumConstructionData::Struct(_)) => {
            let fields = declared
                .iter()
                .enumerate()
                .map(|(i, (name, ty))| (name.clone(), i as u32, ty.clone()))
                .collect();
            Some(ResolvedVariantFields::Struct { fields })
        }
        _ => None,
    }
}
