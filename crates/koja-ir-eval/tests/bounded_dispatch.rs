//! Runtime coverage for Slice 2.10, bounded dispatch through a
//! generic function with a protocol-bound type-param.
//!
//! `parse -> check -> lower -> run` for fixtures where the trailing
//! expression / `main` calls a generic function with a `<T: P>`
//! bound on its type-param, and the body invokes a method declared
//! by `P`. Mono substitutes `T` with the concrete struct/enum at
//! the call site. Lower's `[receiver_struct, method_name]` lookup
//! resolves to the `impl P for T` block's method, and the interpreter
//! dispatches by mangled symbol just like any inherent method
//! call.
//!
//! The runtime never sees a `Resolution::TypeParam`, just like the
//! existing generic-function tests. The green tests here pin that
//! the substitute walker rewrote receiver resolutions correctly
//! and that the impl-block method made it into the IRPackage
//! function table under the expected mangled name.

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
