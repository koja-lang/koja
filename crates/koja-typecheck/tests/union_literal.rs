//! Coverage for a literal in a union position.
//!
//! - **Member choice**: the literal resolves to one member of the
//!   union. The member that is the literal's default type wins
//!   (`String`, `Bool`). Without one, the single member that conforms
//!   to the literal's protocol is the answer and the literal rewrites
//!   to that member's `from_*` call.
//! - **Rejects**: two conforming members is an ambiguity error, and a
//!   union with no fitting member reports the normal mismatch.
//! - **Entries**: a list or map entry flows into the hinted axis type
//!   the way an argument flows into a parameter, so a member widens
//!   into a union axis and a numeric literal narrows to a sized axis.

use koja_ast::ast::{ExprKind, Statement};
use koja_ast::util::dedent;

mod common;

use common::{
    assert_script_fails_with, script_body, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

/// A union shaped like a JSON value, with one member for each
/// literal protocol that has no default type in the union.
const VALUE: &str = "
    struct Null
    end

    struct Number: FloatLiteral, IntLiteral
      text: String

      fn from_float(value: Float) -> Number
        Number{text: \"#{value}\"}
      end

      fn from_int(value: Int) -> Number
        Number{text: \"#{value}\"}
      end
    end

    struct Array: ListLiteral<Value>
      items: List<Value>

      fn from_list(items: List<Value>) -> Array
        Array{items: items}
      end
    end

    struct Object: MapLiteral<String, Value>
      entries: List<(String, Value)>

      fn from_entries(entries: List<(String, Value)>) -> Object
        Object{entries: entries}
      end
    end

    type Value = Null | Bool | Number | String | Array | Object
    ";

/// The source with [`VALUE`] ahead of it, both flush left.
fn with_value(source: &str) -> String {
    format!("{}\n{}", dedent(VALUE), dedent(source))
}

/// One label per `name: Value = <literal>` statement: the `from_*`
/// function of a rewritten literal, or `literal` for one that kept
/// its default type.
fn literal_shapes(source: &str) -> Vec<String> {
    let checked = typecheck(&with_value(source));
    script_body(&checked)
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assignment { value, .. } => Some(match &value.kind {
                ExprKind::MethodCall { method, .. } => method.as_str().to_string(),
                _ => "literal".to_string(),
            }),
            _ => None,
        })
        .collect()
}

#[test]
fn default_type_member_keeps_the_literal() {
    let source = "
        text: Value = \"count #{42}\"
        flag: Value = true
        ";
    assert_eq!(literal_shapes(source), vec!["literal", "literal"]);
}

#[test]
fn single_conforming_member_receives_the_literal() {
    let source = "
        int: Value = -42
        float: Value = 1.5
        list: Value = [1, \"two\"]
        map: Value = [\"k\": 1]
        ";
    assert_eq!(
        literal_shapes(source),
        vec!["from_int", "from_float", "from_list", "from_entries"]
    );
}

#[test]
fn nested_literals_resolve_member_by_member() {
    let source = "
        null: Null = Null{}
        id: Value = 7
        body: Value = [
          \"id\": id,
          \"none\": null,
          \"tags\": [\"x\", 2, true, []],
          \"nested\": [\"a\": [:], \"b\": [\"c\": 1.5]],
        ]
        body
        ";
    typecheck(&with_value(source));
}

#[test]
fn literal_argument_resolves_against_a_union_parameter() {
    let source = "
        fn take(value: Value) -> Value
          value
        end

        take(3)
        take(\"text\")
        take([1, 2])
        take([\"k\": false])
        ";
    typecheck(&with_value(source));
}

#[test]
fn two_conforming_members_are_ambiguous() {
    let source = "
        struct Meters: IntLiteral
          value: Int

          fn from_int(value: Int) -> Meters
            Meters{value: value}
          end
        end

        struct Feet: IntLiteral
          value: Int

          fn from_int(value: Int) -> Feet
            Feet{value: value}
          end
        end

        length: Meters | Feet = 3
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|d| {
            d.message.contains(
                "integer literal is ambiguous for `TestApp.Feet | TestApp.Meters`. \
                 More than one member implements `IntLiteral`",
            ) && d.hint.as_deref().is_some_and(|h| {
                h.contains("`TestApp.Feet`, `TestApp.Meters`") && h.contains("`from_int`")
            })
        }),
        "expected the ambiguity diagnostic with its hint, got {:?}",
        failure.diagnostics,
    );
    assert_eq!(
        failure.diagnostics.len(),
        1,
        "expected the ambiguity to report one time, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn default_type_member_wins_over_a_conforming_member() {
    let source = "
        struct Meters: IntLiteral
          value: Int

          fn from_int(value: Int) -> Meters
            Meters{value: value}
          end
        end

        length: Meters | Int = 3
        length
        ";
    let checked = typecheck(&dedent(source));
    let kinds: Vec<bool> = script_body(&checked)
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assignment { value, .. } => {
                Some(matches!(value.kind, ExprKind::MethodCall { .. }))
            }
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![false], "expected `3` to stay an `Int` literal");
}

#[test]
fn union_without_a_fitting_member_reports_the_mismatch() {
    let source = "
        struct Null
        end

        value: Null | String = 3
        ";
    assert_script_fails_with(
        source,
        &[
            "type annotation on `value` says `String | TestApp.Null`, but the right-hand \
             side has type `Int`",
        ],
    );
}

#[test]
fn entries_widen_into_a_union_axis() {
    let source = "
        list: List<Int | String> = [1, \"a\"]
        map: Map<String, Int | String> = [\"a\": 1, \"b\": \"two\"]
        list
        ";
    typecheck(&dedent(source));
}

#[test]
fn numeric_entries_follow_the_argument_rules() {
    let source = "
        small: Int8 = 5
        narrowed: List<Int8> = [1, 2]
        widened: List<Int> = [small, 2]
        widened
        ";
    typecheck(&dedent(source));
}

#[test]
fn entry_outside_the_axis_type_diagnoses() {
    assert_script_fails_with(
        "list: List<Int | String> = [1.5]",
        &["list literal element type mismatch. Expected `Int | String`, found `Float`"],
    );
    assert_script_fails_with(
        "list: List<Int8> = [300]",
        &["list literal element expects `Int8`, but value `300` does not fit in `Int8`"],
    );
}
