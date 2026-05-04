//! Lower-package sub-pass: translate one sealed [`CheckedPackage`]
//! into an [`IRPackage`] fragment.
//!
//! Pure with respect to its input. Lookup misses panic per the
//! lowering helpers contract — every reference into the AST should
//! already be resolvable thanks to the upstream seal.
//!
//! POC scope: every fn body must lower to a single basic block holding
//! `Const` / `BinaryOp` / `UnaryOp` / `Call` instructions and ending
//! in `Return`. Anything richer surfaces as a [`Diagnostic`] and the
//! offending function is dropped from the package (per-function
//! fail-fast). Seal invariant violations — e.g. a call callee with
//! `Unresolved` resolution after typecheck seal — remain panics per
//! northstar (compiler bugs, not user errors).

use std::collections::BTreeMap;

use expo_alpha_typecheck::{CheckedPackage, GlobalRegistry};
use expo_ast::ast::{
    Arg, BinOp, Diagnostic, Expr, ExprKind, Function, Item, Literal, Param, Statement, UnaryOp,
};
use expo_ast::identifier::{Identifier, Resolution};
use expo_ast::span::Span;

use crate::function::{IRBasicBlock, IRFunction, IRInstruction, IRTerminator};
use crate::package::IRPackage;
use crate::types::{ConstValue, IRBinOp, IRUnaryOp, ValueId};

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
                functions.insert(lowered.identifier.clone(), lowered);
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
    // order. This happens before lowering the body so every param
    // id is strictly less than any body-produced id — body lowering
    // stays naturally topological on the sealed AST. `self` receivers
    // are a feature gap, not a compiler bug: record a diagnostic and
    // bail on this function.
    let mut params = Vec::with_capacity(function.params.len());
    for param in &function.params {
        match param {
            Param::Regular { .. } => {
                params.push(builder.fresh());
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

    let return_value = lower_body(body, &mut builder, registry, diagnostics).ok()?;
    let block = IRBasicBlock {
        instructions: builder.instructions,
        terminator: IRTerminator::Return {
            value: return_value,
        },
    };

    Some(IRFunction {
        blocks: vec![block],
        identifier,
        params,
    })
}

/// Lower a function body to a flat instruction sequence. The "value"
/// of a body is the SSA id produced by lowering its trailing
/// expression statement, or `None` if the body is empty / ends in a
/// non-expression statement.
///
/// The `Result` is just an abort signal — a single `Err(())` means
/// "stop walking this function; a diagnostic has been pushed". The
/// caller turns that into a missing [`IRFunction`].
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
            let dest = builder.fresh();
            builder.push(IRInstruction::BinaryOp {
                dest,
                lhs,
                op: ir_op,
                rhs,
            });
            Ok(dest)
        }
        ExprKind::Call { callee, args } => lower_call(callee, args, builder, registry, diagnostics),
        ExprKind::Group { expr: inner } => lower_expr(inner, builder, registry, diagnostics),
        ExprKind::Literal { value } => {
            let const_value = lower_literal(value, expr.span, diagnostics)?;
            let dest = builder.fresh();
            builder.push(IRInstruction::Const {
                dest,
                value: const_value,
            });
            Ok(dest)
        }
        ExprKind::Unary { op, operand } => {
            let operand = lower_expr(operand, builder, registry, diagnostics)?;
            let dest = builder.fresh();
            builder.push(IRInstruction::UnaryOp {
                dest,
                op: lower_unary_op(*op),
                operand,
            });
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
    let callee_identifier = entry.identifier.clone();

    let mut lowered_args = Vec::with_capacity(args.len());
    for arg in args {
        lowered_args.push(lower_expr(&arg.value, builder, registry, diagnostics)?);
    }

    let dest = builder.fresh();
    builder.push(IRInstruction::Call {
        dest,
        callee: callee_identifier,
        args: lowered_args,
    });
    Ok(dest)
}

fn lower_literal(
    value: &Literal,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ConstValue, ()> {
    match value {
        Literal::Bool(b) => Ok(ConstValue::Bool(*b)),
        Literal::Int(text) => match text.parse::<i64>() {
            Ok(parsed) => Ok(ConstValue::Int(parsed)),
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
#[derive(Default)]
struct BlockBuilder {
    instructions: Vec<IRInstruction>,
    next_value: u32,
}

impl BlockBuilder {
    fn fresh(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn push(&mut self, inst: IRInstruction) {
        self.instructions.push(inst);
    }
}
