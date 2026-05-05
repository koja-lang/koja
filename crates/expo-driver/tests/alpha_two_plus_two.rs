//! End-to-end smoke test for the alpha LLVM backend.
//!
//! Drives the full alpha pipeline through the `expo` binary, mirroring
//! how a user would invoke it. The slice's success criterion is a tiny
//! `fn main -> Int; 2 + 2; end` source compiling to a native binary
//! that exits with status 4 — the lowest-level evidence that
//! frontend → typecheck → IR → LLVM → object → linker is wired
//! correctly.
//!
//! Lives in `expo-driver` instead of `expo-alpha-ir-llvm` because the
//! linking step needs `boring-sys` + the embedded runtime archive, and
//! `expo-driver`'s build graph already brings those in.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use expo_ast::util::dedent;

/// Canonical fixture for the alpha LLVM slice: a `fn main -> Int`
/// whose body returns `2 + 2`. Both tests share this source so the
/// shape is described in one place; [`dedent`] lets it sit
/// naturally indented inside the test file.
const TWO_PLUS_TWO_SOURCE: &str = "
    fn main -> Int
      2 + 2
    end
";

fn expo_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_expo"))
}

/// Workspace-unique scratch directory under `$TMPDIR`. Includes the
/// process id so concurrent test runs (or `cargo test` retries) don't
/// stomp on each other.
fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("expo-alpha-e2e-{}-{label}", std::process::id(),));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("failed to create scratch dir");
    dir
}

fn write_fixture(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, source).expect("failed to write alpha fixture");
    path
}

fn run_expo(args: &[&str]) -> std::process::Output {
    Command::new(expo_bin())
        .args(args)
        .output()
        .expect("failed to execute expo")
}

#[test]
fn alpha_build_two_plus_two_exits_with_four() {
    let scratch = scratch_dir("build_two_plus_two");
    let fixture = write_fixture(&scratch, "two_plus_two.expo", &dedent(TWO_PLUS_TWO_SOURCE));
    let binary = scratch.join("two_plus_two");

    let build_output = run_expo(&[
        "alpha",
        "build",
        fixture.to_str().unwrap(),
        "-o",
        binary.to_str().unwrap(),
    ]);
    assert!(
        build_output.status.success(),
        "expo alpha build failed (exit {:?})\nstderr:\n{}",
        build_output.status.code(),
        String::from_utf8_lossy(&build_output.stderr),
    );
    assert!(
        binary.exists(),
        "expected binary at {}, but it is missing",
        binary.display(),
    );

    let run_output = Command::new(&binary)
        .output()
        .expect("failed to exec compiled alpha binary");
    let exit_code = run_output.status.code().unwrap_or(-1);
    assert_eq!(
        exit_code,
        4,
        "expected `2 + 2` to exit with status 4, got {exit_code}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr),
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_two_plus_two_propagates_exit_code() {
    let scratch = scratch_dir("run_two_plus_two");
    let fixture = write_fixture(&scratch, "two_plus_two.expo", &dedent(TWO_PLUS_TWO_SOURCE));

    // `expo alpha run` execs the produced binary and propagates its
    // exit code. The driver itself doesn't return success/failure
    // booleans here — the integer exit code is the contract.
    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    let exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(
        exit_code,
        4,
        "expected `expo alpha run` to surface exit code 4, got {exit_code}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let _ = fs::remove_dir_all(&scratch);
}
