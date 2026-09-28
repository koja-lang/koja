//! Integration tests for `koja shell` REPL commands: `:quit` and its
//! `:q` alias end the session instead of evaluating further input.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn koja_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koja"))
}

/// Run `koja shell` with `input` piped to stdin and wait for it to exit.
fn shell(input: &str) -> Output {
    let mut child = Command::new(koja_bin())
        .arg("shell")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start koja shell");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child
        .wait_with_output()
        .expect("failed to wait for koja shell")
}

/// Asserts the shell exited on `command` itself rather than on the EOF
/// after it: the expression queued behind the command must never run.
fn assert_exits_on(command: &str) {
    let output = shell(&format!("{command}\n40 + 2\n"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "`{command}` did not exit cleanly:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !stdout.contains("42"),
        "shell kept evaluating after `{command}`:\n{stdout}"
    );
}

#[test]
fn quit_exits_shell() {
    assert_exits_on(":quit");
}

#[test]
fn q_alias_exits_shell() {
    assert_exits_on(":q");
}

#[test]
fn shell_evaluates_without_quit() {
    // Guards the two tests above: without a quit command the queued
    // expression does run, so their "no 42" assertion is meaningful.
    let output = shell("40 + 2\n");
    assert!(String::from_utf8_lossy(&output.stdout).contains("42"));
}
