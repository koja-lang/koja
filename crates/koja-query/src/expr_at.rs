//! Expression lookups by cursor position, for completion and
//! signature help.

use koja_ast::ast::*;
use koja_ast::identifier::{GlobalRegistryId, Resolution};
use koja_typecheck::{FunctionSignature, GlobalKind, GlobalRegistry};

use crate::position::span_contains;
use crate::visit::{self, Visitor};

/// The innermost expression in `file` that contains the 1-indexed
/// cursor position.
pub fn find_expr_at(file: &File, line: u32, col: u32) -> Option<&Expr> {
    let mut finder = ExprAt {
        line,
        col,
        found: None,
    };
    finder.visit_file(file);
    finder.found
}

struct ExprAt<'ast> {
    line: u32,
    col: u32,
    found: Option<&'ast Expr>,
}

impl<'ast> Visitor<'ast> for ExprAt<'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if !span_contains(&expr.span, self.line, self.col) {
            return;
        }
        self.found = Some(expr);
        visit::walk_expr(self, expr);
    }
}

/// A call whose argument list holds the cursor.
pub struct CallSite<'a> {
    pub expr: &'a Expr,
    /// Index of the argument under the cursor, or `args.len()` when
    /// the cursor sits after the last one.
    pub active_param: usize,
}

/// The innermost `Call` or `MethodCall` in `file` that contains the
/// cursor. Desugared calls, whose method name is synthetic, are
/// skipped so `a == b` does not offer help for `eq`.
pub fn find_enclosing_call(file: &File, line: u32, col: u32) -> Option<CallSite<'_>> {
    let mut finder = CallAt {
        line,
        col,
        found: None,
    };
    finder.visit_file(file);
    let expr = finder.found?;
    let args = match &expr.kind {
        ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => args,
        _ => return None,
    };
    let active_param = args
        .iter()
        .position(|arg| span_contains(&arg.span, line, col))
        .unwrap_or(args.len());
    Some(CallSite { expr, active_param })
}

struct CallAt<'ast> {
    line: u32,
    col: u32,
    found: Option<&'ast Expr>,
}

impl<'ast> Visitor<'ast> for CallAt<'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if !span_contains(&expr.span, self.line, self.col) {
            return;
        }
        match &expr.kind {
            ExprKind::Call { .. } => self.found = Some(expr),
            ExprKind::MethodCall { method, .. } if !method.span.synthetic => {
                self.found = Some(expr);
            }
            _ => {}
        }
        visit::walk_expr(self, expr);
    }
}

/// The lifted signature of the function a call resolved to. `None`
/// when `target` is not a global function or its signature is not
/// lifted yet.
pub fn signature_for_target(
    target: Resolution,
    registry: &GlobalRegistry,
) -> Option<&FunctionSignature> {
    let Resolution::Global(id) = target else {
        return None;
    };
    match &registry.get(id)?.kind {
        GlobalKind::Function(definition) => definition.signature.as_ref(),
        _ => None,
    }
}

/// The type a method call receiver dispatches on, as a registry id.
/// Reads the receiver's type stamp, peeling aliases, and falls back
/// to the `Global` stamp typecheck leaves on a static receiver such
/// as `Point` in `Point.new()`.
pub fn receiver_type_id(receiver: &Expr, registry: &GlobalRegistry) -> Option<GlobalRegistryId> {
    if let Some(id) = registry.head_type_id(&receiver.resolution) {
        return Some(id);
    }
    if let ExprKind::Ident {
        resolution: Resolution::Global(id),
        ..
    } = &receiver.kind
    {
        return Some(*id);
    }
    None
}
