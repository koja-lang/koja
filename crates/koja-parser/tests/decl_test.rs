//! Coverage for `test "description" ... end` declarations.
//!
//! Pins:
//! - top-level `test` becomes `Item::Test` with its description
//! - `test` inside a struct, enum, impl, extend, or builtin body lands
//!   in that declaration's `tests`, and a protocol body rejects it
//! - an empty body parses
//! - the description must be a plain string, so a missing string or
//!   an interpolation is an error
//! - `priv test` and annotated `test` are rejected
//! - script mode treats `test` as an item starter

use koja_ast::ast::{Item, Statement};

mod common;

use common::{
    assert_hint_contains, first_builtin, first_enum, first_extend, first_impl, first_struct,
    first_test, parse_clean, parse_clean_script, parse_failing_with,
};

#[test]
fn top_level_test_keeps_its_description_and_body() {
    let test = first_test(
        r#"
        test "push then pop returns the value"
          stack = 1
          stack
        end
        "#,
    );
    assert_eq!(test.description, "push then pop returns the value");
    assert_eq!(test.body.len(), 2);
    assert!(matches!(test.body[0], Statement::Assignment { .. }));
}

#[test]
fn empty_test_body_parses() {
    let test = first_test(
        r#"
        test "nothing yet"
        end
        "#,
    );
    assert_eq!(test.description, "nothing yet");
    assert!(test.body.is_empty());
}

#[test]
fn two_tests_may_share_a_description() {
    let file = parse_clean(
        r#"
        test "same"
        end

        test "same"
        end
        "#,
    );
    let descriptions: Vec<&str> = file
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Test(t) => Some(t.description.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(descriptions, ["same", "same"]);
}

#[test]
fn struct_body_collects_tests_beside_fields_and_functions() {
    let s = first_struct(
        r#"
        struct Stack
          items: List<Int>

          fn push(self, value: Int) -> Stack
            self
          end

          test "push grows the stack"
            1
          end

          test "pop shrinks the stack"
          end
        end
        "#,
    );
    assert_eq!(s.fields.len(), 1);
    assert_eq!(s.functions.len(), 1);
    let descriptions: Vec<&str> = s.tests.iter().map(|t| t.description.as_str()).collect();
    assert_eq!(
        descriptions,
        ["push grows the stack", "pop shrinks the stack"]
    );
    assert_eq!(s.tests[0].body.len(), 1);
}

#[test]
fn enum_body_collects_tests_beside_variants_and_functions() {
    let e = first_enum(
        r#"
        enum Color
          Red
          Green

          fn primary?(self) -> Bool
            true
          end

          test "red is primary"
            1
          end
        end
        "#,
    );
    assert_eq!(e.variants.len(), 2);
    assert_eq!(e.functions.len(), 1);
    assert_eq!(e.tests.len(), 1);
    assert_eq!(e.tests[0].description, "red is primary");
}

#[test]
fn impl_body_collects_tests_beside_methods() {
    let block = first_impl(
        r#"
        impl Display for Color
          fn format(self) -> String
            "red"
          end

          test "formats red"
          end
        end
        "#,
    );
    assert_eq!(block.members.len(), 1);
    assert_eq!(block.tests.len(), 1);
    assert_eq!(block.tests[0].description, "formats red");
}

#[test]
fn extend_body_collects_tests_beside_methods() {
    let block = first_extend(
        r#"
        extend List<Int>
          fn total(self) -> Int
            0
          end

          test "total of empty is zero"
          end
        end
        "#,
    );
    assert_eq!(block.members.len(), 1);
    assert_eq!(block.tests.len(), 1);
    assert_eq!(block.tests[0].description, "total of empty is zero");
}

#[test]
fn builtin_body_collects_tests_beside_functions() {
    let b = first_builtin(
        r#"
        builtin Handle
          fn id(self) -> Int
            0
          end

          test "id is stable"
          end
        end
        "#,
    );
    assert_eq!(b.functions.len(), 1);
    assert_eq!(b.tests.len(), 1);
    assert_eq!(b.tests[0].description, "id is stable");
}

#[test]
fn protocol_body_rejects_tests() {
    let result = parse_failing_with(
        r#"
        protocol Shape
          fn area(self) -> Int

          test "not here"
          end
        end
        "#,
        &["`test` is not valid in a protocol body"],
    );
    assert_hint_contains(&result, "`impl` block of a conforming type");
}

#[test]
fn test_span_covers_keyword_through_end() {
    let test = first_test(
        r#"
        test "spanned"
          1
        end
        "#,
    );
    assert_eq!(test.span.start.line, 1);
    assert_eq!(test.span.end.line, 3);
}

#[test]
fn test_without_a_description_is_an_error() {
    let result = parse_failing_with(
        "
        test
          1
        end
        ",
        &["expected a string description after `test`"],
    );
    assert_hint_contains(&result, "test \"what this test checks\"");
}

#[test]
fn test_description_cannot_interpolate() {
    parse_failing_with(
        r#"
        test "value is #{1}"
          1
        end
        "#,
        &["a test description is a plain string and cannot interpolate"],
    );
}

#[test]
fn priv_test_is_rejected() {
    parse_failing_with(
        r#"
        priv test "hidden"
        end
        "#,
        &["`priv` must be followed by"],
    );
}

#[test]
fn annotated_test_is_rejected() {
    parse_failing_with(
        r#"
        @doc "not allowed"
        test "annotated"
        end
        "#,
        &["annotation must be followed by a declaration"],
    );
}

#[test]
fn script_mode_treats_test_as_an_item() {
    let file = parse_clean_script(
        r#"
        x = 1

        test "in a script"
          x
        end
        "#,
    );
    assert_eq!(file.items.len(), 1);
    assert!(matches!(file.items[0], Item::Test(_)));
    assert_eq!(file.body.as_ref().map(Vec::len), Some(1));
}
