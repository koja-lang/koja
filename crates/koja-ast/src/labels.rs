//! Short, stable labels for AST shapes used in diagnostics and panics.
//! `expr_kind_label` returns a compact kind name (e.g. `"binary"`);
//! `bin_op_label` renders the literal source token (`"+"`, `"and"`).

use crate::ast::{BinOp, CompoundOp, ExprKind, Pattern};
use crate::span::Span;

pub fn expr_kind_label(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Binary { .. } => "binary",
        ExprKind::BinaryLiteral { .. } => "binary-literal",
        ExprKind::Call { .. } => "call",
        ExprKind::Closure { .. } => "closure",
        ExprKind::Cond { .. } => "cond",
        ExprKind::EnumConstruction { .. } => "enum-construction",
        ExprKind::FieldAccess { .. } => "field-access",
        ExprKind::For { .. } => "for",
        ExprKind::Group { .. } => "group",
        ExprKind::Ident { .. } => "ident",
        ExprKind::If { .. } => "if",
        ExprKind::List { .. } => "list",
        ExprKind::Literal { .. } => "literal",
        ExprKind::Loop { .. } => "loop",
        ExprKind::Map { .. } => "map",
        ExprKind::Match { .. } => "match",
        ExprKind::MethodCall { .. } => "method-call",
        ExprKind::Receive { .. } => "receive",
        ExprKind::Self_ { .. } => "self",
        ExprKind::ShortClosure { .. } => "short-closure",
        ExprKind::Spawn { .. } => "spawn",
        ExprKind::String { .. } => "string",
        ExprKind::StructConstruction { .. } => "struct-construction",
        ExprKind::Ternary { .. } => "ternary",
        ExprKind::Unary { .. } => "unary",
        ExprKind::Unless { .. } => "unless",
        ExprKind::While { .. } => "while",
    }
}

pub fn pattern_kind_label(pattern: &Pattern) -> &'static str {
    match pattern {
        Pattern::Binary { .. } => "binary",
        Pattern::Binding { .. } => "binding",
        Pattern::Constructor { .. } => "constructor",
        Pattern::EnumStruct { .. } => "enum-struct",
        Pattern::EnumTuple { .. } => "enum-tuple",
        Pattern::EnumUnit { .. } => "enum-unit",
        Pattern::List { .. } => "list",
        Pattern::Literal { .. } => "literal",
        Pattern::Or { .. } => "or",
        Pattern::Struct { .. } => "struct",
        Pattern::TypedBinding { .. } => "typed-binding",
        Pattern::Wildcard { .. } => "wildcard",
    }
}

pub fn pattern_span(pattern: &Pattern) -> Span {
    match pattern {
        Pattern::Binary { span, .. }
        | Pattern::Binding { span, .. }
        | Pattern::Constructor { span, .. }
        | Pattern::EnumStruct { span, .. }
        | Pattern::EnumTuple { span, .. }
        | Pattern::EnumUnit { span, .. }
        | Pattern::List { span, .. }
        | Pattern::Literal { span, .. }
        | Pattern::Or { span, .. }
        | Pattern::Struct { span, .. }
        | Pattern::TypedBinding { span, .. }
        | Pattern::Wildcard { span, .. } => *span,
    }
}

pub fn bin_op_label(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::And => "and",
        BinOp::Concat => "<>",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Mod => "%",
        BinOp::Mul => "*",
        BinOp::NotEq => "!=",
        BinOp::Or => "or",
        BinOp::Sub => "-",
    }
}

/// Source-token rendering of the binary part of a compound operator
/// (so `+=` reports as `+`, matching `bin_op_label`).
pub fn compound_op_label(op: CompoundOp) -> &'static str {
    match op {
        CompoundOp::Add => "+",
        CompoundOp::Div => "/",
        CompoundOp::Mul => "*",
        CompoundOp::Sub => "-",
    }
}
