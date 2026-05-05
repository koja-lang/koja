//! Lower-package sub-pass: translate one sealed [`CheckedPackage`]
//! into an [`IRPackage`] fragment.
//!
//! Pure with respect to its input. Lookup misses panic per the
//! lowering helpers contract — every reference into the AST should
//! already be resolvable thanks to the upstream seal.
//!
//! Today's scope: every fn body lowers to a control-flow graph of
//! basic blocks holding `Const` / `BinaryOp` / `UnaryOp` / `Call`
//! instructions and terminating in `Return` / `Branch` / `CondBranch`.
//! `if` / `unless` introduce extra blocks; everything else still
//! produces a single-block body. Anything richer surfaces as a
//! [`Diagnostic`] and the offending function is dropped from the
//! package (per-function fail-fast). Seal invariant violations remain
//! panics per northstar (compiler bugs, not user errors).
//!
//! Lowering threads `&mut FnLowerCtx` plus the currently-open
//! `IRBlockId` through every recursive helper. Each `lower_*`
//! returns either a [`FlowResult::Open`] carrying the produced
//! [`ValueId`] and the block to continue from, or [`FlowResult::Closed`]
//! signaling that flow already terminated (e.g. via an early `return`).
//! See [`crate::cfg`] for the [`CFGBuilder`] this context wraps.

use std::collections::BTreeMap;

use expo_alpha_typecheck::{CheckedPackage, FunctionSignature, GlobalKind, GlobalRegistry};
use expo_ast::ast::{
    Arg, BinOp, Diagnostic, Expr, ExprKind, Function, Item, Literal, Param, Statement, UnaryOp,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::cfg::CFGBuilder;
use crate::function::{
    IRBasicBlock, IRBlockId, IRFunction, IRFunctionParam, IRInstruction, IRSymbol, IRTerminator,
};
use crate::package::IRPackage;
use crate::types::{ConstValue, IRBinOp, IRType, IRUnaryOp, ValueId};

pub(crate) fn lower_package(
    pkg: &CheckedPackage,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> IRPackage {
    let mut functions = BTreeMap::new();
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && let Some(lowered) = lower_function(function, &pkg.package, registry, diagnostics)
            {
                functions.insert(lowered.symbol.clone(), lowered);
            }
        }
    }
    IRPackage {
        functions,
        package: pkg.package.clone(),
    }
}

/// Lower a single [`Function`] or return `None` if any feature-gap
/// diagnostic surfaced while lowering it. The function is simply
/// omitted from the package in that case; `lower_program` will turn
/// the accumulated diagnostics into a [`LowerError::Diagnostics`]
/// before seal runs.
fn lower_function(
    function: &Function,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IRFunction> {
    let identifier = Identifier::new(package, vec![function.name.clone()]);
    let Some(body) = function.body.as_ref() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower extern fn `{}` (no body to lower)",
                function.name,
            ),
            function.span,
        ));
        return None;
    };

    let signature = lookup_signature(registry, &identifier);
    let return_type = resolved_type_to_ir_type(&signature.return_type, registry);

    let mut ctx = FnLowerCtx::new();
    let entry = ctx.fresh_block("entry");

    // Allocate one `ValueId` per regular parameter in declaration
    // order, paired with its IRType pulled from the lifted function
    // signature on the registry. Pre-allocation ensures every param
    // id is strictly less than any body-produced id — body lowering
    // stays naturally topological on the sealed AST. `self` receivers
    // are a feature gap, not a compiler bug: record a diagnostic and
    // bail on this function.
    let mut params = Vec::with_capacity(function.params.len());
    let mut signature_index = 0;
    for param in &function.params {
        match param {
            Param::Regular { .. } => {
                let resolved = &signature.params[signature_index].ty;
                let ty = resolved_type_to_ir_type(resolved, registry);
                signature_index += 1;
                let id = ctx.fresh_value(ty.clone());
                params.push(IRFunctionParam { id, ty });
            }
            Param::Self_ { span, .. } => {
                diagnostics.push(Diagnostic::error(
                    format!("alpha IR does not yet lower `self` receivers (on `{identifier}`)",),
                    *span,
                ));
                return None;
            }
        }
    }

    let flow = lower_body(body, &mut ctx, entry, registry, diagnostics).ok()?;
    finalize_open_flow(&mut ctx, flow);

    let blocks = ctx.into_blocks();
    Some(IRFunction {
        blocks,
        params,
        return_type,
        symbol: IRSymbol::from_identifier(&identifier),
    })
}

