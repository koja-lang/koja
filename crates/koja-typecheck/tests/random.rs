//! Surface-level coverage for the auto-imported `Global.random`
//! source. Pins that `Random` registers as a struct, that `bytes`
//! and `int` register as static methods, that the two `@extern "C"`
//! shims register, and that user code can call into the public
//! surface without the autoimport raising diagnostics.

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::typecheck_script as typecheck;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn random_struct_and_public_methods_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["Random"]);
    assert_registered(&checked, &["Random", "bytes"]);
    assert_registered(&checked, &["Random", "int"]);
}

#[test]
fn random_extern_shims_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["Random", "koja_random_bytes"]);
    assert_registered(&checked, &["Random", "koja_random_int"]);
}

#[test]
fn user_code_can_call_random_int() {
    typecheck(&dedent(
        "
        Random.int(0, 100)
        ",
    ));
}

#[test]
fn user_code_can_call_random_bytes() {
    typecheck(&dedent(
        "
        Random.bytes(16)
        ",
    ));
}
