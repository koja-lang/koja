//! Typecheck pins for the `! E` error channel: `-> T ! E` lifting,
//! `Result.Ok` auto-wrapping, and the `try` / `fail` / `rescue`
//! desugars.
//!
//! - **Lift**: `-> T ! E` lifts to `Result<T, E>` with
//!   `declared_fallible` stamped, and `! A | B` folds the union in.
//!   The explicit `-> Result<T, E>` spelling stays non-fallible.
//! - **Ok-wrapping**: trailing expressions and `return` values in a
//!   `!`-spelled function check against the unwrapped success type
//!   and wrap in `Result.Ok`. A `-> () ! E` body gets
//!   `Result.Ok(())` appended when it can fall off the end.
//! - **Desugars**: `try` and `rescue` rewrite to a `match` over the
//!   subject, `fail` rewrites to `return Result.Err(...)`, and
//!   error payloads widen into a declared union at the boundary.
//!   `try` inside a closure targets the closure's own return type.
//! - **Diagnostics**: each misuse surfaces its teacher hint (no
//!   channel, non-`Result` subject, `Option` subject, an error type
//!   that doesn't widen, embedded `fail`, hand-wrapped `Result`).

use koja_ast::ast::{EnumConstructionData, Expr, ExprKind, Literal, Statement};
use koja_ast::util::dedent;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, function_body, function_signature, global_named, int_type,
    package_leaf, trailing_resolution, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail, unit_type,
};

/// Typecheck `source` (dedented) expecting failure, and assert one
/// diagnostic carries both the message and the hint needle. The
/// shared `assert_script_fails_with` only scans messages, and the
/// teacher guidance under test lives in the hint.
fn assert_fails_with_hint(source: &str, message_needle: &str, hint_needle: &str) {
    let failure = typecheck_fail(&dedent(source));
    let found = failure.diagnostics.iter().any(|d| {
        d.message.contains(message_needle)
            && d.hint.as_deref().is_some_and(|h| h.contains(hint_needle))
    });
    assert!(
        found,
        "expected a diagnostic with message containing `{message_needle}` and hint \
         containing `{hint_needle}`, got: {:#?}",
        failure.diagnostics,
    );
}

/// The error enum + `!`-spelled producer shared by most tests.
const PRELUDE: &str = "
    enum MyError
      Nope
    end

    fn parse(s: String) -> Int ! MyError
      if s == \"1\"
        1
      else
        fail MyError.Nope
      end
    end
";

fn with_prelude(body: &str) -> String {
    format!("{}\n{}", dedent(PRELUDE), dedent(body))
}

fn assert_ok_construction(expr: &Expr) {
    assert!(
        matches!(
            &expr.kind,
            ExprKind::EnumConstruction { variant, .. } if variant == "Ok"
        ),
        "expected a `Result.Ok` construction, got {:?}",
        expr.kind,
    );
}

// Lift

#[test]
fn fallible_signature_lifts_to_result() {
    let checked = typecheck(&with_prelude("  parse(\"1\")"));
    let signature = function_signature(&checked, PACKAGE, &["parse"]);
    assert!(signature.declared_fallible);
    let expected = global_named(
        &checked,
        "Result",
        vec![int_type(&checked), package_leaf(&checked, "MyError")],
    );
    assert_eq!(signature.return_type, expected);
    // Callers see an ordinary `Result` value.
    assert_eq!(trailing_resolution(&checked), expected);
}

#[test]
fn explicit_result_spelling_is_not_fallible() {
    let source = "
        fn wrapped() -> Result<Int, String>
          Result.Ok(1)
        end

          wrapped()
        ";
    let checked = typecheck(&dedent(source));
    let signature = function_signature(&checked, PACKAGE, &["wrapped"]);
    assert!(!signature.declared_fallible);
}

#[test]
fn fallible_union_error_lifts_into_result() {
    let source = "
        enum ParseError
          Bad
        end

        enum NetError
          Timeout
        end

        fn combined() -> Int ! ParseError | NetError
          1
        end

          combined()
        ";
    let checked = typecheck(&dedent(source));
    let signature = function_signature(&checked, PACKAGE, &["combined"]);
    assert!(signature.declared_fallible);
}

// Ok-wrapping