/// Lookup the lifted [`FunctionSignature`] for `identifier` in the
/// registry. The seal contract guarantees a registered function has
/// a `Some(_)` signature stamped by `lift_signatures`, so a miss or
/// `None` here is a compiler bug, not a feature gap.
fn lookup_signature<'a>(
    registry: &'a GlobalRegistry,
    identifier: &Identifier,
) -> &'a FunctionSignature {
    let entry = registry.lookup(identifier).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: function `{identifier}` not in registry — \
             collect/seal invariant violation",
        );
    });
    match &entry.1.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!(
            "alpha IR lower: function `{identifier}` has no lifted signature \
             ({}) — lift_signatures invariant violation",
            other.label(),
        ),
    }
}

/// Lower a sequence of statements into a CFG fragment, starting in
/// `entry`. Both function bodies and script bodies share this path;
/// the caller owns the [`FnLowerCtx`] so it can pre-allocate
/// parameter `ValueId`s before any body-emitted id is allocated, and
/// inspect the trailing flow to decide how to finalize the entry
/// block (`Return` for `lower_function`; same for `lower_script`,
/// using the trailing-value type as the script's return type).
///
/// `Err(())` means "a feature-gap diagnostic was already pushed and
/// the caller should drop this body / function from the surrounding
/// fragment". This matches the per-function fail-fast policy
/// `lower_program` already implements; `lower_script` mirrors it for
/// the implicit script body.
pub(crate) fn lower_body_to_blocks(
    body: &[Statement],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(Vec<IRBasicBlock>, IRType), ()> {
    let mut ctx = FnLowerCtx::new();
    let entry = ctx.fresh_block("entry");
    let flow = lower_body(body, &mut ctx, entry, registry, diagnostics)?;
    let return_type = match &flow {
        FlowResult::Open {
            value: Some(id), ..
        } => ctx.type_of(*id),
        FlowResult::Open { value: None, .. } => IRType::Unit,
        // Closed-flow on a script body means an explicit `return`
        // exited the script. `Unit` is a defensible default here —
        // the auto-print wrapper inspects this type to pick a
        // printer, and a script that returns explicitly today only
        // does so via `return_value: Option<expr>` whose type the
        // body lowering already plumbed through `Return.value`.
        // Tightening this to "type of the returned value" is a
        // follow-up if/when scripts care.
        FlowResult::Closed => IRType::Unit,
    };
    finalize_open_flow(&mut ctx, flow);
    Ok((ctx.into_blocks(), return_type))
}

/// Walk a sequence of statements, threading the open block through
/// each one. Returns the trailing statement's flow result; an
/// empty body returns `Open { value: None, block: entry }`.
fn lower_body(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    mut block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<FlowResult, ()> {
    let mut last_value: Option<ValueId> = None;
    for stmt in body {
        match lower_statement(stmt, ctx, block, registry, diagnostics)? {
            FlowResult::Open { value, block: next } => {
                last_value = value;
                block = next;
            }
            FlowResult::Closed => return Ok(FlowResult::Closed),
        }
    }
    Ok(FlowResult::Open {
        value: last_value,
        block,
    })
}

fn lower_statement(
    stmt: &Statement,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<FlowResult, ()> {
    match stmt {
        Statement::Expr(expr) => {
            let (value, next) = lower_expr(expr, ctx, block, registry, diagnostics)?;
            Ok(FlowResult::Open {
                value: Some(value),
                block: next,
            })
        }
        Statement::Return { value, .. } => {
            let return_value = match value.as_ref() {
                Some(expr) => {
                    let (id, next) = lower_expr(expr, ctx, block, registry, diagnostics)?;
                    ctx.cfg
                        .set_terminator(next, IRTerminator::Return { value: Some(id) });
                    Some(id)
                }
                None => {
                    ctx.cfg
                        .set_terminator(block, IRTerminator::Return { value: None });
                    None
                }
            };
            // Suppress the unused-binding warning while keeping the
            // shape parallel to the `if` / `unless` branches that
            // care about the returned value.
            let _ = return_value;
            Ok(FlowResult::Closed)
        }
        Statement::Assignment { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower `=` assignment statements",
                *span,
            ));
            Err(())
        }
        Statement::CompoundAssign { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower compound assignment statements",
                *span,
            ));
            Err(())
        }
        Statement::Break { span } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower `break` statements",
                *span,
            ));
            Err(())
        }
    }
}

