//! End-to-end pinning for parser error messages.
//!
//! These tests are intentionally substring-based (via `contains`)
//! rather than exact-equality so the wording of any single message
//! can be tightened without invalidating dozens of unrelated tests.
//! If a message moves materially, update the substring here so the
//! test still pins the *spirit* of the diagnostic.

use expo_ast::util::dedent;

mod common;

use common::{assert_hint_contains, assert_message_contains, parse_failing};

#[test]
fn unterminated_struct_emits_error() {
    let src = dedent(
        "
        struct Open
          x: Int
        ",
    );
    let result = parse_failing(&src);
    assert!(!result.errors.is_empty());
}

#[test]
fn else_if_pins_user_facing_message() {
    let src = dedent(
        "
        fn run
          if x
            1
          else if y
            2
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "else if is not supported");
    assert_hint_contains(&result, "cond");
}

#[test]
fn tuple_expression_diagnostic_is_actionable() {
    let src = dedent(
        "
        fn run
          (1, 2)
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "tuples are not supported");
    assert_message_contains(&result, "struct");
}

#[test]
fn cond_without_else_message() {
    let src = dedent(
        "
        fn run
          cond
            x > 0 -> 1
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "cond requires an `else ->` arm");
}

#[test]
fn alias_path_must_end_with_type_ident() {
    let src = dedent(
        "
        alias Net.tcp
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "alias path must end with a type name (PascalCase)");
}

#[test]
fn annotation_without_declaration_diagnostic() {
    let src = dedent(
        "
        @doc \"oops\"
        \"not a decl\"
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "annotation must be followed by a declaration");
}

#[test]
fn annotation_in_protocol_must_precede_fn() {
    let src = dedent(
        "
        protocol P
          @doc \"oops\"
          struct Bad
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(
        &result,
        "annotation in protocol must be followed by a function signature",
    );
}

#[test]
fn impl_body_rejects_random_decl() {
    let src = dedent(
        "
        impl Foo
          struct Nested
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "expected function or type alias in impl block");
}

#[test]
fn invalid_assignment_target_emits_hint() {
    let src = dedent(
        "
        fn run
          1 + 2 = 5
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "invalid assignment target");
    assert_hint_contains(&result, "variables and fields");
}

#[test]
fn diagnostics_have_well_formed_spans() {
    let src = "fn x\n  (1, 2)\nend\n";
    let result = parse_failing(src);
    for diag in &result.errors {
        assert!(
            diag.span.end.offset >= diag.span.start.offset,
            "span has end before start: {:?}",
            diag.span,
        );
    }
}

#[test]
fn top_level_bare_expression_in_file_mode_is_an_error() {
    let src = dedent(
        "
        42 + 17
        ",
    );
    let result = parse_failing(&src);
    assert!(!result.errors.is_empty());
}

#[test]
fn diagnostic_messages_are_non_empty() {
    // Sanity: no diagnostic ever ships with an empty message.
    let src = "fn x\n  (1, 2)\nend\n@@@";
    let result = parse_failing(src);
    for diag in &result.errors {
        assert!(!diag.message.is_empty());
    }
}
