//! End-to-end pinning for parser error messages.
//!
//! These tests are intentionally substring-based (via `contains`)
//! rather than exact-equality so the wording of any single message
//! can be tightened without invalidating dozens of unrelated tests.
//! If a message moves materially, update the substring here so the
//! test still pins the *spirit* of the diagnostic.

use koja_ast::ast::Item;

mod common;

use common::{assert_hint_contains, error_messages, parse_failing, parse_failing_with};

#[test]
fn unterminated_struct_emits_error() {
    parse_failing(
        "
        struct Open
          x: Int
        ",
    );
}

#[test]
fn priv_before_impl_is_rejected() {
    let result = parse_failing_with(
        "
        struct Point
          x: Int
        end

        priv impl Debug for Point
          fn format(self) -> String
            \"p\"
          end
        end
        ",
        &["`priv` must be followed by"],
    );
    // Recovery still works. The impl block itself parses on the next pass.
    assert!(
        result
            .ast
            .items
            .iter()
            .any(|item| matches!(item, Item::Impl(_))),
        "expected the impl block to survive recovery",
    );
}

#[test]
fn priv_before_non_declaration_is_rejected() {
    parse_failing_with(
        "
        priv 42
        ",
        &["`priv` must be followed by"],
    );
}

#[test]
fn else_if_pins_user_facing_message() {
    let result = parse_failing_with(
        "
        fn run
          if x
            1
          else if y
            2
          end
        end
        ",
        &["else if is not supported"],
    );
    assert_hint_contains(&result, "cond");
}

#[test]
fn tuple_expression_diagnostic_is_actionable() {
    parse_failing_with(
        "
        fn run
          (1, 2)
        end
        ",
        &["tuples are not supported", "struct"],
    );
}

#[test]
fn cond_without_else_message() {
    parse_failing_with(
        "
        fn run
          cond
            x > 0 -> 1
          end
        end
        ",
        &["cond requires an `else ->` arm"],
    );
}

#[test]
fn alias_path_must_end_with_type_ident() {
    parse_failing_with(
        "
        alias Net.tcp
        ",
        &["alias path must end with a type name (PascalCase)"],
    );
}

#[test]
fn annotation_without_declaration_diagnostic() {
    parse_failing_with(
        "
        @doc \"oops\"
        \"not a decl\"
        ",
        &["annotation must be followed by a declaration"],
    );
}

#[test]
fn annotation_in_protocol_must_precede_fn() {
    parse_failing_with(
        "
        protocol P
          @doc \"oops\"
          struct Bad
          end
        end
        ",
        &["annotation in protocol must be followed by a function signature"],
    );
}

#[test]
fn impl_body_rejects_random_decl() {
    parse_failing_with(
        "
        extend Foo
          struct Nested
          end
        end
        ",
        &["expected function or type alias in block body"],
    );
}

#[test]
fn invalid_assignment_target_emits_hint() {
    let result = parse_failing_with(
        "
        fn run
          1 + 2 = 5
        end
        ",
        &["invalid assignment target"],
    );
    assert_hint_contains(&result, "variables and fields");
}

#[test]
fn diagnostics_have_well_formed_spans() {
    let result = parse_failing("fn x\n  (1, 2)\nend\n");
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
    parse_failing(
        "
        42 + 17
        ",
    );
}

#[test]
fn diagnostic_messages_are_non_empty() {
    // Sanity: no diagnostic ever ships with an empty message.
    let result = parse_failing("fn x\n  (1, 2)\nend\n@@@");
    for diag in &result.errors {
        assert!(!diag.message.is_empty());
    }
}

#[test]
fn unclosed_paren_renders_both_tokens_readably() {
    parse_failing_with("fn foo(", &["expected `)`, found end of file"]);
}

#[test]
fn missing_end_renders_keyword_and_keeps_hint() {
    let result = parse_failing_with(
        "
        fn foo
          1
        ",
        &["keyword `end`"],
    );
    assert_hint_contains(&result, "must be closed with 'end'");
}

#[test]
fn lowercase_struct_name_includes_found_identifier() {
    parse_failing_with(
        "
        struct point
        end
        ",
        &["expected type identifier, found identifier `point`"],
    );
}

#[test]
fn keyword_as_function_name_renders_keyword() {
    parse_failing_with(
        "
        fn match(x: Int)
        end
        ",
        &["expected identifier, found keyword `match`"],
    );
}

#[test]
fn unclosed_generic_renders_expected_gt() {
    parse_failing_with(
        "
        fn f(x: List<Int)
        end
        ",
        &["expected `>`, found `)`"],
    );
}

#[test]
fn stray_token_at_top_level_renders_lexeme() {
    parse_failing_with("}\n", &["unexpected token at top level: `}`"]);
}

#[test]
fn diagnostics_never_leak_debug_token_names() {
    // Guard against a `{:?}` regression: none of the internal enum
    // variant names may appear in any message across these fixtures.
    let sources = [
        "fn foo(",
        "fn foo\n  1\n",
        "struct point\nend\n",
        "fn match(x: Int)\nend\n",
        "fn f(x: List<Int)\nend\n",
        "}\n",
        "const = 1\n",
        "fn run\n  x = match\nend\n",
    ];
    let debug_names = [
        "EndOfFile",
        "Newline",
        "RParen",
        "LParen",
        "RBrace",
        "LBrace",
        "TypeIdent",
        "Ident(",
        "IntLit",
    ];
    for src in sources {
        let result = parse_failing(src);
        for message in error_messages(&result) {
            for name in debug_names {
                assert!(
                    !message.contains(name),
                    "debug token name `{name}` leaked into message: {message}",
                );
            }
        }
    }
}
