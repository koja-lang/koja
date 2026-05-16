//! Coverage for `protocol` declarations.
//!
//! Pins:
//! - method signatures with and without bodies (required vs default)
//! - type parameters on the protocol header
//! - method-level `@annotation`
//! - the error path for an annotation in a protocol body that is
//!   not followed by a function signature

use expo_ast::ast::{Item, TypeExpr};
use expo_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_protocol(source: &str) -> expo_ast::ast::ProtocolDecl {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Protocol(p) = item {
            return p;
        }
    }
    panic!("no protocol in parsed output");
}

#[test]
fn protocol_required_method() {
    let src = dedent(
        "
        protocol Show
          fn show(self) -> String
        end
        ",
    );
    let p = first_protocol(&src);
    assert_eq!(p.methods.len(), 1);
    assert_eq!(p.methods[0].name, "show");
    assert!(p.methods[0].body.is_none());
}

#[test]
fn protocol_default_method_body() {
    let src = dedent(
        "
        protocol Greet
          fn hello(self) -> String
            \"hi\"
          end
        end
        ",
    );
    let p = first_protocol(&src);
    assert_eq!(p.methods.len(), 1);
    assert!(p.methods[0].body.is_some());
}

#[test]
fn protocol_mixed_required_and_default_methods() {
    let src = dedent(
        "
        protocol Show
          fn show(self) -> String
          fn debug(self) -> String
            \"debug\"
          end
        end
        ",
    );
    let p = first_protocol(&src);
    assert_eq!(p.methods.len(), 2);
    assert!(p.methods[0].body.is_none());
    assert!(p.methods[1].body.is_some());
}

#[test]
fn protocol_with_type_params() {
    let src = dedent(
        "
        protocol From<T>
          fn from(value: T) -> Self
        end
        ",
    );
    let p = first_protocol(&src);
    assert_eq!(p.type_params.len(), 1);
    assert_eq!(p.type_params[0].name, "T");
    let method = &p.methods[0];
    assert!(matches!(method.return_type, Some(TypeExpr::Self_ { .. })));
}

#[test]
fn protocol_method_with_annotation() {
    let src = dedent(
        "
        protocol Greet
          @doc \"a polite greeting\"
          fn hello(self) -> String
        end
        ",
    );
    let p = first_protocol(&src);
    assert_eq!(p.methods.len(), 1);
    assert_eq!(p.methods[0].annotations.len(), 1);
    assert_eq!(p.methods[0].annotations[0].name, "doc");
}

#[test]
fn annotation_not_followed_by_fn_in_protocol_fails() {
    let src = dedent(
        "
        protocol Bad
          @doc \"oops\"
          struct Nested
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(
        &result,
        "annotation in protocol must be followed by a function signature",
    );
}
