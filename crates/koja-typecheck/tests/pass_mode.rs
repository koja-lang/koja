//! Coverage that `lift_signatures` propagates the surface-syntax
//! [`PassMode`] verbatim onto [`ResolvedParam::mode`] for both
//! regular params (`name: T` → `Borrow`, `move name: T` → `Move`)
//! and the `self` receiver. Downstream IR consumers
//! (`ownership_for_param`, the parameter-promotion `LocalWrite`
//! stamp) key drop decisions on this field.

use koja_ast::ast::PassMode;
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::{
    CheckedProgram, FunctionSignature, GlobalKind, ProtocolDefinition, ResolvedParam,
};

mod common;

use common::{PACKAGE, typecheck_file as typecheck};

fn lookup_function_signature<'a>(
    checked: &'a CheckedProgram,
    package: &str,
    path: &[&str],
) -> &'a FunctionSignature {
    let ident = Identifier::new(package, path.iter().map(|s| (*s).to_string()).collect());
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Function(Some(signature)) => signature,
        other => panic!("expected lifted Function(Some(_)) for `{ident}`, got {other:?}"),
    }
}

fn lookup_protocol_definition<'a>(
    checked: &'a CheckedProgram,
    name: &str,
) -> &'a ProtocolDefinition {
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

fn find_param<'a>(signature: &'a FunctionSignature, name: &str) -> &'a ResolvedParam {
    signature
        .params
        .iter()
        .find(|param| param.name == name)
        .unwrap_or_else(|| {
            panic!(
                "param `{name}` missing from signature; have: {:?}",
                signature.params.iter().map(|p| &p.name).collect::<Vec<_>>()
            )
        })
}

#[test]
fn default_borrow_param_lifts_to_passmode_borrow() {
    let source = "
        fn id(x: Int) -> Int
          x
        end

        fn main
          id(1)
        end
    ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, PACKAGE, &["id"]);
    let param = find_param(signature, "x");

    assert_eq!(
        param.mode,
        PassMode::Borrow,
        "default-borrow `x: Int` must lift to PassMode::Borrow",
    );
}

#[test]
fn move_param_lifts_to_passmode_move() {
    let source = "
        fn shout(move s: String) -> String
          s
        end

        fn main
          shout(\"hi\")
        end
    ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, PACKAGE, &["shout"]);
    let param = find_param(signature, "s");

    assert_eq!(
        param.mode,
        PassMode::Move,
        "move-mode `move s: String` must lift to PassMode::Move",
    );
}

#[test]
fn move_self_receiver_lifts_to_passmode_move() {
    let source = "
        struct Box
          fn consume(move self) -> Int
            1
          end
        end

        fn main
          1
        end
    ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, PACKAGE, &["Box", "consume"]);
    let param = find_param(signature, "self");

    assert_eq!(
        param.mode,
        PassMode::Move,
        "`move self` must lift to PassMode::Move on the receiver param",
    );
}

#[test]
fn default_self_receiver_lifts_to_passmode_borrow() {
    let source = "
        struct Box
          fn read(self) -> Int
            1
          end
        end

        fn main
          1
        end
    ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, PACKAGE, &["Box", "read"]);
    let param = find_param(signature, "self");

    assert_eq!(
        param.mode,
        PassMode::Borrow,
        "default `self` must lift to PassMode::Borrow on the receiver param",
    );
}

#[test]
fn protocol_method_param_carries_passmode_through_lift() {
    // `move` propagates onto a protocol's non-self params too — the
    // [`ResolvedProtocolMethod::non_self_params`] vec uses the same
    // [`ResolvedParam`] shape, so the same field is exercised on the
    // protocol path.
    let source = "
        protocol Sink
          fn drain(move payload: String) -> Int
        end

        fn main
          1
        end
    ";

    let checked = typecheck(&dedent(source));
    let definition = lookup_protocol_definition(&checked, "Sink");
    let method = definition
        .methods
        .iter()
        .find(|m| m.name == "drain")
        .expect("`drain` method missing from protocol");
    let param = method
        .non_self_params
        .iter()
        .find(|p| p.name == "payload")
        .expect("`payload` param missing from method");

    assert_eq!(
        param.mode,
        PassMode::Move,
        "protocol method's `move payload: String` must lift to PassMode::Move",
    );
}
