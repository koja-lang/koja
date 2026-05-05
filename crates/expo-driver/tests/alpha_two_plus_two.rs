//! End-to-end smoke tests for the alpha LLVM backend, the driver
//! mode dispatch, and the `--backend` flag.
//!
//! Drives the full alpha pipeline through the `expo` binary,
//! mirroring how a user would invoke it. The codegen success
//! criterion is that a bare `2 + 2` source compiles to a native
//! binary that exits with status 4 — the lowest-level evidence
//! that frontend → typecheck → IR → LLVM → object → linker is
//! wired correctly (a POC artifact: scripts will exit 0 by default
//! once `Kernel.exit` is wired in). The interpreter backend is
//! covered by a separate test that asserts the trailing value
//! `4` lands on stdout. Build and run paths both exercise the
//! script-mode pipeline (`.exps` extension); program-mode
//! end-to-end coverage waits on the project-pipeline follow-up.
//!
//! In addition to the codegen smoke tests, this file pins the
//! driver's mode-dispatch contract for [`alpha::resolve_alpha_mode`]:
//! standalone `.expo` files, unknown extensions, missing
//! `expo.toml`, project-mode (stubbed), and `build --backend=interpreter`
//! all surface specific error messages. The project-mode error is
//! asserted on its exact string so the follow-up PR that fills in
//! project mode is a pure stub-replacement.
//!
//! Lives in `expo-driver` instead of `expo-alpha-ir-llvm` because
//! the linking step needs `boring-sys` + the embedded runtime
//! archive, and `expo-driver`'s build graph already brings those in.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use expo_ast::util::dedent;

/// Script-mode fixture: bare `2 + 2` as the trailing expression of
/// the implicit script body. Used by both the build and run tests
/// (the only difference is whether the binary is kept or execed).
const TWO_PLUS_TWO_SCRIPT_SOURCE: &str = "
    2 + 2
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

/// Run `expo` from the test binary's working directory.
fn run_expo(args: &[&str]) -> std::process::Output {
    Command::new(expo_bin())
        .args(args)
        .output()
        .expect("failed to execute expo")
}

/// Run `expo` with `cwd` set to the given directory — the
/// project-mode and missing-`expo.toml` tests need this since the
/// resolver looks at the process's current directory.
fn run_expo_in(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(expo_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to execute expo")
}

#[test]
fn alpha_build_two_plus_two_exits_with_four() {
    let scratch = scratch_dir("build_two_plus_two");
    let fixture = write_fixture(
        &scratch,
        "two_plus_two.exps",
        &dedent(TWO_PLUS_TWO_SCRIPT_SOURCE),
    );
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
fn alpha_run_llvm_script_two_plus_two_propagates_exit_code() {
    let scratch = scratch_dir("run_llvm_two_plus_two");
    let fixture = write_fixture(
        &scratch,
        "two_plus_two.exps",
        &dedent(TWO_PLUS_TWO_SCRIPT_SOURCE),
    );

    // `expo alpha run --backend=llvm` parses script-mode and
    // dispatches to `lower_script` + `compile_script`; the bare
    // `2 + 2` becomes the implicit body of the produced binary's
    // `main`. Exit code 4 is the integer value of `2 + 2`
    // truncated to 8 bits — a POC artifact that will become 0
    // (success) once `Kernel.exit` is wired in.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    let exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(
        exit_code,
        4,
        "expected `expo alpha run --backend=llvm` to surface exit code 4, got {exit_code}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_two_plus_two_prints_value() {
    let scratch = scratch_dir("run_interpreter_two_plus_two");
    let fixture = write_fixture(
        &scratch,
        "two_plus_two.exps",
        &dedent(TWO_PLUS_TWO_SCRIPT_SOURCE),
    );

    // `expo alpha run` with no `--backend` flag defaults to the
    // interpreter; the trailing value is printed to stdout and the
    // process exits 0 regardless. Asymmetric with the LLVM backend
    // by design — see the run-llvm test for context.
    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim() == "4",
        "expected interpreter to print `4`, got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_build_interpreter_backend_errors() {
    let scratch = scratch_dir("build_interpreter_backend");
    let fixture = write_fixture(
        &scratch,
        "two_plus_two.exps",
        &dedent(TWO_PLUS_TWO_SCRIPT_SOURCE),
    );

    let output = run_expo(&[
        "alpha",
        "build",
        "--backend=interpreter",
        fixture.to_str().unwrap(),
    ]);
    assert!(
        !output.status.success(),
        "expected `expo alpha build --backend=interpreter` to error, but it succeeded",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot produce a binary"),
        "expected stderr to explain that the interpreter can't build, got:\n{stderr}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_program_extension_outside_project_errors() {
    let scratch = scratch_dir("program_outside_project");
    let fixture = write_fixture(&scratch, "stray.expo", "fn main -> Int\n  0\nend\n");

    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "expected `expo alpha run` on a standalone .expo file to error, but it succeeded",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is a project file"),
        "expected stderr to explain the project-file constraint, got:\n{stderr}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_unrecognized_extension_errors() {
    let scratch = scratch_dir("unrecognized_extension");
    let fixture = write_fixture(&scratch, "stray.txt", "2 + 2\n");

    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "expected `expo alpha run` on a .txt file to error, but it succeeded",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized source extension"),
        "expected stderr to mention the unrecognized extension, got:\n{stderr}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_no_file_no_project_errors() {
    let scratch = scratch_dir("no_file_no_project");

    let output = run_expo_in(&scratch, &["alpha", "run"]);
    assert!(
        !output.status.success(),
        "expected `expo alpha run` with no file and no project to error, but it succeeded",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no source file specified"),
        "expected stderr to mention the missing source file / project, got:\n{stderr}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_in_project_returns_stub_error() {
    let scratch = scratch_dir("project_stub");
    write_fixture(
        &scratch,
        "expo.toml",
        "[project]\nname = \"stub\"\nversion = \"0.0.0\"\n",
    );

    let output = run_expo_in(&scratch, &["alpha", "run"]);
    assert!(
        !output.status.success(),
        "expected the project-mode stub to exit non-zero, but `expo alpha run` succeeded",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("alpha project mode is not yet implemented"),
        "expected the project-mode stub error, got:\n{stderr}",
    );

    let _ = fs::remove_dir_all(&scratch);
}
