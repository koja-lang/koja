//! Typecheck coverage for the locals slice: variable
//! declaration, reassignment, parameter references, compound
//! assignment (`+=`, `-=`, `*=`, `/=`), and the feature-gap
//! tuple destructuring (rebinding existing locals, declaring new
//! ones), and the diagnostics that fence off out-of-scope shapes
//! (multi-segment lvalues, type annotations on reassignment,
//! type-changing reassignment).
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
//! [`LocalId`]: koja_ast::identifier::LocalId
//! [`LValue::local_id`]: koja_ast::ast::LValue::local_id
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local

use koja_ast::ast::{CompoundOp, ExprKind, Pattern, Statement};
use koja_ast::identifier::{LocalId, Resolution};
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    assert_file_fails_with, diagnostic_messages, function_body, global_leaf,
    typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
};

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
    let decl_id = target
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
        target.local_id.expect("Assignment should stamp local_id")
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
    let assign_id = target.local_id.expect("Assignment should stamp local_id");

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

    assert_file_fails_with(source, &["reassign", "x"]);
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

    assert_file_fails_with(source, &["type annotation", "first declaration"]);
}

#[test]
fn decl_with_mismatched_annotation_diagnoses() {
    let source = "
        fn main
          x: Int = \"hello\"
          x
        end
        ";

    assert_file_fails_with(source, &["type annotation", "x"]);
}

/// Helper: ensure a function body's i-th statement is a
/// `CompoundAssign` with the given op, the target stamps a `local_id`
/// matching the prior `Assignment`'s decl, and the rhs has the
/// expected primitive type. Used by the four happy-path arithmetic
/// cases to keep them small.
fn assert_compound_op(
    checked: &CheckedProgram,
    body_index: usize,
    expected_op: CompoundOp,
    expected_primitive: &str,
) {
    let body = function_body(checked, "main");
    let Statement::Assignment { target, .. } = &body[0] else {
        panic!(
            "expected first statement to be Assignment, got {:?}",
            body[0]
        );
    };
    let decl_id = target
        .local_id
        .expect("decl should stamp local_id on LValue");

    let Statement::CompoundAssign {
        target: compound_target,
        op,
        value,
        ..
    } = &body[body_index]
    else {
        panic!(
            "expected statement {body_index} to be CompoundAssign, got {:?}",
            body[body_index],
        );
    };
    assert_eq!(*op, expected_op, "wrong compound op");
    assert_eq!(
        compound_target.local_id,
        Some(decl_id),
        "compound-assign target should reference the existing local",
    );
    assert_eq!(
        value.resolution,
        global_leaf(checked, expected_primitive),
        "rhs should resolve to `{expected_primitive}`",
    );
}

#[test]
fn compound_assign_add_int_resolves() {
    let source = "
        fn main
          x = 1
          x += 2
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    assert_compound_op(&checked, 1, CompoundOp::Add, "Int");
}

#[test]
fn compound_assign_sub_int_resolves() {
    let source = "
        fn main
          x = 5
          x -= 2
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    assert_compound_op(&checked, 1, CompoundOp::Sub, "Int");
}

#[test]
fn compound_assign_mul_int_resolves() {
    let source = "
        fn main
          x = 3
          x *= 4
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    assert_compound_op(&checked, 1, CompoundOp::Mul, "Int");
}

#[test]
fn compound_assign_div_int_resolves() {
    let source = "
        fn main
          x = 8
          x /= 2
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    assert_compound_op(&checked, 1, CompoundOp::Div, "Int");
}

#[test]
fn compound_assign_float_resolves() {
    let source = "
        fn main
          x = 1.0
          x += 2.5
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    assert_compound_op(&checked, 1, CompoundOp::Add, "Float");
}

#[test]
fn compound_assign_undeclared_diagnoses() {
    let source = "
        fn main
          x += 1
          0
        end
        ";

    assert_file_fails_with(source, &["undeclared variable", "`x`"]);
}