fn lower_expr(
    expr: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let (lhs, block) = lower_expr(left, ctx, block, registry, diagnostics)?;
            let (rhs, block) = lower_expr(right, ctx, block, registry, diagnostics)?;
            let ir_op = lower_bin_op(*op, expr.span, diagnostics)?;
            let result_ty = bin_op_result_type(ir_op, ctx.type_of(lhs));
            let dest = ctx.fresh_value(result_ty);
            ctx.cfg.append(
                block,
                IRInstruction::BinaryOp {
                    dest,
                    lhs,
                    op: ir_op,
                    rhs,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, ctx, block, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => lower_expr(inner, ctx, block, registry, diagnostics),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            if else_body.is_some() {
                diagnostics.push(Diagnostic::error(
                    "alpha IR does not yet lower `else` branches",
                    expr.span,
                ));
                return Err(());
            }
            lower_if(condition, then_body, ctx, block, registry, diagnostics)
        }
        ExprKind::Literal { value } => {
            let const_value = lower_literal(value, expr.span, diagnostics)?;
            let ty = const_value_type(&const_value);
            let dest = ctx.fresh_value(ty);
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest,
                    value: const_value,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Unary { op, operand } => {
            let (operand, block) = lower_expr(operand, ctx, block, registry, diagnostics)?;
            let ir_op = lower_unary_op(*op);
            let result_ty = unary_op_result_type(ir_op, ctx.type_of(operand));
            let dest = ctx.fresh_value(result_ty);
            ctx.cfg.append(
                block,
                IRInstruction::UnaryOp {
                    dest,
                    op: ir_op,
                    operand,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Unless { condition, body } => {
            lower_unless(condition, body, ctx, block, registry, diagnostics)
        }
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha IR does not yet lower this expression kind ({})",
                    expr_kind_label(other),
                ),
                expr.span,
            ));
            Err(())
        }
    }
}

/// Lower an `if cond do then_body end` (no-else). Adds a then-block
/// and a merge-block; terminates the current block with a
/// `CondBranch` to those; lowers the then-body inside the then-block
/// and falls through to merge unless the body closed flow with an
/// early `return`. Always produces a fresh `Const::Unit` in the
/// merge block as the if-expression's value. Caller is responsible
/// for rejecting `else_body.is_some()` upstream — this slice has no
/// path to lower it (value-producing `if` / `else` lands with the
/// locals slice).
fn lower_if(
    condition: &Expr,
    then_body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, diagnostics)?;
    let then_block = ctx.fresh_block("if_then");
    let merge_block = ctx.fresh_block("if_merge");
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            then_block,
            else_block: merge_block,
        },
    );

    lower_arm_into(
        then_body,
        ctx,
        then_block,
        merge_block,
        registry,
        diagnostics,
    )?;

    Ok((emit_unit(ctx, merge_block), merge_block))
}

