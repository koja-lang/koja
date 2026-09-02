//! Function values are `Equality`. `f == g`, `f.equals?(g)`, and the
//! `Equality` bound all admit function types. Only `equals?` resolves
//! on a function receiver: `f.format()` stays rejected, and the two
//! sides must have the same function type.

use koja_ast::util::dedent;

mod common;

use common::{assert_script_fails_with, typecheck_script};

const FUNCTIONS: &str = "
    fn double(x: Int) -> Int
      x * 2
    end

    fn succ(x: Int) -> Int
      x + 1
    end

    fn hello(name: String) -> String
      \"hi #{name}\"
    end
    ";

#[test]
fn equality_operators_on_functions_resolve_to_bool() {
    let source = format!(
        "{FUNCTIONS}
        same: Bool = &double/1 == &double/1
        different: Bool = &double/1 != &succ/1
        explicit: Bool = &double/1.equals?(&succ/1)
        adder = fn (x: Int) -> Int x + 1 end
        (adder == adder).print()
        "
    );
    typecheck_script(&dedent(&source));
}

#[test]
fn equality_bound_admits_function_types() {
    let source = format!(
        "{FUNCTIONS}
        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        fn same<T>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        equal(&double/1, &succ/1).print()
        same(&double/1, &succ/1).print()
        "
    );
    typecheck_script(&dedent(&source));
}

#[test]
fn mismatched_function_types_diagnose() {
    let source = format!("{FUNCTIONS}(&double/1 == &hello/1).print()");
    assert_script_fails_with(
        &source,
        &["Function equality requires both sides to have the same type"],
    );
}

#[test]
fn format_on_a_function_value_is_rejected() {
    let source = format!("{FUNCTIONS}(&double/1).format().print()");
    assert_script_fails_with(&source, &["no function `format` on function type"]);
}

#[test]
fn non_equality_functions_on_a_function_value_are_rejected() {
    let source = format!("{FUNCTIONS}(&double/1).hash().print()");
    assert_script_fails_with(&source, &["no function `hash` on function type"]);
}
