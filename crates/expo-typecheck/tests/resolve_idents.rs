//! Typecheck coverage for bare-identifier resolution.
//!
//! Local-binding success-path coverage lives in `tests/locals.rs`;
//! this file pins the resolver's edge cases — currently just the
//! "unknown identifier" diagnostic for an unbound name.

use expo_ast::util::dedent;

mod common;

use common::{diagnostic_messages, typecheck_file_fail as typecheck_fail};

#[test]
fn unknown_identifier_diagnoses() {
    let source = "
        fn main
          undefined
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("undefined") || m.contains("unknown")),
        "expected unknown-identifier diagnostic, got {messages:?}",
    );
}
