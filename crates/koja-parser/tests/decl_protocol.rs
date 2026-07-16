//! Coverage for `protocol` declarations.
//!
//! Pins:
//! - method signatures with and without bodies (required vs default)
//! - type parameters on the protocol header
//! - method-level `@annotation`
//! - the error path for an annotation in a protocol body that is
//!   not followed by a function signature

use koja_ast::ast::{TypeExpr, Visibility};

mod common;

use common::{first_protocol, parse_failing_with};

#[test]
fn priv_protocol_records_private_visibility() {
    let p = first_protocol(
        "
        priv protocol Show
          fn show(self) -> String
        end
        ",
    );
    assert_eq!(p.visibility, Visibility::Private);
    assert_eq!(p.name, "Show");
}

#[test]
fn protocol_defaults_to_public_visibility() {
    let p = first_protocol(
        "
        protocol Show
          fn show(self) -> String
        end
        ",
    );
    assert_eq!(p.visibility, Visibility::Public);
}

#[test]
fn protocol_required_method() {
    let p = first_protocol(
        "
        protocol Show
          fn show(self) -> String
        end
        ",
    );
    assert_eq!(p.methods.len(), 1);
    assert_eq!(p.methods[0].name, "show");
    assert!(p.methods[0].body.is_none());
}

#[test]
fn protocol_default_method_body() {
    let p = first_protocol(
        "
        protocol Greet
          fn hello(self) -> String
            \"hi\"
          end
        end
        ",
    );
    assert_eq!(p.methods.len(), 1);
    assert!(p.methods[0].body.is_some());
}

#[test]
fn protocol_mixed_required_and_default_methods() {
    let p = first_protocol(
        "
        protocol Show
          fn show(self) -> String
          fn debug(self) -> String
            \"debug\"
          end
        end
        ",
    );
    assert_eq!(p.methods.len(), 2);
    assert!(p.methods[0].body.is_none());
    assert!(p.methods[1].body.is_some());
}

#[test]
fn protocol_with_type_params() {
    let p = first_protocol(
        "
        protocol From<T>
          fn from(value: T) -> Self
        end
        ",
    );
    assert_eq!(p.type_params.len(), 1);
    assert_eq!(p.type_params[0].name, "T");
    let method = &p.methods[0];
    assert!(matches!(method.return_type, Some(TypeExpr::Self_ { .. })));
}

#[test]
fn protocol_method_with_annotation() {
    let p = first_protocol(
        "
        protocol Greet
          @doc \"a polite greeting\"
          fn hello(self) -> String
        end
        ",
    );
    assert_eq!(p.methods.len(), 1);
    assert_eq!(p.methods[0].annotations.len(), 1);
    assert_eq!(p.methods[0].annotations[0].name, "doc");
}

#[test]
fn annotation_not_followed_by_fn_in_protocol_fails() {
    parse_failing_with(
        "
        protocol Bad
          @doc \"oops\"
          struct Nested
          end
        end
        ",
        &["annotation in protocol must be followed by a function signature"],
    );
}
