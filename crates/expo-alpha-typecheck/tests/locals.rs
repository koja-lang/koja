//! Typecheck coverage for the alpha locals slice: variable
//! declaration, reassignment, parameter references, and the
//! feature-gap diagnostics that fence off out-of-scope shapes
//! (compound assignment, multi-segment lvalues, pattern
//! destructuring, type annotations on reassignment, type-changing
//! reassignment).
//!
//! Per-function locals are addressed by [`LocalId`], so on success
//! the resolver stamps two AST nodes:
//!
//! - The decl/reassign target's [`LValue::local_id`] (so IR lower
//!   can walk straight to it without re-running scope lookup).
//! - Bare-identifier reads via [`Resolution::Local`] on the
//!   referencing [`Expr`].
//!
//! These tests pin both stamps end-to-end alongside the
//! diagnostics.
//!
//! [`LocalId`]: expo_ast::identifier::LocalId
//! [`LValue::local_id`]: expo_ast::ast::LValue::local_id
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{AssignTarget, ExprKind, Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn function_body<'a>(checked: &'a CheckedProgram, fn_name: &str) -> &'a [Statement] {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && function.name == fn_name
            {
                return function.body.as_deref().expect("function has no body");
            }
        }
    }
    panic!("fn `{fn_name}` not found in checked program");
}

#[test]
fn local_decl_stamps_lvalue_and_uses_inferred_type() {
    let source = "
        fn main
          x = 42
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "main");

    let Statement::Assignment { target, value, .. } = &body[0] else {
        panic!(
            "expected first statement to be Assignment, got {:?}",
            body[0]
        );
    };
    let AssignTarget::LValue(lvalue) = target else {
        panic!("expected LValue target, got {target:?}");
    };
    let decl_id = lvalue
        .local_id
        .expect("decl should stamp local_id on LValue");
    assert_eq!(value.resolution, global_leaf(&checked, "Int"));

    let Statement::Expr(trailing) = &body[1] else {
        panic!("expected trailing Statement::Expr, got {:?}", body[1]);
    };
    let ExprKind::Ident { resolution, .. } = &trailing.kind else {
        panic!("expected trailing Ident, got {:?}", trailing.kind);
    };
    assert_eq!(*resolution, Resolution::Local(decl_id));
    assert_eq!(trailing.resolution, global_leaf(&checked, "Int"));
}

#[test]
fn local_decl_with_matching_annotation_succeeds() {
    let source = "
        fn main
          x: Int = 42
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "main");
    let Statement::Expr(trailing) = body.last().unwrap() else {
        panic!("expected trailing expr");
    };
    assert_eq!(trailing.resolution, global_leaf(&checked, "Int"));
}

#[test]
fn local_reassignment_keeps_same_local_id_and_type() {
    let source = "
        fn main
          x = 1
          x = 2
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "main");

    let stamped_id = |stmt: &Statement| -> _ {
        let Statement::Assignment { target, .. } = stmt else {
            panic!("expected Assignment, got {stmt:?}");
        };
        let AssignTarget::LValue(lvalue) = target else {
            panic!("expected LValue target, got {target:?}");
        };
        lvalue.local_id.expect("Assignment should stamp local_id")
    };
    assert_eq!(
        stamped_id(&body[0]),
        stamped_id(&body[1]),
        "reassignment must reuse the original LocalId",
    );

    let Statement::Expr(trailing) = &body[2] else {
        panic!("expected trailing expr");
    };
    let ExprKind::Ident { resolution, .. } = &trailing.kind else {
        panic!("expected trailing Ident");
    };
    assert_eq!(*resolution, Resolution::Local(stamped_id(&body[0])));
}

#[test]
fn param_reference_resolves_to_local() {
    let source = "
        fn id(n: Int) -> Int
          n
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "id");
    let Statement::Expr(trailing) = body.last().unwrap() else {
        panic!("expected trailing Expr");
    };
    let ExprKind::Ident { resolution, .. } = &trailing.kind else {
        panic!("expected trailing Ident, got {:?}", trailing.kind);
    };
    assert!(
        matches!(resolution, Resolution::Local(_)),
        "param reference should stamp Resolution::Local, got {resolution:?}",
    );
    assert_eq!(trailing.resolution, global_leaf(&checked, "Int"));
}

#[test]
fn param_reassignment_keeps_param_local_id() {
    let source = "
        fn id(n: Int) -> Int
          n = 5
          n
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "id");

    let Statement::Assignment { target, .. } = &body[0] else {
        panic!("expected first stmt to be Assignment");
    };
    let AssignTarget::LValue(lvalue) = target else {
        panic!("expected LValue");
    };
    let assign_id = lvalue.local_id.expect("Assignment should stamp local_id");

    let Statement::Expr(trailing) = &body[1] else {
        panic!("expected trailing Expr");
    };
    let ExprKind::Ident { resolution, .. } = &trailing.kind else {
        panic!("expected trailing Ident");
    };
    assert_eq!(
        *resolution,
        Resolution::Local(assign_id),
        "reassigning a parameter should reuse the parameter's LocalId",
    );
}

#[test]
fn reassignment_with_different_type_diagnoses() {
    let source = "
        fn main
          x = 1
          x = \"oops\"
          x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("reassign") && m.contains('x')),
        "expected reassignment type-mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn reassignment_with_annotation_diagnoses() {
    let source = "
        fn main
          x = 1
          x: Int = 2
          x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("type annotation") && m.contains("first declaration")),
        "expected annotation-on-reassignment diagnostic, got {messages:?}",
    );
}

#[test]
fn decl_with_mismatched_annotation_diagnoses() {
    let source = "
        fn main
          x: Int = \"hello\"
          x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("type annotation") && m.contains('x')),
        "expected annotation-mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn compound_assignment_diagnoses_feature_gap() {
    let source = "
        fn main
          x = 1
          x += 1
          x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("compound assignment") || m.contains("+=")),
        "expected compound-assignment feature-gap diagnostic, got {messages:?}",
    );
}

#[test]
fn field_assignment_diagnoses_feature_gap() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          p = Point{x: 1, y: 2}
          p.x = 5
          p
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("field assignment") || m.contains("p.x")),
        "expected field-assignment feature-gap diagnostic, got {messages:?}",
    );
}

#[test]
fn local_does_not_leak_across_functions() {
    let source = "
        fn first
          x = 1
        end

        fn second
          x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains('x') && (m.contains("undefined") || m.contains("unknown"))),
        "expected unknown-identifier diagnostic in `second`, got {messages:?}",
    );
}
