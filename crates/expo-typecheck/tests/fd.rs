//! Surface-level coverage for the auto-imported `Global.fd` source.
//! Pins that `Fd`, `File`, and `FileMode` register, that the public
//! methods on each (`Fd.read` / `Fd.write` / `File.read` / etc.)
//! show up in the registry, that the twelve `@extern "C"` shims
//! register under the correct receiver, and that user code can
//! exercise the file-path APIs without the autoimport raising
//! diagnostics.

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
fn fd_struct_and_public_methods_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Fd"]);
    assert_registered(&checked, &["Fd", "block"]);
    assert_registered(&checked, &["Fd", "close"]);
    assert_registered(&checked, &["Fd", "read"]);
    assert_registered(&checked, &["Fd", "unwatch"]);
    assert_registered(&checked, &["Fd", "watch"]);
    assert_registered(&checked, &["Fd", "write"]);
}

#[test]
fn file_struct_and_public_methods_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["File"]);
    assert_registered(&checked, &["File", "close"]);
    assert_registered(&checked, &["File", "delete"]);
    assert_registered(&checked, &["File", "exists?"]);
    assert_registered(&checked, &["File", "open"]);
    assert_registered(&checked, &["File", "read"]);
    assert_registered(&checked, &["File", "rename"]);
    assert_registered(&checked, &["File", "write"]);
}

#[test]
fn file_mode_enum_registers() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["FileMode"]);
}

#[test]
fn fd_extern_shims_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Fd", "expo_fd_close"]);
    assert_registered(&checked, &["Fd", "expo_fd_read"]);
    assert_registered(&checked, &["Fd", "expo_fd_write"]);
    assert_registered(&checked, &["Fd", "expo_io_block"]);
    assert_registered(&checked, &["Fd", "expo_rt_unwatch_fd"]);
    assert_registered(&checked, &["Fd", "expo_rt_watch_fd"]);
}

#[test]
fn file_extern_shims_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["File", "expo_file_delete"]);
    assert_registered(&checked, &["File", "expo_file_exists"]);
    assert_registered(&checked, &["File", "expo_file_open"]);
    assert_registered(&checked, &["File", "expo_file_read_all"]);
    assert_registered(&checked, &["File", "expo_file_rename"]);
    assert_registered(&checked, &["File", "expo_file_write_all"]);
}

#[test]
fn user_code_can_call_file_write_and_read() {
    typecheck(&dedent(
        "
        fn main -> Result<String, String>
          _ = File.write(\"out.txt\", \"hello\")
          File.read(\"out.txt\")
        end
        ",
    ));
}

#[test]
fn user_code_can_call_file_exists_predicate() {
    typecheck(&dedent(
        "
        fn main -> Bool
          File.exists?(\"out.txt\")
        end
        ",
    ));
}

#[test]
fn user_code_can_open_with_file_mode_match() {
    typecheck(&dedent(
        "
        fn main -> Result<File, String>
          File.open(\"out.txt\", FileMode.Read)
        end
        ",
    ));
}
