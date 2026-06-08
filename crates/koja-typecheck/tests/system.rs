//! Surface-level coverage for the auto-imported `Global.system`
//! source. Pins that `System` registers as a struct, that its four
//! static methods (`cwd` / `get_env` / `hostname` / `set_env`) and
//! their `@extern "C"` shims register, and that user code can call
//! into them without the autoimport raising diagnostics.

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
fn system_struct_and_public_methods_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["System"]);
    assert_registered(&checked, &["System", "cwd"]);
    assert_registered(&checked, &["System", "get_env"]);
    assert_registered(&checked, &["System", "hostname"]);
    assert_registered(&checked, &["System", "set_env"]);
}

#[test]
fn system_extern_shims_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["System", "koja_cwd"]);
    assert_registered(&checked, &["System", "koja_get_env"]);
    assert_registered(&checked, &["System", "koja_hostname"]);
    assert_registered(&checked, &["System", "koja_set_env"]);
}

#[test]
fn user_code_can_call_system_cwd() {
    typecheck(&dedent(
        "
        System.cwd()
        ",
    ));
}

#[test]
fn user_code_can_call_system_get_env() {
    typecheck(&dedent(
        "
        System.get_env(\"HOME\")
        ",
    ));
}

#[test]
fn user_code_can_call_system_set_env() {
    typecheck(&dedent(
        "
        System.set_env(\"FOO\", \"bar\")
        ",
    ));
}

#[test]
fn user_code_can_call_system_hostname() {
    typecheck(&dedent(
        "
        System.hostname()
        ",
    ));
}