#[test]
fn trailing_expr_wraps_in_ok() {
    let checked = typecheck(&with_prelude("  parse(\"1\")"));
    let body = function_body(&checked, "parse");
    // The trailing `if` is the return value, so the whole
    // expression wraps once (its `fail` arm already diverges).
    let Some(Statement::Expr(trailing)) = body.last() else {
        panic!("expected a trailing expression");
    };
    assert_ok_construction(trailing);
}

#[test]
fn explicit_return_wraps_in_ok() {
    let source = "
        enum MyError
          Nope
        end

        fn double(n: Int) -> Int ! MyError
          return n * 2
        end

          double(2)
        ";
    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "double");
    let Some(Statement::Return {
        value: Some(value), ..
    }) = body.last()
    else {
        panic!("expected a trailing return");
    };
    assert_ok_construction(value);
}

#[test]
fn unit_success_appends_ok_unit_on_fall_off() {
    let source = "
        enum MyError
          Nope
        end

        fn note() -> () ! MyError
          x = 1
        end

          note()
        ";
    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "note");
    assert_eq!(body.len(), 2, "expected an appended `Result.Ok(())`");
    let Some(Statement::Expr(appended)) = body.last() else {
        panic!("expected a trailing expression");
    };
    assert_ok_construction(appended);
    let ExprKind::EnumConstruction {
        data: EnumConstructionData::Tuple(payload),
        ..
    } = &appended.kind
    else {
        panic!("expected a tuple payload");
    };
    assert!(matches!(
        payload[0].kind,
        ExprKind::Literal {
            value: Literal::Unit
        }
    ));
}

#[test]
fn bare_error_signature_lifts_to_result_of_unit() {
    let source = "
        enum MyError
          Nope
        end

        fn note(flag: Bool) ! MyError
          unless flag
            fail MyError.Nope
          end
        end

          note(true)
        ";
    let checked = typecheck(&dedent(source));
    let signature = function_signature(&checked, PACKAGE, &["note"]);
    assert!(signature.declared_fallible);
    let expected = global_named(
        &checked,
        "Result",
        vec![unit_type(&checked), package_leaf(&checked, "MyError")],
    );
    assert_eq!(signature.return_type, expected);
    // The body can fall off the end, so a `Result.Ok(())` appends.
    let body = function_body(&checked, "note");
    let Some(Statement::Expr(appended)) = body.last() else {
        panic!("expected a trailing expression");
    };
    assert_ok_construction(appended);
}

#[test]
fn bare_return_in_unit_success_wraps_ok_unit() {
    let source = "
        enum MyError
          Nope
        end

        fn quit(flag: Bool) -> () ! MyError
          if flag
            return
          end
          x = 1
        end

          quit(true)
        ";
    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "quit");
    let Some(Statement::Expr(Expr {
        kind: ExprKind::If { then_body, .. },
        ..
    })) = body.first()
    else {
        panic!("expected a leading `if`");
    };
    let Some(Statement::Return {
        value: Some(value), ..
    }) = then_body.first()
    else {
        panic!("expected the bare return to gain a value");
    };
    assert_ok_construction(value);
}

// Desugars

