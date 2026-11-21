//! Builders for the unresolved AST nodes that the resolve-time
//! rewrites share. Each rewrite (`for`, `assert`, `try` / `rescue`,
//! `==` / `!=`) replaces a node with ordinary nodes and then resolves
//! them like hand-written code.
//!
//! A name that the user never wrote gets a synthetic span, so
//! reference lookups skip it.

use koja_ast::ast::{Arg, Expr, ExprKind, LValue, Name, Pattern, Statement};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

/// Assign a compiler-synthesized hidden local. The segment name is
/// synthetic so reference lookups skip it.
pub(super) fn assign_local(name: &str, value: Expr, span: Span) -> Statement {
    Statement::Assignment {
        target: LValue {
            head_resolved_type: None,
            local_id: None,
            segments: vec![Name::new(name, span.as_synthetic())],
            span,
        },
        type_annotation: None,
        value,
        span,
    }
}

/// A binding pattern at `span`. Callers pass the user's own span for
/// a written binder and a synthetic span for a hidden one.
pub(super) fn binding_pattern(name: &str, span: Span) -> Pattern {
    Pattern::Binding {
        local_id: None,
        name: Name::new(name, span),
        span,
    }
}

/// Read a compiler-synthesized hidden local. The whole expression is
/// synthetic because the user never wrote this identifier.
pub(super) fn ident(name: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::Ident {
            name: name.to_string(),
            resolution: Resolution::Unresolved,
        },
        span.as_synthetic(),
    )
}

/// Call a compiler-synthesized method. The call keeps the given span
/// and only the method name is synthetic.
pub(super) fn method_call(receiver: Expr, method: &str, args: Vec<Arg>, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: Name::new(method, span.as_synthetic()),
            args,
            target: Resolution::Unresolved,
            type_args: Vec::new(),
        },
        span,
    )
}
