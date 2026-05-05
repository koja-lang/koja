//! End-to-end smoke tests for the alpha LLVM backend, the driver
//! mode dispatch, and the `--backend` flag.
//!
//! Drives the full alpha pipeline through the `expo` binary,
//! mirroring how a user would invoke it. The codegen success
//! criterion is that a bare `2 + 2` source compiles to a native
//! binary that prints `4` on stdout and exits 0 — the lowest-level
//! evidence that frontend → typecheck → IR → LLVM → object →
//! linker is wired correctly. The auto-print wrapper that gives the
//! binary its observable behavior lives in
//! [`expo-runtime/src/alpha.rs`](../../expo-runtime/src/alpha.rs); it's
//! temporary scaffolding mirroring the eval interpreter's
//! `print-then-exit-0` contract while the language has no
//! user-level prints. Once `IO.puts` lands the wrapper is removed
//! and these tests will be replaced by ones that assert
//! user-program-controlled output. Build and run paths both
//! exercise the script-mode pipeline (`.exps` extension);
//! program-mode end-to-end coverage waits on the project-pipeline
//! follow-up.
//!
//! Backend symmetry is the headline contract pinned here:
//! `expo alpha run --backend=interpreter` and
//! `expo alpha run --backend=llvm` produce identical stdout + exit
//! code on the same source.
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

/// Script-mode fixture exercising boolean lowering: `true and
/// false` evaluates to `false`, both backends print `false` and
/// exit 0.
const BOOL_AND_SCRIPT_SOURCE: &str = "
    true and false
";

/// Script-mode fixture that drives a helper-function call from the
/// implicit body. `answer()` is declared inside the script source,
/// the trailing expression invokes it and adds 1, and the
/// auto-print wrapper renders `43\n`. Exercises the full
/// declare-then-call path through `compile_script` end-to-end.
const HELPER_CALL_SCRIPT_SOURCE: &str = "
    fn answer -> Int
      42
    end

    answer() + 1
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
fn alpha_build_script_prints_value_and_exits_zero() {
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

    // The auto-print wrapper in `expo-runtime/src/alpha.rs` prints
    // `4\n` and returns 0 from the binary's `main`. Temporary
    // scaffolding that goes away once `IO.puts` lands.
    let run_output = Command::new(&binary)
        .output()
        .expect("failed to exec compiled alpha binary");
    assert!(
        run_output.status.success(),
        "expected built binary to exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        run_output.status.code(),
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr),
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert_eq!(
        stdout.trim(),
        "4",
        "expected built binary to print `4`, got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_prints_value_and_exits_zero() {
    let scratch = scratch_dir("run_llvm_two_plus_two");
    let fixture = write_fixture(
        &scratch,
        "two_plus_two.exps",
        &dedent(TWO_PLUS_TWO_SCRIPT_SOURCE),
    );

    // `expo alpha run --backend=llvm` parses script-mode and
    // dispatches to `lower_script` + `compile_script`; the bare
    // `2 + 2` becomes the implicit body of the produced binary's
    // `main`, which then prints `4\n` and exits 0 thanks to the
    // auto-print wrapper. Backend symmetry with the interpreter
    // test below is the contract under test.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "4",
        "expected LLVM backend to print `4`, got stdout:\n{stdout}",
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
    // process exits 0. The LLVM backend matches this contract via
    // the auto-print wrapper, see `alpha_run_llvm_script_*` above.
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
fn alpha_run_llvm_script_calls_helper_prints_value() {
    let scratch = scratch_dir("run_llvm_helper_call");
    let fixture = write_fixture(
        &scratch,
        "helper_call.exps",
        &dedent(HELPER_CALL_SCRIPT_SOURCE),
    );

    // Pins the function-definition + call path through codegen
    // end-to-end. `fn answer` lowers to a non-entry helper that
    // the compiler declares with mangled symbol `TestApp.answer`
    // and the script body issues the matching `call i64`. The
    // built binary's `main` runs the call, adds 1, hands the
    // result to `__expo_alpha_print_i64`, and exits 0 — observable
    // as `43\n` on stdout. Backend symmetry with the interpreter
    // is implicitly checked by the matching alpha-eval test in
    // `expo-alpha-ir-eval/tests/calls.rs`.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (helper call) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "43",
        "expected LLVM backend to print `43` (42 from helper + 1), got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_bool_prints_false_and_exits_zero() {
    let scratch = scratch_dir("run_llvm_bool_and");
    let fixture = write_fixture(&scratch, "bool_and.exps", &dedent(BOOL_AND_SCRIPT_SOURCE));

    // Pins the boolean lowering + the auto-print wrapper's
    // `__expo_alpha_print_bool` path end-to-end: `true and false`
    // lowers through `IRBinOp::And` on i1, the wrapper zext's to
    // i64 and calls the bool printer, which writes `false\n` and
    // returns; `main` then exits 0.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (bool) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "false",
        "expected LLVM backend to print `false`, got stdout:\n{stdout}",
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
