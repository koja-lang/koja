//! Tuple equality element gating. Tuples support `==` / `!=` only
//! when every element (recursively) has valid equality semantics:
//! closure and union elements reject instead of being silently
//! skipped, and the structural `Equality` bound is element-
//! conditional so `fn f<T: Equality>` cannot accept bad shapes.

use koja_ast::util::dedent;

mod common;

use common::{assert_script_fails_with, typecheck_script, typecheck_script_fail};

/// Like `assert_script_fails_with`, but needles may also match a
/// diagnostic's hint (the element-gating reason lives there).
fn assert_fails_with_hint(source: &str, needles: &[&str]) {
    let failure = typecheck_script_fail(&dedent(source));
    let rendered: Vec<String> = failure
        .diagnostics
        .iter()
        .map(|diagnostic| match &diagnostic.hint {
            Some(hint) => format!("{} (hint: {hint})", diagnostic.message),
            None => diagnostic.message.clone(),
        })
        .collect();
    for needle in needles {
        assert!(
            rendered.iter().any(|m| m.contains(needle)),
            "expected a diagnostic containing `{needle}`, got: {rendered:#?}",
        );
    }
}

const CLOSURES: &str = "
    f = fn (x: Int) -> Int
      x
    end
    g = fn (x: Int) -> Int
      x + 1
    end
    ";

#[test]
fn closure_element_diagnoses() {
    let source = format!("{CLOSURES}((f, 1) == (g, 1)).print()");
    assert_fails_with_hint(
        &source,
        &[
            "cannot compare tuples containing",
            "closures cannot be compared for equality",
        ],
    );
}

#[test]
fn closure_element_diagnoses_through_not_equals() {
    let source = format!("{CLOSURES}((f, 1) != (g, 1)).print()");
    assert_fails_with_hint(&source, &["closures cannot be compared for equality"]);
}

#[test]
fn nested_tuple_hiding_a_closure_diagnoses() {
    let source = format!("{CLOSURES}((1, (2, f)) == (1, (2, g))).print()");
    assert_fails_with_hint(&source, &["closures cannot be compared for equality"]);
}

#[test]
fn union_element_diagnoses() {
    assert_fails_with_hint(
        "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        pet: Pet = Cat{name: \"Whiskers\"}
        ((pet, 1) == (pet, 1)).print()
        ",
        &[
            "cannot compare tuples containing",
            "union values cannot be compared for equality",
        ],
    );
}

#[test]
fn equality_bound_rejects_tuple_with_closure_element() {
    let source = format!(
        "
        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end
{CLOSURES}
        equal((f, 1), (g, 1))
        "
    );
    assert_script_fails_with(
        &source,
        &[
            "does not implement protocol `Equality`",
            "required by type parameter `T`",
        ],
    );
}

#[test]
fn equality_bound_rejects_nested_tuple_with_union_element() {
    assert_script_fails_with(
        "
        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        pet: Pet = Cat{name: \"Whiskers\"}
        equal((1, (2, pet)), (1, (2, pet)))
        ",
        &["does not implement protocol `Equality`"],
    );
}

#[test]
fn comparable_elements_still_compile() {
    typecheck_script(&dedent(
        "
        struct Point
          x: Int
        end

        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        ((1, \"a\") == (1, \"a\")).print()
        ((1.5, (true, \"x\")) == (1.5, (true, \"x\"))).print()
        ((Point{x: 1}, 2) == (Point{x: 1}, 2)).print()
        equal((1, 2), (1, 2)).print()
        ",
    ));
}

#[test]
fn debug_bound_still_admits_tuples_with_closure_elements() {
    // `Debug` stays unconditional: opaque elements render as "...".
    let source = format!(
        "
        fn render<T: Debug>(value: T) -> String
          value.format()
        end
{CLOSURES}
        render((f, 1)).print()
        "
    );
    typecheck_script(&dedent(&source));
}
