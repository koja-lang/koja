//! Surface-level coverage for the auto-imported `Global.io` source.
//! Pins that `IO` registers as a struct, that `IOReady` registers
//! as an enum with three variants, that `STDIN` / `STDOUT` /
//! `STDERR` register as module-level constants, that the public
//! methods (`gets` / `puts` / `warn` / `write`) register, and that
//! user code can call into them without the autoimport raising
//! diagnostics.

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
fn io_struct_and_public_methods_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["IO"]);
    assert_registered(&checked, &["IO", "gets"]);
    assert_registered(&checked, &["IO", "puts"]);
    assert_registered(&checked, &["IO", "warn"]);
    assert_registered(&checked, &["IO", "write"]);
}

#[test]
fn io_ready_enum_registers() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["IOReady"]);
}

#[test]
fn standard_fd_constants_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["STDERR"]);
    assert_registered(&checked, &["STDIN"]);
    assert_registered(&checked, &["STDOUT"]);
}

#[test]
fn user_code_can_call_io_puts() {
    typecheck(&dedent(
        "
        IO.puts(\"hello\")
        ",
    ));
}

#[test]
fn user_code_can_call_io_warn() {
    typecheck(&dedent(
        "
        IO.warn(\"oops\")
        ",
    ));
}

#[test]
fn user_code_can_call_io_write() {
    typecheck(&dedent(
        "
        IO.write(\"hello\")
        ",
    ));
}
