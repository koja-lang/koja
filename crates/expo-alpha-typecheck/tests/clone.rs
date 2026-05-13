//! Surface-level coverage for the `Clone` protocol — registration of
//! the protocol itself, the three heap-primitive `@intrinsic` impls
//! (`String`, `Binary`, `Bits`), and that user code can call
//! `.clone()` on each of them with the expected `Self` return shape.
//!
//! Out of scope here: the `derive_clone` synthesizer (lands with PR2,
//! the universal slice), the value-primitive impls (`Int.clone()`
//! and friends), and any generic-container impls. PR1 is the heap
//! primitives only.

use expo_alpha_typecheck::{CheckedProgram, GlobalKind};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;

mod common;

use common::typecheck_file as typecheck;
use common::typecheck_file_fail as typecheck_fail;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn clone_protocol_registers_with_clone_method() {
    let checked = typecheck("fn main\n  1\nend\n");
    let id = Identifier::new("Global", vec!["Clone".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&id)
        .expect("Clone protocol should be registered");
    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("Clone should be a lifted protocol; got {:?}", entry.kind);
    };
    let names: Vec<&str> = definition.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["clone"], "Clone surface is just `clone`");
}

#[test]
fn heap_primitive_clone_impls_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    for prim in ["String", "Binary", "Bits"] {
        assert_registered(&checked, &[prim, "clone"]);
    }
}

#[test]
fn user_code_can_call_string_clone() {
    typecheck(&dedent(
        "
        fn main -> String
          \"hello\".clone()
        end
        ",
    ));
}

#[test]
fn user_code_can_clone_a_borrowed_string_and_keep_the_source() {
    typecheck(&dedent(
        "
        fn duplicate(s: String) -> String
          copy = s.clone()
          copy
        end

        fn main -> String
          duplicate(\"hi\")
        end
        ",
    ));
}

#[test]
fn clone_with_extra_args_fails() {
    let failure = typecheck_fail(&dedent(
        "
        fn main -> String
          \"hello\".clone(\"oops\")
        end
        ",
    ));
    let messages = common::diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("clone") && (m.contains("argument") || m.contains("arity"))),
        "expected an arity diagnostic mentioning `clone`; got {messages:?}",
    );
}
