//! Tuple equality element gating. Tuples support `==` / `!=` when
//! every element (recursively) is `Equality`. Function and union
//! elements compare like any other value, so only an instantiation
//! left out by a hand-written conditional impl rejects, and the
//! structural `Equality` bound is element-conditional so
//! `fn f<T: Equality>` follows the same rule.

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

const PETS: &str = "
    struct Cat
      name: String
    end

    struct Dog
      name: String
    end

    type Pet = Cat | Dog

    pet: Pet = Cat{name: \"Whiskers\"}
    ";

#[test]
fn closure_elements_compare() {
    let source = format!(
        "{CLOSURES}
        ((f, 1) == (g, 1)).print()
        ((f, 1) != (g, 1)).print()
        ((1, (2, f)) == (1, (2, g))).print()
        "
    );
    typecheck_script(&dedent(&source));
}

#[test]
fn union_elements_compare() {
    let source = format!("{PETS}((pet, 1) == (pet, 1)).print()");
    typecheck_script(&dedent(&source));
}

#[test]
fn equality_bound_admits_tuples_with_closure_and_union_elements() {
    let source = format!(
        "
        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end
{CLOSURES}
{PETS}
        equal((f, 1), (g, 1)).print()
        equal((1, (2, pet)), (1, (2, pet))).print()
        "
    );
    typecheck_script(&dedent(&source));
}

/// A hand-written conditional impl is the one way a type can miss
/// `Equality`: `Box<Float>` conforms, `Box<String>` does not.
const CONDITIONAL_BOX: &str = "
    struct Box<T>
      value: T
    end

    impl Equality for Box<T: Hash>
      fn equals?(self, other: Self) -> Bool
        self.value.hash() == other.value.hash()
      end
    end

    boxed = Box{value: 1.5}
    ";

#[test]
fn non_equality_element_diagnoses() {
    let source = format!("{CONDITIONAL_BOX}((boxed, 1) == (boxed, 1)).print()");
    assert_fails_with_hint(
        &source,
        &[
            "cannot compare tuples containing `Box<Float>`",
            "`Box<Float>` does not implement `Equality`",
        ],
    );
}

#[test]
fn equality_bound_rejects_tuple_with_non_equality_element() {
    let source = format!(
        "
        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end
{CONDITIONAL_BOX}
        equal((boxed, 1), (boxed, 1))
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
fn mismatched_tuple_shapes_diagnose() {
    assert_script_fails_with(
        "((1, 2) == (1, \"a\")).print()",
        &["Tuple equality requires both sides to have the same type"],
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
    // `Debug` stays unconditional: function elements render as "...".
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