/// Lower an `unless cond do body end`. Identical wiring to `if` with
/// the arms swapped: cond=`true` skips to merge, cond=`false` runs
/// the body.
fn lower_unless(
    condition: &Expr,
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, diagnostics)?;
    let body_block = ctx.fresh_block("unless_body");
    let merge_block = ctx.fresh_block("unless_merge");
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            then_block: merge_block,
            else_block: body_block,
        },
    );

    lower_arm_into(body, ctx, body_block, merge_block, registry, diagnostics)?;

    Ok((emit_unit(ctx, merge_block), merge_block))
}

/// Lower an arm of an `if` / `unless`: walk the body in `arm_block`,
/// then unconditionally jump to `merge_block` if the flow is still
/// open. Closed flow (early `return` inside the arm) leaves the
/// terminator already set; we don't overwrite it.
fn lower_arm_into(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    arm_block: IRBlockId,
    merge_block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ()> {
    match lower_body(body, ctx, arm_block, registry, diagnostics)? {
        FlowResult::Open { block, .. } => {
            ctx.cfg
                .set_terminator(block, IRTerminator::Branch(merge_block));
        }
        FlowResult::Closed => {}
    }
    Ok(())
}

/// Emit a fresh `Const::Unit` in `block` and return its `ValueId`.
fn emit_unit(ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let dest = ctx.fresh_value(IRType::Unit);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest,
            value: ConstValue::Unit,
        },
    );
    dest
}

/// Wire a still-open trailing flow up to its function's `Return`.
/// Closed flows already set their own terminator (an inner `return`);
/// nothing to do.
fn finalize_open_flow(ctx: &mut FnLowerCtx, flow: FlowResult) {
    if let FlowResult::Open { value, block } = flow {
        ctx.cfg
            .set_terminator(block, IRTerminator::Return { value });
    }
}

/// Lower a `ExprKind::Call`. The seal contract guarantees the callee
/// is a bare `Ident` whose inner `Resolution` is `Global(id)` — any
/// deviation is a compiler bug, not a feature gap, so we panic rather
/// than emit a diagnostic.
fn lower_call(
    callee: &Expr,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let ExprKind::Ident { resolution, name } = &callee.kind else {
        panic!(
            "alpha IR lower: call callee must be a bare Ident after typecheck seal (got {:?})",
            callee.kind,
        );
    };
    let Resolution::Global(id) = resolution else {
        panic!("alpha IR lower: callee `{name}` has Unresolved resolution after typecheck seal",);
    };
    let entry = registry.get(*id).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: callee id {id} not present in the registry — \
             seal invariant violation",
        )
    });
    let signature = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!(
            "alpha IR lower: callee `{}` resolved to non-function entry ({}) — \
             typecheck seal violation",
            entry.identifier,
            other.label(),
        ),
    };
    let return_ty = resolved_type_to_ir_type(&signature.return_type, registry);
    let callee_symbol = IRSymbol::from_identifier(&entry.identifier);

    let mut lowered_args = Vec::with_capacity(args.len());
    let mut current = block;
    for arg in args {
        let (value, next) = lower_expr(&arg.value, ctx, current, registry, diagnostics)?;
        lowered_args.push(value);
        current = next;
    }

    let dest = ctx.fresh_value(return_ty);
    ctx.cfg.append(
        current,
        IRInstruction::Call {
            dest,
            callee: callee_symbol,
            args: lowered_args,
        },
    );
    Ok((dest, current))
}

fn lower_literal(
    value: &Literal,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ConstValue, ()> {
    match value {
        Literal::Bool(b) => Ok(ConstValue::Bool(*b)),
        // Slice scope: every Int literal lowers to the 64-bit signed
        // variant. Once stdlib stubs grow `Int8`..`UInt64` and literal
        // width inference lands, this match grows arms (or threads
        // expected width through from typecheck).
        Literal::Int(text) => match text.parse::<i64>() {
            Ok(parsed) => Ok(ConstValue::Int64(parsed)),
            Err(err) => {
                diagnostics.push(Diagnostic::error(
                    format!("invalid Int literal `{text}`: {err}"),
                    span,
                ));
                Err(())
            }
        },
        Literal::Unit => Ok(ConstValue::Unit),
        Literal::Float(_) => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower Float literals",
                span,
            ));
            Err(())
        }
        Literal::String(_) => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower String literals",
                span,
            ));
            Err(())
        }
    }
}

