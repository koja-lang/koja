//! Rename one type parameter throughout a function.

use koja_ast::ast::TypeExpr;
use koja_ast::visit_mut::{self, VisitorMut};

/// Replaces every bare reference to `from` with `to`. A bare
/// reference is a single-segment [`TypeExpr::Named`] path, so
/// nested uses such as `(M, Option<ReplyTo<R>>)` rewrite all the way
/// down through the default walk.
pub(super) struct RenameTypeParam<'a> {
    pub(super) from: &'a str,
    pub(super) to: &'a TypeExpr,
}

impl VisitorMut for RenameTypeParam<'_> {
    fn visit_type_expr_mut(&mut self, type_expr: &mut TypeExpr) {
        match type_expr {
            TypeExpr::Named { path, .. } if path.len() == 1 && path[0] == self.from => {
                *type_expr = self.to.clone();
            }
            _ => visit_mut::walk_type_expr_mut(self, type_expr),
        }
    }
}
