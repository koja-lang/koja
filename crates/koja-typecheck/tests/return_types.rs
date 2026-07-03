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
    diagnostic_messages, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
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

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("return type mismatch") && m.contains("answer")),
        "expected return-type-mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn empty_body_with_non_unit_return_diagnoses() {
    let source = "
        fn answer -> Int
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("return type mismatch") && m.contains("empty body")),
        "expected empty-body return-type diagnostic, got {messages:?}",
    );
}

#[test]
fn trailing_assignment_with_non_unit_return_diagnoses() {
    let source = "
        fn answer -> Int
          x = 42
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("return type mismatch") && m.contains("non-expression")),
        "expected non-expression trailing-statement diagnostic, got {messages:?}",
    );
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

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("return type mismatch") && m.contains("caller")),
        "expected return-type-mismatch on `caller`, got {messages:?}",
    );
}
