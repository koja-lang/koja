//! Lower-package sub-pass: translate one sealed [`CheckedPackage`]
//! into an [`IRPackage`] fragment.
//!
//! Pure with respect to its input. Lookup misses panic per the
//! lowering helpers contract — every reference into the AST should
//! already be resolvable thanks to the upstream seal.
//!
//! POC scope: every fn body must lower to a single basic block holding
//! `Const` / `BinaryOp` instructions and ending in `Return`. Anything
//! richer panics until the corresponding feature lands.

use std::collections::BTreeMap;

use expo_ast::ast::{BinOp, Expr, ExprKind, Function, Item, Literal, Statement};
use expo_ast::identifier::Identifier;
use expo_typecheck_v2::CheckedPackage;

use crate::function::{IRBasicBlock, IRFunction, IRInstruction, IRTerminator};
use crate::package::IRPackage;
use crate::types::{ConstValue, IRBinOp, ValueId};

pub(crate) fn lower_package(pkg: &CheckedPackage) -> IRPackage {
    let mut functions = BTreeMap::new();
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item {
                let lowered = lower_function(function, &pkg.package);
                functions.insert(lowered.identifier.clone(), lowered);
            }
        }
    }
    IRPackage {
        functions,
        package: pkg.package.clone(),
    }
}

fn lower_function(function: &Function, package: &str) -> IRFunction {
    let identifier = Identifier::new(package, vec![function.name.clone()]);
    let body = function
        .body
        .as_ref()
        .expect("v2 IR POC does not yet support extern fns");

    let mut builder = BlockBuilder::default();
    let return_value = lower_body(body, &mut builder);
    let block = IRBasicBlock {
        instructions: builder.instructions,
        terminator: IRTerminator::Return {
            value: return_value,
        },
    };

    IRFunction {
        blocks: vec![block],
        identifier,
    }
}

/// Lower a function body to a flat instruction sequence. The "value"
/// of a body is the SSA id produced by lowering its trailing
/// expression statement, or `None` if the body is empty / ends in a
/// non-expression statement.
fn lower_body(body: &[Statement], builder: &mut BlockBuilder) -> Option<ValueId> {
    let mut last_value = None;
    for stmt in body {
        last_value = lower_statement(stmt, builder);
    }
    last_value
}

fn lower_statement(stmt: &Statement, builder: &mut BlockBuilder) -> Option<ValueId> {
    match stmt {
        Statement::Expr(expr) => Some(lower_expr(expr, builder)),
        Statement::Return { value, .. } => value.as_ref().map(|expr| lower_expr(expr, builder)),
        Statement::Assignment { .. }
        | Statement::Break { .. }
        | Statement::CompoundAssign { .. } => {
            panic!("v2 IR POC does not yet lower this statement kind: {stmt:?}")
        }
    }
}

fn lower_expr(expr: &Expr, builder: &mut BlockBuilder) -> ValueId {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let lhs = lower_expr(left, builder);
            let rhs = lower_expr(right, builder);
            let dest = builder.fresh();
            builder.push(IRInstruction::BinaryOp {
                dest,
                lhs,
                op: lower_bin_op(*op),
                rhs,
            });
            dest
        }
        ExprKind::Group { expr: inner } => lower_expr(inner, builder),
        ExprKind::Literal { value } => {
            let dest = builder.fresh();
            builder.push(IRInstruction::Const {
                dest,
                value: lower_literal(value),
            });
            dest
        }
        other => panic!("v2 IR POC does not yet lower expression kind {other:?}"),
    }
}

fn lower_literal(value: &Literal) -> ConstValue {
    match value {
        Literal::Bool(b) => ConstValue::Bool(*b),
        Literal::Int(text) => {
            let parsed = text
                .parse::<i64>()
                .unwrap_or_else(|err| panic!("invalid Int literal `{text}`: {err}"));
            ConstValue::Int(parsed)
        }
        Literal::Unit => ConstValue::Unit,
        Literal::Float(_) | Literal::String(_) => {
            panic!("v2 IR POC does not yet lower this literal kind: {value:?}")
        }
    }
}

fn lower_bin_op(op: BinOp) -> IRBinOp {
    match op {
        BinOp::Add => IRBinOp::Add,
        BinOp::Div => IRBinOp::Div,
        BinOp::Mod => IRBinOp::Mod,
        BinOp::Mul => IRBinOp::Mul,
        BinOp::Sub => IRBinOp::Sub,
        other => panic!("v2 IR POC does not yet lower binary operator {other:?}"),
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
