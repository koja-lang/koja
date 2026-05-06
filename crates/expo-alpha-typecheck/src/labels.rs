//! Short, stable, human-readable labels for AST shapes. Used by every
//! pipeline pass that needs to mention an [`ExprKind`] / [`Item`] /
//! [`BinOp`] in a diagnostic or panic message, so the same vocabulary
//! surfaces from `collect`, `lift_signatures`, `resolve`, and `seal`.
//!
//! The split between [`expr_kind_label`] / [`item_label`] (compact
//! kind names like `"binary"` / `"fn"`) and [`bin_op_label`] (surface
//! syntax like `"+"` / `"and"`) is deliberate: the former describe
//! the AST node *kind*, the latter render the *literal source token*
//! a user would have typed. Both flavors live here because they're
//! the same audience (diagnostics) and same shape (AST → `&'static str`).

use expo_ast::ast::{BinOp, ExprKind, Item};
use expo_ast::span::Span;

pub(crate) fn expr_kind_label(kind: &ExprKind) -> &'static str {
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

pub(crate) fn item_label(item: &Item) -> &'static str {
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

pub(crate) fn item_span(item: &Item) -> Span {
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

/// Surface-syntax rendering of a binary operator for user-facing
/// diagnostic messages (`"+"`, `"and"`, `"<>"`, …). Distinct from
/// [`expr_kind_label`]: the latter returns the kind name (`"binary"`),
/// this returns what the user actually typed.
pub(crate) fn bin_op_label(op: BinOp) -> &'static str {
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