#[test]
fn compound_assign_type_mismatch_diagnoses() {
    let source = "
        fn main
          x = 1
          x += 1.0
          x
        end
        ";

    assert_file_fails_with(source, &["type mismatch", "`x`"]);
}

#[test]
fn compound_assign_non_arith_lhs_diagnoses() {
    let source = "
        fn main
          b = true
          b += true
          b
        end
        ";

    assert_file_fails_with(source, &["Int", "Float", "`b`"]);
}

#[test]
fn compound_assign_on_field_target_typechecks() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          p = Point{x: 1, y: 2}
          p.x += 5
          p
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn field_assignment_typechecks_on_struct_field() {
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

    typecheck(&dedent(source));
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

/// The `LocalId` stamped on a single-segment `Assignment`.
fn assignment_local_id(stmt: &Statement) -> LocalId {
    let Statement::Assignment { target, .. } = stmt else {
        panic!("expected Assignment, got {stmt:?}");
    };
    target.local_id.expect("Assignment should stamp local_id")
}

/// The `LocalId`s stamped on each element of a flat
/// `Destructure` tuple pattern, in order.
fn destructure_local_ids(stmt: &Statement) -> Vec<LocalId> {
    let Statement::Destructure { pattern, .. } = stmt else {
        panic!("expected Destructure, got {stmt:?}");
    };
    let Pattern::Tuple { elements, .. } = pattern else {
        panic!("expected tuple pattern, got {pattern:?}");
    };
    elements
        .iter()
        .map(|element| {
            let Pattern::Binding { local_id, .. } = element else {
                panic!("expected binding element, got {element:?}");
            };
            local_id.expect("destructure binding should stamp local_id")
        })
        .collect()
}

#[test]
fn destructure_in_branch_rebinds_outer_locals() {
    let source = "
        fn main
          n = 0
          ok = false
          if true
            (n, ok) = (1, true)
          end
          n
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "main");
    let n_id = assignment_local_id(&body[0]);
    let ok_id = assignment_local_id(&body[1]);

    let Statement::Expr(branch) = &body[2] else {
        panic!("expected `if` statement, got {:?}", body[2]);
    };
    let ExprKind::If { then_body, .. } = &branch.kind else {
        panic!("expected If, got {:?}", branch.kind);
    };
    assert_eq!(
        destructure_local_ids(&then_body[0]),
        vec![n_id, ok_id],
        "destructure inside a branch must rebind the enclosing locals",
    );

    let Statement::Expr(trailing) = &body[3] else {
        panic!("expected trailing expr");
    };
    let ExprKind::Ident { resolution, .. } = &trailing.kind else {
        panic!("expected trailing Ident");
    };
    assert_eq!(*resolution, Resolution::Local(n_id));
}

#[test]
fn destructure_mixed_pattern_rebinds_and_declares() {
    let source = "
        fn main
          n = 0
          (n, fresh) = (1, \"x\")
          fresh
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "main");
    let n_id = assignment_local_id(&body[0]);
    let ids = destructure_local_ids(&body[1]);
    assert_eq!(ids[0], n_id, "existing name must rebind");
    assert_ne!(ids[1], n_id, "new name must mint its own LocalId");

    let Statement::Expr(trailing) = &body[2] else {
        panic!("expected trailing expr");
    };
    let ExprKind::Ident { resolution, .. } = &trailing.kind else {
        panic!("expected trailing Ident");
    };
    assert_eq!(*resolution, Resolution::Local(ids[1]));
    assert_eq!(trailing.resolution, global_leaf(&checked, "String"));
}

#[test]
fn destructure_rebinding_with_different_type_diagnoses() {
    let source = "
        fn main
          n = 0
          (n, ok) = (\"oops\", true)
          n
        end
        ";

    assert_file_fails_with(source, &["reassign", "n"]);
}

#[test]
fn destructure_onto_package_constant_diagnoses() {
    let source = "
        const LIMIT: Int = 10

        fn main
          (LIMIT, ok) = (1, true)
          ok
        end
        ";

    assert_file_fails_with(source, &["LIMIT", "constant"]);
}
