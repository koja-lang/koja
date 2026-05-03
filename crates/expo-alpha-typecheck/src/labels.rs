//! Short, stable, human-readable labels for AST shapes. Used by
//! `resolve` and `seal` for diagnostic / panic messages so the same
//! vocabulary surfaces from every sub-pass.

use expo_ast::ast::{ExprKind, Item};

pub(crate) fn expr_kind_label(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Arena { .. } => "arena",
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
        ExprKind::Self_ => "self",
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
        Item::Shared(_) => "shared",
        Item::Struct(_) => "struct",
        Item::TypeAlias(_) => "type",
    }
}

pub(crate) fn item_span(item: &Item) -> expo_ast::span::Span {
    match item {
        Item::Alias(decl) => decl.span,
        Item::Constant(c) => c.span,
        Item::Enum(e) => e.span,
        Item::Function(f) => f.span,
        Item::Impl(i) => i.span,
        Item::Protocol(p) => p.span,
        Item::Shared(s) => s.span,
        Item::Struct(s) => s.span,
        Item::TypeAlias(t) => t.span,
    }
}
