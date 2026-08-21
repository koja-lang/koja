//! Runtime coverage for cross-package protocol impls. A codec
//! package defines a protocol, implements it for a stdlib type, and
//! the app implements it for its own struct. Both directions
//! dispatch through one `T: P` bound, so this pins that mono's
//! `[concrete_target, method]` lookup finds impl methods registered
//! under a foreign target's package.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

#[test]
fn codec_pattern_dispatches_both_directions_through_one_bound() {
    let value = common::evaluate_script_with_dep(
        "Codec",
        &dedent(
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for String
              fn to_wire(self) -> String
                self
              end
            end

            fn render<T: Encodable>(value: T) -> String
              value.to_wire()
            end
            ",
        ),
        &dedent(
            "
            struct Point
              x: Int
            end

            impl Codec.Encodable for Point
              fn to_wire(self) -> String
                \"point #{self.x}\"
              end
            end

            \"#{Codec.render(\"hi \")}#{Codec.render(Point{x: 7})}\"
            ",
        ),
    )
    .expect("interpreter should not error on this fixture");
    assert_eq!(value, Value::string("hi point 7"));
}

#[test]
fn concrete_generic_instantiation_dispatches_through_bound_and_dot_call() {
    // A dep implements its protocol for `List<Int>`, one concrete
    // instantiation of a foreign generic type. The script reaches
    // the impl method through a `T: P` bound and through bare
    // dot-call on the matching receiver.
    let value = common::evaluate_script_with_dep(
        "Codec",
        &dedent(
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for List<Int>
              fn to_wire(self) -> String
                \"#{self.length()} ints\"
              end
            end

            fn render<T: Encodable>(value: T) -> String
              value.to_wire()
            end
            ",
        ),
        "\"#{Codec.render([1, 2, 3])} / #{[4, 5].to_wire()}\"",
    )
    .expect("interpreter should not error on this fixture");
    assert_eq!(value, Value::string("3 ints / 2 ints"));
}

#[test]
fn bare_dot_call_on_foreign_conformance_runs() {
    let value = common::evaluate_script_with_dep(
        "Codec",
        &dedent(
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for String
              fn to_wire(self) -> String
                self
              end
            end
            ",
        ),
        "\"direct\".to_wire()",
    )
    .expect("interpreter should not error on this fixture");
    assert_eq!(value, Value::string("direct"));
}
