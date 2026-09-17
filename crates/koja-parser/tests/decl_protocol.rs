//! Coverage for `protocol` declarations.
//!
//! Pins:
//! - method signatures with and without bodies (required vs default)
//! - type parameters on the protocol header
//! - method-level `@annotation`
//! - the error path for an annotation in a protocol body that is
//!   not followed by a function signature
//! - nested protocols in both spellings: qualified at the top level
//!   (`protocol Date.Format`) and lexically inside a type body

use koja_ast::ast::{Item, TypeExpr, Visibility};

mod common;

use common::{first_enum, first_protocol, first_struct, parse_failing_with};

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
    assert_eq!(*p.name(), "Show");
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
fn protocol_method_with_error_type() {
    let p = first_protocol(
        "
        protocol Store
          fn load(self, key: String) -> String ! StoreError
        end
        ",
    );
    let method = &p.methods[0];
    assert!(
        matches!(method.return_type, Some(TypeExpr::Named { ref path, .. }) if path == &["String"])
    );
    assert!(
        matches!(method.error_type, Some(TypeExpr::Named { ref path, .. }) if path == &["StoreError"])
    );
}

#[test]
fn protocol_method_with_bare_error_type() {
    let p = first_protocol(
        "
        protocol Store
          fn flush(self) ! StoreError
        end
        ",
    );
    let method = &p.methods[0];
    assert!(method.return_type.is_none());
    assert!(
        matches!(method.error_type, Some(TypeExpr::Named { ref path, .. }) if path == &["StoreError"])
    );
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

#[test]
fn qualified_protocol_records_full_path() {
    let p = first_protocol(
        "
        protocol Date.Format
          fn format_date(self, date: Date) -> String
        end
        ",
    );
    assert_eq!(p.path, vec!["Date", "Format"]);
    assert_eq!(*p.name(), "Format");
    assert_eq!(p.owner_path().len(), 1);
    assert_eq!(p.owner_path()[0], "Date");
}

#[test]
fn struct_with_nested_protocol() {
    let s = first_struct(
        "
        struct Date
          day: Int

          protocol Format
            fn format_date(self, date: Date) -> String
          end
        end
        ",
    );
    assert_eq!(s.nested.len(), 1);
    let Item::Protocol(format) = &s.nested[0] else {
        panic!("expected a nested protocol");
    };
    assert_eq!(format.path, vec!["Format"]);
    assert_eq!(format.methods.len(), 1);
}

#[test]
fn enum_with_nested_private_protocol() {
    let e = first_enum(
        "
        enum Weekday
          Monday

          priv protocol Show
            fn show(self) -> String
          end
        end
        ",
    );
    let Item::Protocol(show) = &e.nested[0] else {
        panic!("expected a nested protocol");
    };
    assert_eq!(*show.name(), "Show");
    assert_eq!(show.visibility, Visibility::Private);
}

#[test]
fn nested_protocol_rejects_multi_segment_name() {
    parse_failing_with(
        "
        struct Owner
          protocol Foo.Bar
            fn bar(self)
          end
        end
        ",
        &["take a single name, found `Foo.Bar`"],
    );
}
