//! Surface-level coverage for the `Debug` protocol — registration of
//! the protocol itself, the primitive `@intrinsic` impls, the
//! universal-Debug fallback that lets `T.format()` resolve on bare
//! type parameters, and the hand-written stdlib container impls.
//!
//! The synthesizer-side coverage (which structs / enums get
//! synthesized impls, what the bodies look like) lives in
//! [`derive_debug`].

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::{CheckedProgram, GlobalKind};

mod common;

use common::typecheck_file as typecheck;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn debug_protocol_registers_with_format_print_inspect() {
    let checked = typecheck("fn main\n  1\nend\n");
    let id = Identifier::new("Global", vec!["Debug".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&id)
        .expect("Debug protocol should be registered");
    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("Debug should be a lifted protocol; got {:?}", entry.kind);
    };
    let names: Vec<&str> = definition.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains(&"format"),
        "Debug.format missing; got {names:?}",
    );
    assert!(
        names.contains(&"print"),
        "Debug.print missing; got {names:?}",
    );
    assert!(
        names.contains(&"inspect"),
        "Debug.inspect missing; got {names:?}",
    );
}

#[test]
fn primitive_debug_impls_register_format_method() {
    let checked = typecheck("fn main\n  1\nend\n");
    for prim in [
        "Bool", "Float", "Float32", "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32",
        "UInt64",
    ] {
        assert_registered(&checked, &[prim, "format"]);
    }
}

#[test]
fn binary_and_bits_have_debug_impls() {
    // `Binary` and `Bits` carry placeholder Debug impls so generic
    // containers carrying them (`Result<Binary, _>`, `Option<Bits>`)
    // monomorphize cleanly. See `lib/global/src/debug.koja`.
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Binary", "format"]);
    assert_registered(&checked, &["Bits", "format"]);
}

#[test]
fn generic_container_debug_impls_register_format_method() {
    // Hand-written impls live in
    // `lib/global/src/debug_containers.koja`. Each one stamps a
    // `<Type>.format` entry in the registry; calling it requires
    // monomorphization (covered in `koja-ir/tests`).
    let checked = typecheck("fn main\n  1\nend\n");
    for ty in ["List", "Map", "Option", "Pair", "Result", "Set"] {
        assert_registered(&checked, &[ty, "format"]);
    }
}

#[test]
fn universal_debug_fallback_resolves_format_on_bare_type_param() {
    // `T` has no declared bound but `T.format()` resolves through
    // the universal-Debug fallback in
    // `koja-typecheck/src/pipeline/resolve/calls/bounded.rs`.
    let source = "
        fn show<T>(value: T) -> String
          value.format()
        end

        fn main
          show(1)
          0
        end
        ";

    let checked = typecheck(&dedent(source));
    let entry = checked
        .registry
        .lookup(&Identifier::new(common::PACKAGE, vec!["show".to_string()]))
        .map(|(_, e)| e)
        .expect("show registered");
    let GlobalKind::Function(Some(_)) = &entry.kind else {
        panic!(
            "show should have a lifted function signature; got {:?}",
            entry.kind,
        );
    };
}
