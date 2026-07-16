//! Typecheck coverage for the protocol-decl side of `lift_signatures`:
//! registration shape, dispatch flavors, duplicate-decl handling, and
//! per-feature gap diagnostics for unsupported protocol-level
//! features (generics, `Self` returns, annotations).

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::{
    CheckedProgram, Dispatch, GlobalKind, ProtocolDefinition, ResolvedProtocolMethod,
};

mod common;

use common::{PACKAGE, assert_script_fails_with, typecheck_script as typecheck};

fn protocol_definition<'a>(checked: &'a CheckedProgram, name: &str) -> &'a ProtocolDefinition {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Protocol(Some(definition)) => definition,
        other => panic!("expected lifted Protocol(Some(_)) for `{ident}`, got {other:?}"),
    }
}

fn protocol_method<'a>(
    definition: &'a ProtocolDefinition,
    name: &str,
) -> &'a ResolvedProtocolMethod {
    definition
        .methods
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("protocol method `{name}` missing"))
}

#[test]
fn protocol_decl_registers_with_lifted_definition() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = protocol_definition(&checked, "Greeter");
    assert_eq!(definition.methods.len(), 1);
    let greet = protocol_method(definition, "greet");
    assert_eq!(greet.dispatch, Dispatch::Instance);
    assert!(greet.non_self_params.is_empty());
    assert!(!greet.has_default);
}

#[test]
fn protocol_static_method_lifts_with_static_dispatch() {
    let source = "
        protocol Maker
          fn make() -> Int
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = protocol_definition(&checked, "Maker");
    let make = protocol_method(definition, "make");
    assert_eq!(make.dispatch, Dispatch::Static);
}

#[test]
fn duplicate_protocol_decl_diagnoses() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        protocol Greeter
          fn greet(self) -> Int
        end
        ";

    assert_script_fails_with(source, &["already defined", "Greeter"]);
}

#[test]
fn generic_protocol_method_diagnoses_feature_gap() {
    let source = "
        protocol Greeter
          fn greet<U>(self, u: U) -> U
        end
        ";

    assert_script_fails_with(source, &["generic protocol methods"]);
}

#[test]
fn protocol_annotation_diagnoses_feature_gap() {
    let source = "
        @inline
        protocol Greeter
          fn greet(self) -> String
        end
        ";

    assert_script_fails_with(source, &["annotations on protocols"]);
}