#[test]
fn try_desugars_to_match_and_types_as_ok() {
    let source = "
        fn caller() -> Int ! MyError
          n = try parse(\"1\")
          n + 1
        end

          caller()
        ";
    let checked = typecheck(&with_prelude(source));
    let body = function_body(&checked, "caller");
    let Some(Statement::Assignment { value, .. }) = body.first() else {
        panic!("expected the try binding");
    };
    assert!(
        matches!(value.kind, ExprKind::Match { .. }),
        "expected `try` to desugar to a match, got {:?}",
        value.kind,
    );
    assert_eq!(value.resolution, int_type(&checked));
}

#[test]
fn fail_desugars_to_return_err() {
    let source = "
        fn boom() -> Int ! MyError
          fail MyError.Nope
        end

          boom()
        ";
    let checked = typecheck(&with_prelude(source));
    let body = function_body(&checked, "boom");
    let Some(Statement::Return {
        value: Some(value), ..
    }) = body.last()
    else {
        panic!("expected `fail` to desugar to a return");
    };
    assert!(
        matches!(
            &value.kind,
            ExprKind::EnumConstruction { variant, .. } if variant == "Err"
        ),
        "expected a `Result.Err` construction, got {:?}",
        value.kind,
    );
}

#[test]
fn try_and_fail_widen_errors_into_declared_union() {
    let source = "
        enum ParseError
          Bad
        end

        enum NetError
          Timeout
        end

        fn fetch(ok: Bool) -> String ! NetError
          unless ok
            fail NetError.Timeout
          end
          \"1\"
        end

        fn digit(s: String) -> Int ! ParseError
          if s == \"1\"
            1
          else
            fail ParseError.Bad
          end
        end

        fn combined(ok: Bool) -> Int ! ParseError | NetError
          body = try fetch(ok)
          try digit(body)
        end

          combined(true)
        ";
    typecheck(&dedent(source));
}

#[test]
fn rescue_types_as_subject_ok_type() {
    let checked = typecheck(&with_prelude("  parse(\"2\") rescue _ -> 0"));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn rescue_binder_carries_the_error_type() {
    let source = "
        fn describe(e: MyError) -> Int
          0
        end

          parse(\"2\") rescue e -> describe(e)
        ";
    let checked = typecheck(&with_prelude(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn rescue_handler_may_fail() {
    let source = "
        enum OtherError
          AlsoNope
        end

        fn translated() -> Int ! OtherError
          parse(\"2\") rescue _ -> fail OtherError.AlsoNope
        end

          translated()
        ";
    typecheck(&with_prelude(source));
}

#[test]
fn try_in_closure_targets_the_closure_channel() {
    // The enclosing function has no channel at all. The `try` and
    // `fail` inside the closure resolve against the closure's own
    // `Result` return type.
    let source = "
        fn run() -> Int
          double = fn (n: Int) -> Result<Int, MyError>
            if n > 10
              fail MyError.Nope
            end
            v = try parse(\"1\")
            Result.Ok(n * 2 + v)
          end
          double(3) rescue _ -> 0
        end

          run()
        ";
    typecheck(&with_prelude(source));
}

// Diagnostics

#[test]
fn try_without_channel_is_rejected() {
    let source = "
        fn caller() -> Int
          try parse(\"1\")
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "`try` needs an error channel",
        "-> T ! E",
    );
}

#[test]
fn try_on_non_result_is_rejected() {
    let source = "
        fn caller() -> Int ! MyError
          try 42
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "`try` needs a `Result` subject",
        "fallible expression",
    );
}

#[test]
fn try_on_option_hints_or_err() {
    let source = "
        fn caller(o: Option<Int>) -> Int ! MyError
          try o
        end

          caller(Option.Some(1))
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "`try` needs a `Result` subject",
        ".or_err(error)",
    );
}

#[test]
fn fail_without_channel_is_rejected() {
    let source = "
        fn caller() -> Int
          fail MyError.Nope
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "`fail` needs an error channel",
        "-> T ! E",
    );
}

#[test]
fn fail_that_does_not_widen_is_rejected() {
    let source = "
        enum OtherError
          AlsoNope
        end

        fn caller() -> Int ! MyError
          fail OtherError.AlsoNope
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "declares error type",
        "widen the declared error union",
    );
}

#[test]
fn embedded_fail_is_rejected() {
    let source = "
        fn caller(flag: Bool) -> Int ! MyError
          flag ? 1 : fail MyError.Nope
        end

          caller(true)
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "cannot be embedded in a larger expression",
        "anywhere `return` does",
    );
}

#[test]
fn rescue_on_non_result_is_rejected() {
    let source = "
        fn caller() -> Int
          42 rescue _ -> 0
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "`rescue` needs a `Result` subject",
        "fallible expression",
    );
}

#[test]
fn rescue_handler_type_mismatch_names_the_handler() {
    let source = "
        fn caller() -> String
          parse(\"2\") rescue _ -> \"zero\"
        end

          caller()
        ";
    assert_script_fails_with(
        &with_prelude(source),
        &["rescue arms have inconsistent types", "the rescue handler"],
    );
}

#[test]
fn hand_wrapped_return_gets_auto_wrap_hint() {
    let source = "
        fn caller() -> Int ! MyError
          return Result.Ok(5)
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "return type mismatch",
        "wraps success values in `Result.Ok` automatically",
    );
}

#[test]
fn hand_wrapped_trailing_gets_auto_wrap_hint() {
    let source = "
        fn caller() -> Int ! MyError
          Result.Ok(5)
        end

          caller()
        ";
    assert_fails_with_hint(
        &with_prelude(source),
        "return type mismatch",
        "wraps success values in `Result.Ok` automatically",
    );
}
