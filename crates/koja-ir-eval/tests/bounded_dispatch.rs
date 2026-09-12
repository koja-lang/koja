//! Runtime coverage for bounded dispatch through a generic function
//! with a protocol-bound type-param. Monomorphization substitutes
//! `T` at the call site, so the interpreter dispatches the `impl P
//! for T` method by mangled symbol like any inherent method.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

fn evaluate_program(source: &str) -> Value {
    common::evaluate_program(source).expect("interpreter should not error on this fixture")
}

#[test]
fn bounded_dispatch_returns_concrete_impl_method_value() {
    let value = evaluate_program(&dedent(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        fn main -> String
          show(Point{x: 1})
        end
        ",
    ));
    assert_eq!(value, Value::string("Point"));
}

#[test]
fn bounded_dispatch_distinct_concrete_args_dispatch_to_distinct_impls() {
    let value = evaluate_program(&dedent(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        struct Tag
          label: String
        end

        impl Greeter for Tag
          fn greet(self) -> String
            self.label
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        fn main -> String
          a = show(Point{x: 1})
          b = show(Tag{label: \"hi\"})
          a
        end
        ",
    ));
    assert_eq!(value, Value::string("Point"));
}

#[test]
fn bounded_dispatch_through_generic_struct_receiver_runs_to_completion() {
    // Receiver type itself is generic: `Bag<Int>` implements `Greeter`
    // via `impl Greeter for Bag<T>`. Mono'ing `show<Bag<Int>>` and
    // the inline `Bag<Int>.greet` together must produce a coherent
    // `Bag_$Int64$.greet` callee for the substituted body.
    let value = evaluate_program(&dedent(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Bag<T>
          item: T
        end

        impl Greeter for Bag<T>
          fn greet(self) -> String
            \"Bag\"
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        fn main -> String
          show(Bag{item: 1})
        end
        ",
    ));
    assert_eq!(value, Value::string("Bag"));
}
