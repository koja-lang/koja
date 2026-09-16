//! A stale use of a renamed stdlib global gets a hint that names the
//! replacement, at both places a missing global surfaces.

use koja_ast::ast::Diagnostic;
use koja_ast::util::dedent;

mod common;

use common::typecheck_script_fail;

fn has_rename_hint(diagnostics: &[Diagnostic], message_needle: &str) -> bool {
    diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains(message_needle)
            && diagnostic.hint.as_deref() == Some("`DateTime` was renamed to `Timestamp`")
    })
}

#[test]
fn stale_type_name_hints_at_the_rename() {
    let failure = typecheck_script_fail(&dedent(
        r#"
        fn stamp(now: DateTime) -> Int
          0
        end
        "#,
    ));
    assert!(
        has_rename_hint(
            &failure.diagnostics,
            "does not recognize the type name `DateTime`"
        ),
        "got: {:?}",
        failure.diagnostics
    );
}

#[test]
fn stale_static_call_hints_at_the_rename() {
    let failure = typecheck_script_fail(&dedent(
        r#"
        fn stamp -> Int
          now = DateTime.now()
          0
        end
        "#,
    ));
    assert!(
        has_rename_hint(&failure.diagnostics, "unknown identifier `DateTime`"),
        "got: {:?}",
        failure.diagnostics
    );
}

#[test]
fn unrelated_misses_carry_no_hint() {
    let failure = typecheck_script_fail(&dedent(
        r#"
        fn stamp -> Int
          now = Nothing.now()
          0
        end
        "#,
    ));
    assert!(failure.diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("unknown identifier `Nothing`") && diagnostic.hint.is_none()
    }));
}
