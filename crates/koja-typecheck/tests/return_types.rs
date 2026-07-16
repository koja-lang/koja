//! Typecheck coverage for trailing-expression-vs-declared-return
//! checking. Mirrors v1's `check_implicit_return` shape: when the
//! declared return type is non-Unit, the body's trailing statement
//! must be a `Statement::Expr` whose resolution equals the declared
//! return type. Unit-returning functions skip the check, and
//! upstream-failed expressions stay quiet to avoid pile-on
//! diagnostics.

use koja_ast::util::dedent;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

#[test]
fn matching_trailing_expr_type_succeeds() {
    let source = "
        fn answer -> Int
          42
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn unit_return_accepts_arbitrary_trailing_expr() {
    let source = "
        fn ignore_value
          42
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn mismatched_trailing_expr_type_diagnoses() {
    let source = "
        fn answer -> Int
          \"forty-two\"
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "answer"]);
}

#[test]
fn empty_body_with_non_unit_return_diagnoses() {
    let source = "
        fn answer -> Int
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "empty body"]);
}

#[test]
fn trailing_assignment_with_non_unit_return_diagnoses() {
    let source = "
        fn answer -> Int
          x = 42
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "non-expression"]);
}

#[test]
fn upstream_unresolved_trailing_expr_does_not_pile_on() {
    // The trailing expression fails to resolve (`undefined`) which
    // emits its own diagnostic. The return-type check should stay
    // quiet so the user doesn't see a redundant mismatch.
    let source = "
        fn answer -> Int
          undefined
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        !messages.iter().any(|m| m.contains("return type mismatch")),
        "return-type check should defer to the upstream diagnostic, got {messages:?}",
    );
}

#[test]
fn declared_return_matches_called_function_return_type() {
    let source = "
        fn helper -> Int
          1
        end

        fn caller -> Int
          helper()
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn declared_return_mismatches_called_function_return_type_diagnoses() {
    let source = "
        fn helper -> Int
          1
        end

        fn caller -> String
          helper()
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "caller"]);
}
