//! Lower-package sub-pass: translate one sealed [`CheckedPackage`]
//! into an [`IRPackage`] fragment.
//!
//! Pure with respect to its input. Lookup misses panic per the
//! lowering helpers contract — every reference into the AST should
//! already be resolvable thanks to the upstream seal.
//!
//! Today's scope: every fn body must lower to a single basic block
//! holding `Const` / `BinaryOp` / `UnaryOp` / `Call` instructions and
//! ending in `Return`. Anything richer surfaces as a [`Diagnostic`]
//! and the offending function is dropped from the package
//! (per-function fail-fast). Seal invariant violations — e.g. a call
//! callee with `Unresolved` resolution after typecheck seal — remain
//! panics per northstar (compiler bugs, not user errors).
//!
//! Type tracking: the [`BlockBuilder`] tracks an [`IRType`] for every
//! emitted [`ValueId`]. Each lowering helper that produces a fresh
//! value records its result type at push time, so the trailing
//! expression's type is available for stamping on
//! [`IRFunction::return_type`] without re-querying the typecheck
//! registry. `Call` is the one helper that does query the registry —
//! callee return types live on the [`FunctionSignature`].

use std::collections::BTreeMap;

use expo_alpha_typecheck::{CheckedPackage, FunctionSignature, GlobalKind, GlobalRegistry};
use expo_ast::ast::{
    Arg, BinOp, Diagnostic, Expr, ExprKind, Function, Item, Literal, Param, Statement, UnaryOp,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::function::{
    IRBasicBlock, IRFunction, IRFunctionParam, IRInstruction, IRSymbol, IRTerminator,
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

    let mut builder = BlockBuilder::default();

    // Allocate one `ValueId` per regular parameter in declaration
    // order, paired with its IRType pulled from the lifted function
    // signature on the registry. Pre-allocation ensures every param
    // id is strictly less than any body-produced id — body lowering
    // stays naturally topological on the sealed AST. `self` receivers
    // are a feature gap, not a compiler bug: record a diagnostic and
    // bail on this function.
    let signature = lookup_signature(registry, &identifier);
    let mut params = Vec::with_capacity(function.params.len());
    let mut signature_index = 0;
    for param in &function.params {
        match param {
            Param::Regular { .. } => {
                let resolved = &signature.params[signature_index].ty;
                let ty = resolved_type_to_ir_type(resolved, registry);
                signature_index += 1;
                params.push(IRFunctionParam {
                    id: builder.fresh(),
                    ty,
                });
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

    let (blocks, return_type) =
        lower_body_to_blocks(body, &mut builder, registry, diagnostics).ok()?;

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

/// Lower a sequence of statements into the single-block, single-return
/// IR shape that both function bodies and script bodies share today.
///
/// The caller owns the [`BlockBuilder`]; this lets `lower_function`
/// pre-allocate parameter `ValueId`s before any body-emitted id is
/// allocated, while `lower_script` can pass a fresh builder. On
/// success the builder is consumed into a single [`IRBasicBlock`]
/// terminated by `Return` with the trailing expression's value (or
/// `Unit`, if the body has no trailing expression value).
///
/// `Err(())` means "a feature-gap diagnostic was already pushed and
/// the caller should drop this body / function from the surrounding
/// fragment". This matches the per-function fail-fast policy
/// `lower_program` already implements; `lower_script` mirrors it for
/// the implicit script body.
pub(crate) fn lower_body_to_blocks(
    body: &[Statement],
    builder: &mut BlockBuilder,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(Vec<IRBasicBlock>, IRType), ()> {
    let return_value = lower_body(body, builder, registry, diagnostics)?;
    let return_type = match return_value {
        Some(id) => builder.type_of(id),
        None => IRType::Unit,
    };
    let block = IRBasicBlock {
        instructions: std::mem::take(&mut builder.instructions),
        terminator: IRTerminator::Return {
            value: return_value,
        },
    };
    Ok((vec![block], return_type))
}

/// Walk a sequence of statements, lowering each through the existing
/// statement helper. Returns the trailing statement's `ValueId` or
/// `None` for an empty body / a body that ends in a non-expression
/// statement.
///
/// `Err(())` is just an abort signal: a single error means "stop
/// walking; a diagnostic has been pushed".
fn lower_body(
    body: &[Statement],
    builder: &mut BlockBuilder,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<ValueId>, ()> {
    let mut last_value = None;
    for stmt in body {
        last_value = lower_statement(stmt, builder, registry, diagnostics)?;
    }
    Ok(last_value)
}

fn lower_statement(
    stmt: &Statement,
    builder: &mut BlockBuilder,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<ValueId>, ()> {
    match stmt {
        Statement::Expr(expr) => Ok(Some(lower_expr(expr, builder, registry, diagnostics)?)),
        Statement::Return { value, .. } => match value.as_ref() {
            Some(expr) => Ok(Some(lower_expr(expr, builder, registry, diagnostics)?)),
            None => Ok(None),
        },
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
    builder: &mut BlockBuilder,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ValueId, ()> {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let lhs = lower_expr(left, builder, registry, diagnostics)?;
            let rhs = lower_expr(right, builder, registry, diagnostics)?;
            let ir_op = lower_bin_op(*op, expr.span, diagnostics)?;
            let result_ty = bin_op_result_type(ir_op, builder.type_of(lhs));
            let dest = builder.fresh();
            builder.push_typed(
                IRInstruction::BinaryOp {
                    dest,
                    lhs,
                    op: ir_op,
                    rhs,
                },
                dest,
                result_ty,
            );
            Ok(dest)
        }
        ExprKind::Call { callee, args } => lower_call(callee, args, builder, registry, diagnostics),
        ExprKind::Group { expr: inner } => lower_expr(inner, builder, registry, diagnostics),
        ExprKind::Literal { value } => {
            let const_value = lower_literal(value, expr.span, diagnostics)?;
            let ty = const_value_type(&const_value);
            let dest = builder.fresh();
            builder.push_typed(
                IRInstruction::Const {
                    dest,
                    value: const_value,
                },
                dest,
                ty,
            );
            Ok(dest)
        }
        ExprKind::Unary { op, operand } => {
            let operand = lower_expr(operand, builder, registry, diagnostics)?;
            let ir_op = lower_unary_op(*op);
            let result_ty = unary_op_result_type(ir_op, builder.type_of(operand));
            let dest = builder.fresh();
            builder.push_typed(
                IRInstruction::UnaryOp {
                    dest,
                    op: ir_op,
                    operand,
                },
                dest,
                result_ty,
            );
            Ok(dest)
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

/// Lower a `ExprKind::Call`. The seal contract guarantees the callee
/// is a bare `Ident` whose inner `Resolution` is `Global(id)` — any
/// deviation is a compiler bug, not a feature gap, so we panic rather
/// than emit a diagnostic.
fn lower_call(
    callee: &Expr,
    args: &[Arg],
    builder: &mut BlockBuilder,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ValueId, ()> {
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
    for arg in args {
        lowered_args.push(lower_expr(&arg.value, builder, registry, diagnostics)?);
    }

    let dest = builder.fresh();
    builder.push_typed(
        IRInstruction::Call {
            dest,
            callee: callee_symbol,
            args: lowered_args,
        },
        dest,
        return_ty,
    );
    Ok(dest)
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

/// Builder for a single basic block: tracks the instruction list and
/// hands out fresh SSA value ids. Reset / replaced when control flow
/// lands and lower starts emitting multiple blocks.
///
/// Also tracks an [`IRType`] for every emitted [`ValueId`] so callers
/// can ask "what type does this value have?" at any point during
/// lowering — used to derive operator result types and stamp
/// [`IRFunction::return_type`] from the trailing expression.
#[derive(Default)]
pub(crate) struct BlockBuilder {
    pub(crate) instructions: Vec<IRInstruction>,
    next_value: u32,
    value_types: BTreeMap<ValueId, IRType>,
}

impl BlockBuilder {
    fn fresh(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn push_typed(&mut self, inst: IRInstruction, dest: ValueId, ty: IRType) {
        debug_assert_eq!(
            inst.dest(),
            dest,
            "push_typed: instruction dest must match the type-tracker key",
        );
        self.value_types.insert(dest, ty);
        self.instructions.push(inst);
    }

    fn type_of(&self, id: ValueId) -> IRType {
        self.value_types
            .get(&id)
            .cloned()
            .unwrap_or_else(|| panic!("alpha IR lower: missing type for {id} — lowering bug"))
    }
}
