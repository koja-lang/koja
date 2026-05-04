//! Lift sub-pass for script-mode files: hoists `File.body`'s top-level
//! statements into a synthesized `fn main` item so every downstream
//! pass (`collect`, `resolve`, `seal`, `expo-alpha-ir`, `expo-alpha-ir-eval`) sees a
//! uniform shape with no script-vs-file branch.
//!
//! Runs as the first sub-pass of [`crate::check_program`]; sealed-AST
//! invariant after this pass is `file.body.is_none()`.
//!
//! This pass is intentionally narrow and self-contained — when the
//! synthetic `main` wrap is replaced by a native script-execution
//! semantic in the IR, this entire file can be deleted in one
//! commit.
//!
//! Collision behavior with an explicit user `fn main`: `lift_script`
//! always pushes the synthetic item; the existing `collect` pass
//! catches the duplicate identifier and emits a normal "already
//! defined" diagnostic. No special handling needed here.

use expo_ast::ast::{File, Function, Item, Visibility};

/// Hoist `file.body`'s statements into a synthetic `fn main` item.
/// No-op when `file.body.is_none()`.
///
/// The parser collapses an empty script body to `None` already (see
/// `Parser::parse_file`), so by contract `Some(_)` here always means
/// at least one statement. We still defensively skip an empty `body`
/// to keep the "no synthetic main without statements" rule local to
/// this pass.
///
/// After this returns, `file.body` is always `None` — that is the
/// sealed-AST invariant downstream passes rely on.
pub(crate) fn lift_script(file: &mut File) {
    let Some(body) = file.body.take() else { return };
    if body.is_empty() {
        return;
    }
    let synthetic = Function {
        annotations: Vec::new(),
        body: Some(body),
        name: "main".to_string(),
        params: Vec::new(),
        return_type: None,
        span: file.span,
        type_params: Vec::new(),
        visibility: Visibility::Public,
    };
    file.items.push(Item::Function(synthetic));
}

#[cfg(test)]
mod tests {
    use expo_ast::ast::{Expr, ExprKind, Item, Literal, Statement};
    use expo_ast::span::{Position, Span};

    use super::*;

    fn dummy_span() -> Span {
        let p = Position {
            offset: 0,
            line: 1,
            column: 1,
        };
        Span::new(p, p)
    }

    fn int_literal(value: i64) -> Expr {
        Expr::new(
            ExprKind::Literal {
                value: Literal::Int(value.to_string()),
            },
            dummy_span(),
        )
    }

    #[test]
    fn lifts_body_into_fn_main() {
        let mut file = File {
            body: Some(vec![Statement::Expr(int_literal(2))]),
            comments: Vec::new(),
            items: Vec::new(),
            package: "TestApp".to_string(),
            path: None,
            span: dummy_span(),
        };

        lift_script(&mut file);

        assert!(file.body.is_none(), "body must be cleared after lift");
        assert_eq!(file.items.len(), 1);
        let Item::Function(function) = &file.items[0] else {
            panic!("expected synthesized fn item, got {:?}", file.items[0]);
        };
        assert_eq!(function.name, "main");
        assert_eq!(function.visibility, Visibility::Public);
        assert!(function.params.is_empty());
        assert!(function.return_type.is_none());
        let body = function.body.as_ref().expect("synthetic main has a body");
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn no_body_means_no_op() {
        let mut file = File {
            body: None,
            comments: Vec::new(),
            items: Vec::new(),
            package: "TestApp".to_string(),
            path: None,
            span: dummy_span(),
        };

        lift_script(&mut file);

        assert!(file.body.is_none());
        assert!(file.items.is_empty());
    }

    #[test]
    fn empty_body_is_a_no_op() {
        let mut file = File {
            body: Some(Vec::new()),
            comments: Vec::new(),
            items: Vec::new(),
            package: "TestApp".to_string(),
            path: None,
            span: dummy_span(),
        };

        lift_script(&mut file);

        assert!(
            file.body.is_none(),
            "body must be cleared even on the no-op path so the seal invariant holds"
        );
        assert!(
            file.items.is_empty(),
            "no synthetic `fn main` should be created when there are no statements"
        );
    }
}
