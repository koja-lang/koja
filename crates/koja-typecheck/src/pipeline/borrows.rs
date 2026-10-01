//! Post-resolve position check for `CPtr.borrow`. The borrowed
//! pointer is a zero-cost view of a `Binary`'s payload, valid only
//! while the source is live. Consuming it within the borrowing
//! statement (call argument, chained receiver) is always safe under
//! ordinary scope-exit drop semantics, so those are the only legal
//! positions. Binding, returning, or storing the result is rejected
//! with a teaching diagnostic pointing at `CPtr.copy`.

use koja_ast::ast::{
    Diagnostic, Expr, ExprKind, File, Function, LValue, Param, ProtocolMethod, Statement, path_text,
};
use koja_ast::identifier::Resolution;
use koja_ast::visit::{self, Visitor};

use crate::registry::GlobalRegistry;

/// How the expression position under inspection treats a
/// `CPtr.borrow` result.
#[derive(Clone, Copy)]
enum Position<'a> {
    /// Right-hand side of `target = ...`.
    Bound(&'a LValue),
    /// Consumed in-statement as a call argument or chained receiver.
    Consumed,
    /// Any other position (struct field, collection element, ...).
    Escaping,
    /// Explicit `return` or implicit tail-expression return.
    Returned,
}

pub(crate) fn check_file(
    file: &File,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut walker = Walker {
        diagnostics,
        position: Position::Escaping,
        registry,
    };
    walker.visit_file(file);
}

/// Recursion state for one file. `registry` answers the
/// `CPtr.borrow` identity check, `diagnostics` collects the escapes,
/// and `position` is the position of the node about to be visited.
struct Walker<'a> {
    diagnostics: &'a mut Vec<Diagnostic>,
    position: Position<'a>,
    registry: &'a GlobalRegistry,
}

impl<'a> Walker<'a> {
    /// Run `visit` with `position` in place, then restore the
    /// caller's position for the siblings that follow.
    fn at(&mut self, position: Position<'a>, visit: impl FnOnce(&mut Self)) {
        let parent = std::mem::replace(&mut self.position, position);
        visit(self);
        self.position = parent;
    }

    /// Walk a statement body. The tail statement, when it is a bare
    /// expression, produces the body's value, so it checks against
    /// `tail` (implicit return for function/closure bodies) instead of
    /// the ordinary statement positions.
    fn check_body(&mut self, body: &'a [Statement], tail: Position<'a>) {
        let Some((last, leading)) = body.split_last() else {
            return;
        };
        visit::walk_body(self, leading);
        match last {
            Statement::Expr(expr) => self.at(tail, |w| w.visit_expr(expr)),
            other => self.visit_statement(other),
        }
    }

    /// Params carry defaults. The body's tail expression is the
    /// implicit return.
    fn check_callable(&mut self, params: &'a [Param], body: Option<&'a [Statement]>) {
        for param in params {
            self.visit_param(param);
        }
        if let Some(body) = body {
            self.check_body(body, Position::Returned);
        }
    }

    fn emit_escape(&mut self, expr: &Expr) {
        let opening = match self.position {
            Position::Bound(target) => {
                format!(
                    "a borrowed pointer cannot be bound to `{}`",
                    path_text(&target.segments),
                )
            }
            Position::Consumed => return,
            Position::Escaping => "a borrowed pointer cannot be stored".to_string(),
            Position::Returned => "a borrowed pointer cannot be returned".to_string(),
        };
        self.diagnostics.push(Diagnostic::error(
            format!(
                "{opening}. It is only valid within the statement that borrows it. Pass it \
             directly to a call, or use `CPtr.copy(...)` for an owned copy",
            ),
            expr.span,
        ));
    }
}

impl<'a> Visitor<'a> for Walker<'a> {
    fn visit_function(&mut self, function: &'a Function) {
        self.check_callable(&function.params, function.body.as_deref());
    }

    fn visit_protocol_method(&mut self, method: &'a ProtocolMethod) {
        self.check_callable(&method.params, method.body.as_deref());
    }

    fn visit_statement(&mut self, statement: &'a Statement) {
        let position = match statement {
            Statement::Assignment { target, .. } => Position::Bound(target),
            Statement::Return { .. } => Position::Returned,
            _ => Position::Escaping,
        };
        self.at(position, |w| visit::walk_statement(w, statement));
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if is_cptr_borrow(expr, self.registry) {
            self.emit_escape(expr);
        }
        if let ExprKind::Closure { body, .. } = &expr.kind {
            self.check_body(body, Position::Returned);
            return;
        }
        let children = match &expr.kind {
            // Only an `assert` that failed its channel check survives
            // resolve. Its operands consume like call arguments.
            ExprKind::Assert { .. } | ExprKind::Call { .. } | ExprKind::MethodCall { .. } => {
                Position::Consumed
            }
            ExprKind::Fail { .. } | ExprKind::ShortClosure { .. } => Position::Returned,
            // Parentheses are pure grouping, so `(CPtr.borrow(b)).read()`
            // consumes the same as the unparenthesized chain.
            ExprKind::Group { .. } => self.position,
            _ => Position::Escaping,
        };
        self.at(children, |w| visit::walk_expr(w, expr));
    }
}

/// True when `expr` is a static call to the `Global.CPtr.borrow`
/// intrinsic. Resolve rewrites static receivers to a synthetic
/// `Ident` carrying the type's `Resolution::Global`, so the match is
/// exact even through aliasing or local shadowing.
fn is_cptr_borrow(expr: &Expr, registry: &GlobalRegistry) -> bool {
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &expr.kind
    else {
        return false;
    };
    if method != "borrow" {
        return false;
    }
    let ExprKind::Ident {
        resolution: Resolution::Global(receiver_id),
        ..
    } = &receiver.kind
    else {
        return false;
    };
    registry.get(*receiver_id).is_some_and(|entry| {
        entry.identifier.is_in_package("Global") && entry.identifier.path() == ["CPtr"]
    })
}
