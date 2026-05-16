//! Surface-level coverage for the auto-imported `Global.random`
//! source. Pins that `Random` registers as a struct, that `bytes`
//! and `int` register as static methods, that the two `@extern "C"`
//! shims register, and that user code can call into the public
//! surface without the autoimport raising diagnostics.

use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_typecheck::CheckedProgram;

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
fn random_struct_and_public_methods_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Random"]);
    assert_registered(&checked, &["Random", "bytes"]);
    assert_registered(&checked, &["Random", "int"]);
}

#[test]
fn random_extern_shims_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Random", "expo_random_bytes"]);
    assert_registered(&checked, &["Random", "expo_random_int"]);
}

#[test]
fn user_code_can_call_random_int() {
    typecheck(&dedent(
        "
        fn main -> Int
          Random.int(0, 100)
        end
        ",
    ));
}

#[test]
fn user_code_can_call_random_bytes() {
    typecheck(&dedent(
        "
        fn main -> Binary
          Random.bytes(16)
        end
        ",
    ));
}
