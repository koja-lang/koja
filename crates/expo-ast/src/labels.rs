//! Short, stable labels for AST shapes used in diagnostics and panics.
//! `expr_kind_label` / `item_label` return compact kind names ("binary",
//! "fn"); `bin_op_label` renders the literal source token ("+", "and").

use crate::ast::{BinOp, ExprKind, Item};
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

pub fn item_label(item: &Item) -> &'static str {
    match item {
        Item::Alias(_) => "alias",
        Item::Constant(_) => "const",
        Item::Enum(_) => "enum",
        Item::Function(_) => "fn",
        Item::Impl(_) => "impl",
        Item::Protocol(_) => "protocol",
        Item::Struct(_) => "struct",
        Item::TypeAlias(_) => "type",
    }
}

pub fn item_span(item: &Item) -> Span {
    match item {
        Item::Alias(decl) => decl.span,
        Item::Constant(c) => c.span,
        Item::Enum(e) => e.span,
        Item::Function(f) => f.span,
        Item::Impl(i) => i.span,
        Item::Protocol(p) => p.span,
        Item::Struct(s) => s.span,
        Item::TypeAlias(t) => t.span,
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