fn lower_bin_op(op: BinOp, span: Span, diagnostics: &mut Vec<Diagnostic>) -> Result<IRBinOp, ()> {
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
        BinOp::Concat => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower the `<>` concat operator",
                span,
            ));
            Err(())
        }
    }
}

fn lower_unary_op(op: UnaryOp) -> IRUnaryOp {
    match op {
        UnaryOp::Neg => IRUnaryOp::Neg,
        UnaryOp::Not => IRUnaryOp::Not,
    }
}

/// Map a [`ConstValue`] variant to its [`IRType`]. Pure
/// transliteration — each integer width gets its mirroring type, and
/// `Bool` / `Unit` round-trip directly.
fn const_value_type(value: &ConstValue) -> IRType {
    match value {
        ConstValue::Bool(_) => IRType::Bool,
        ConstValue::Int8(_) => IRType::Int8,
        ConstValue::Int16(_) => IRType::Int16,
        ConstValue::Int32(_) => IRType::Int32,
        ConstValue::Int64(_) => IRType::Int64,
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
fn bin_op_result_type(op: IRBinOp, operand_ty: IRType) -> IRType {
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
fn unary_op_result_type(op: IRUnaryOp, operand_ty: IRType) -> IRType {
    match op {
        IRUnaryOp::Neg => operand_ty,
        IRUnaryOp::Not => IRType::Bool,
    }
}

/// Translate a typecheck-resolved [`ResolvedType`] to an [`IRType`].
///
/// Today the alpha registry's stdlib stubs only cover the scalars
/// alpha typecheck synthesizes from literals (`Int`, `Bool`, `Unit`,
/// `Float`, `String`). Anything else — width-explicit ints, user
/// structs, polymorphic containers — is a feature gap and panics with
/// a "not yet translatable" message. As stdlib stubs grow this match
/// grows in lockstep.
fn resolved_type_to_ir_type(ty: &ResolvedType, registry: &GlobalRegistry) -> IRType {
    let Resolution::Global(id) = ty.resolution else {
        panic!(
            "alpha IR lower: ResolvedType has Unresolved resolution after typecheck seal — \
             compiler bug",
        );
    };
    let entry = registry.get(id).unwrap_or_else(|| {
        panic!("alpha IR lower: ResolvedType id {id} missing from registry — seal violation",)
    });
    if !entry.identifier.is_in_package("Global") {
        panic!(
            "alpha IR lower: cannot translate non-`Global` type `{}` to IRType yet",
            entry.identifier,
        );
    }
    match entry.identifier.last() {
        "Int" => IRType::Int64,
        "Bool" => IRType::Bool,
        "Unit" => IRType::Unit,
        other => panic!(
            "alpha IR lower: cannot translate `Global.{other}` to IRType yet \
             (Float / String / width-explicit ints land in follow-up slices)",
        ),
    }
}

/// Short, user-facing label for an [`ExprKind`] that the alpha IR
/// cannot yet lower. Kept local because it only serves feature-gap
/// diagnostics; a public `ExprKind::label()` would imply stability
/// guarantees we aren't ready to make.
fn expr_kind_label(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Binary { .. } => "binary expression",
        ExprKind::BinaryLiteral { .. } => "binary literal",
        ExprKind::Call { .. } => "call",
        ExprKind::Closure { .. } => "closure",
        ExprKind::Cond { .. } => "cond",
        ExprKind::EnumConstruction { .. } => "enum construction",
        ExprKind::FieldAccess { .. } => "field access",
        ExprKind::For { .. } => "for",
        ExprKind::Group { .. } => "group",
        ExprKind::Ident { .. } => "identifier reference",
        ExprKind::If { .. } => "if",
        ExprKind::List { .. } => "list literal",
        ExprKind::Literal { .. } => "literal",
        ExprKind::Loop { .. } => "loop",
        ExprKind::Map { .. } => "map literal",
        ExprKind::Match { .. } => "match",
        ExprKind::MethodCall { .. } => "method call",
        ExprKind::Receive { .. } => "receive",
        ExprKind::Self_ => "self reference",
        ExprKind::ShortClosure { .. } => "short closure",
        ExprKind::Spawn { .. } => "spawn",
        ExprKind::String { .. } => "string interpolation",
        ExprKind::StructConstruction { .. } => "struct construction",
        ExprKind::Ternary { .. } => "ternary",
        ExprKind::Unary { .. } => "unary",
        ExprKind::Unless { .. } => "unless",
        ExprKind::While { .. } => "while",
    }
}

/// The shape every `lower_*` helper returns. `Open` carries the
/// trailing value (when the construct produces one) and the block
/// where flow continues; `Closed` signals that an inner statement
/// already terminated the function (the only path today is
/// `Statement::Return`). Closed branches don't fall through to a
/// surrounding merge block — the caller's wiring inspects `is_closed`
/// on the relevant block via the `CFGBuilder`'s closed-set, or sees
/// `FlowResult::Closed` directly from this enum.
#[derive(Debug, Clone)]
enum FlowResult {
    Open {
        value: Option<ValueId>,
        block: IRBlockId,
    },
    Closed,
}

/// Per-function lowering context. Owns the [`CFGBuilder`] plus the
/// `ValueId` / `IRBlockId` counters and a `value -> IRType` index
/// callers consult to derive operator result types and the function's
/// return type without re-querying the typecheck registry.
///
/// One context per `IRFunction` (or per script body). Discarded after
/// the function's blocks are extracted via [`Self::into_blocks`];
/// downstream consumers (seal, backends) build their own indices.
pub(crate) struct FnLowerCtx {
    pub(crate) cfg: CFGBuilder,
    next_value: u32,
    next_block: u32,
    value_types: BTreeMap<ValueId, IRType>,
}

impl FnLowerCtx {
    pub(crate) fn new() -> Self {
        Self {
            cfg: CFGBuilder::new(),
            next_value: 0,
            next_block: 0,
            value_types: BTreeMap::new(),
        }
    }

    /// Mint a fresh `ValueId` and record its `IRType`.
    pub(crate) fn fresh_value(&mut self, ty: IRType) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        self.value_types.insert(id, ty);
        id
    }

    /// Mint a fresh `IRBlockId` and add the corresponding empty
    /// block to the [`CFGBuilder`].
    pub(crate) fn fresh_block(&mut self, label: impl Into<String>) -> IRBlockId {
        let id = IRBlockId(self.next_block);
        self.next_block += 1;
        self.cfg.add_block(id, label);
        id
    }

    /// Lookup the recorded `IRType` for `id`. Panics on a miss —
    /// every emitted `ValueId` registers its type at allocation time,
    /// so a miss is a lowering bug.
    pub(crate) fn type_of(&self, id: ValueId) -> IRType {
        self.value_types
            .get(&id)
            .cloned()
            .unwrap_or_else(|| panic!("alpha IR lower: missing type for {id} — lowering bug"))
    }

    /// Consume the context and return the accumulated block list.
    /// Asserts via `CFGBuilder`'s closed-set that every block has had
    /// a real terminator stamped — an unclosed block reaching the
    /// caller is a lowering bug.
    pub(crate) fn into_blocks(self) -> Vec<IRBasicBlock> {
        let (blocks, closed) = self.cfg.into_blocks_with_closed();
        for block in &blocks {
            if !closed.contains_key(&block.id) {
                panic!(
                    "alpha IR lower: block {} ({}) was opened but never had its terminator set — \
                     lowering bug",
                    block.id, block.label,
                );
            }
        }
        blocks
    }
}
