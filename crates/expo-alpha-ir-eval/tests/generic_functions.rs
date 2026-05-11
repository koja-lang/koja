//! End-to-end runtime coverage for the generics slice's function
//! arm. Every fixture here drives `parse → check → lower → run` and
//! observes the trailing [`Value`] — green tests pin that the
//! monomorphization closure pass produces functions the interpreter
//! can dispatch by mangled symbol without any generics-aware code
//! inside `expo-alpha-ir-eval`.
//!
//! The interpreter never sees a [`Resolution::TypeParam`] — it only
//! consults [`IRSymbol`]s on `Call` instructions and [`IRFunction`]s
//! in [`IRPackage::functions`]. So a green test for `id(1)` returning
//! `1` is also a contract that the IR pipeline reached eval with a
//! concrete `id_$Int64$` decl and a `Call` against the matching
//! mangled symbol.

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

fn evaluate_program(source: &str) -> Value {
    common::evaluate_program(source).expect("interpreter should not error on this fixture")
}

#[test]
fn identity_function_returns_each_concrete_call_site_value() {
    assert_eq!(
        evaluate_script(&dedent(
            "
            fn id<T>(x: T) -> T
              x
            end

            id(42)
            "
        )),
        Value::Int(42),
    );
    assert_eq!(
        evaluate_script(&dedent(
            "
            fn id<T>(x: T) -> T
              x
            end

            id(\"hello\")
            "
        )),
        Value::String("hello".into()),
    );
}

#[test]
fn generic_function_calling_another_generic_threads_through_runtime() {
    let value = evaluate_script(&dedent(
        "
        fn id<T>(x: T) -> T
          x
        end

        fn passthrough<U>(y: U) -> U
          id(y)
        end

        passthrough(7)
        ",
    ));
    assert_eq!(value, Value::Int(7));
}

#[test]
fn method_on_generic_struct_dispatches_under_mangled_symbol() {
    let value = evaluate_program(&dedent(
        "
        struct Pair<T, U>
          a: T
          b: U

          fn first(self) -> T
            self.a
          end
        end

        fn main
          p = Pair{a: 1, b: \"x\"}
          p.first()
        end
        ",
    ));
    assert_eq!(value, Value::Int(1));
}
