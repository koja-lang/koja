//! Driver-level smoke tests for `expo alpha {eval,shell}`.
//!
//! These cover the *full* path from the CLI down to the v2
//! interpreter (`parse → typecheck-v2 → ir-v2 → ir-eval-v2`). When
//! they pass, the v2 alpha pipeline is end-to-end alive at its POC
//! scope without disturbing the v1 `expo eval` / `expo shell`
//! production paths.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn expo_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_expo"))
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .expect("expo/examples directory not found")
}

#[test]
fn alpha_eval_two_plus_two_prints_four() {
    let example = examples_dir().join("two_plus_two.expo");
    let output = Command::new(expo_bin())
        .arg("alpha")
        .arg("eval")
        .arg(&example)
        .output()
        .expect("failed to spawn `expo alpha eval`");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "`expo alpha eval` failed (exit {:?}):\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code(),
    );
    assert_eq!(
        stdout.trim(),
        "4",
        "expected `4`, got stdout=`{stdout}` stderr=`{stderr}`",
    );
}

#[test]
fn alpha_shell_evaluates_arithmetic_across_session() {
    let mut child = Command::new(expo_bin())
        .arg("alpha")
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `expo alpha shell`");
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin
            .write_all(b"2 + 2\n3 * 4\n:quit\n")
            .expect("write to child stdin");
    }
    let output = child.wait_with_output().expect("wait on child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "`expo alpha shell` exit failed (code {:?}): stdout={stdout} stderr={stderr}",
        output.status.code(),
    );
    let result_lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.parse::<i64>().is_ok())
        .collect();
    assert_eq!(
        result_lines,
        vec!["4", "12"],
        "expected REPL to print [4, 12]; full stdout: {stdout}\nstderr: {stderr}",
    );
}

#[test]
fn alpha_shell_rolls_back_session_on_error() {
    let mut child = Command::new(expo_bin())
        .arg("alpha")
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `expo alpha shell`");
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin
            .write_all(b"2 + 2\nundefined_thing\n5 + 7\n:quit\n")
            .expect("write to child stdin");
    }
    let output = child.wait_with_output().expect("wait on child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "shell should still exit 0");
    let result_lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.parse::<i64>().is_ok())
        .collect();
    assert_eq!(
        result_lines,
        vec!["4", "12"],
        "rolled-back input shouldn't poison later evals; stdout: {stdout}\nstderr: {stderr}",
    );
    assert!(
        stderr.contains("error:"),
        "expected an `error:` line on stderr, got: {stderr}",
    );
}
