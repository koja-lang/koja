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

/// Script-mode fixture exercising `if` lowering through both
/// backends. Each helper carries the same `if` shape with a
/// different literal cond: `pick_then` runs the early `return 1`
/// inside the then-arm; `pick_merge` skips the body and falls
/// through to the trailing `2` in the merge block. Identifier
/// references in function bodies aren't resolved until the locals
/// slice, so the cond can't be a parameter — two helpers stand in.
/// The script body sums them, the auto-print wrapper renders
/// `3\n`. Pins the CFG-based lowering path end-to-end (LLVM emits
/// a real `br i1` + `br label` pair; the interpreter dispatches
/// on `CondBranch` at runtime).
const IF_BRANCH_SCRIPT_SOURCE: &str = "
    fn pick_then -> Int
      if true
        return 1
      end
      2
    end

    fn pick_merge -> Int
      if false
        return 1
      end
      2
    end

    pick_then() + pick_merge()
";

/// Script-mode fixture exercising the string-literal slice. Bare
/// `"hello"` lowers to `IRInstruction::Const { ConstValue::String }`
/// with return type `IRType::String`; the LLVM emit produces a
/// private constant matching expo-codegen's
/// `[i64 bit_length][payload bytes][NUL]` layout, and the auto-print
/// wrapper hands the payload pointer to `__expo_alpha_print_string`.
/// Stdout is `hello\n`. Pins the full alpha pipeline + runtime
/// printer for `IRType::String`.
const STRING_LITERAL_SCRIPT_SOURCE: &str = "
    \"hello\"
";

/// Script-mode fixture exercising the float-literal slice through
/// arithmetic. `2.0 + 2.0` lowers to two `Const(Float64)` ops + an
/// `IRBinOp::Add`; LLVM emits `fadd double` and the auto-print
/// wrapper dispatches to `__expo_alpha_print_f64`. Stdout is
/// `4.0\n` (Rust `{:?}` formatting, matching the interpreter's
/// `Value::Float64` `Display`).
const FLOAT_ARITH_SCRIPT_SOURCE: &str = "
    2.0 + 2.0
";

/// Script-mode fixture pinning the float ordered-comparison path:
/// `1.5 < 2.5` lowers to `IRBinOp::Lt` over two `Float64` operands.
/// LLVM emits `fcmp olt double` and zext's the i1 to i64 for the
/// bool printer; both backends print `true\n`.
const FLOAT_COMPARE_SCRIPT_SOURCE: &str = "
    1.5 < 2.5
";

/// Script-mode fixture exercising the `@intrinsic` slice end-to-end.
/// The script declares `@intrinsic fn print(s: String)` and calls it
/// with `"hello"`. LLVM lowers the call to `call void
/// @Global.print(ptr ...)`, the synthesized `@Global.print` body
/// dispatches to `__expo_alpha_print_string`, and the runtime
/// printer writes `hello\n`. The trailing expression has type
/// `Unit` so the auto-print wrapper around `main` is a no-op.
/// Backend symmetry: the eval path runs the
/// [`expo_alpha_ir_eval::intrinsics::global_print`] handler, which
/// writes the same `hello\n` to stdout via Rust's `io::stdout`. Both
/// produce identical observable output.
const INTRINSIC_PRINT_SCRIPT_SOURCE: &str = "
    @intrinsic
    fn print(s: String)

    print(\"hello\")
";

/// Script-mode fixture exercising `unless` lowering through both
/// backends. `unless cond` runs its body when the cond is `false`,
/// the inverse of `if`. `pick_body` runs the early `return 1`
/// because its cond is `false`; `pick_skip` falls through to `2`
/// because its cond is `true`. The sum is `3`; the auto-print
/// wrapper renders `3\n`. Pins the swapped-arms CondBranch shape
/// `unless` emits, distinct from `if`'s shape.
const UNLESS_BRANCH_SCRIPT_SOURCE: &str = "
    fn pick_body -> Int
      unless false
        return 1
      end
      2
    end

    fn pick_skip -> Int
      unless true
        return 1
      end
      2
    end

    pick_body() + pick_skip()
";

/// Script-mode fixture exercising the alpha struct slice end-to-end:
/// a `struct Point` decl, a struct literal `Point { x: 5, y: 10 }`,
/// and a field-read `.x` projecting the literal-5 leaf. Pins the
/// full pipeline contract:
///
/// - typecheck registers `TestApp.Point` with two `Int` fields and
///   resolves the literal + projection;
/// - IR lowering stamps an `IRStructDecl` on `IRPackage::structs`,
///   produces an `IRInstruction::StructInit` with canonicalized
///   field-init order, and threads an `IRInstruction::FieldGet`
///   through the script body;
/// - LLVM emits `%TestApp.Point = type { i64, i64 }`, materializes
///   the literal through alloca + GEP + store, projects `.x` via a
///   second alloca + GEP + load, and the auto-print wrapper
///   dispatches to `__expo_alpha_print_i64`;
/// - the eval interpreter constructs a `Value::Struct { symbol,
///   fields }`, indexes field 0, and the driver's auto-print emits
///   the same `5\n`.
const STRUCT_FIELD_SCRIPT_SOURCE: &str = "
    struct Point
      x: Int
      y: Int
    end

    Point{x: 5, y: 10}.x
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
fn alpha_run_llvm_script_if_branch_prints_three() {
    let scratch = scratch_dir("run_llvm_if_branch");
    let fixture = write_fixture(&scratch, "if_branch.exps", &dedent(IF_BRANCH_SCRIPT_SOURCE));

    // `pick(true) + pick(false)` exercises the early-return arm and
    // the merge fall-through inside the same helper. LLVM emits a
    // multi-block `pick` with `br i1` on the cond and `br label`
    // back to the merge; the script-mode `main` is a single block
    // wrapping the two calls and the addition.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (if branch) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "3",
        "expected LLVM backend to print `3` (1 + 2 from the two pick() arms), got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_if_branch_prints_three() {
    let scratch = scratch_dir("run_interpreter_if_branch");
    let fixture = write_fixture(&scratch, "if_branch.exps", &dedent(IF_BRANCH_SCRIPT_SOURCE));

    // Backend symmetry with the LLVM test above. The interpreter
    // dispatches the `CondBranch` terminator at runtime instead of
    // emitting machine-code branches, but the observable contract
    // (stdout `3`, exit 0) matches.
    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, if branch) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "3",
        "expected interpreter to print `3`, got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_string_literal_prints_hello() {
    let scratch = scratch_dir("run_llvm_string_literal");
    let fixture = write_fixture(
        &scratch,
        "string_literal.exps",
        &dedent(STRING_LITERAL_SCRIPT_SOURCE),
    );

    // Pins the full alpha string-literal slice through the LLVM
    // backend: `"hello"` lowers to `Const(ConstValue::String)`,
    // emit_const_string lays out a private constant with the v1
    // header (`{ i64 40, [6 x i8] c"hello\00" }`), the auto-print
    // wrapper hands the payload pointer to
    // `__expo_alpha_print_string`, and the runtime printer reads
    // the bit-length 8 bytes back, writes the bytes, and a newline.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (string literal) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        output.stdout,
        b"hello\n",
        "expected LLVM backend to print `hello\\n`, got stdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_string_literal_prints_hello() {
    let scratch = scratch_dir("run_interpreter_string_literal");
    let fixture = write_fixture(
        &scratch,
        "string_literal.exps",
        &dedent(STRING_LITERAL_SCRIPT_SOURCE),
    );

    // Backend symmetry with the LLVM test above. The interpreter
    // produces `Value::String("hello")`; its `Display` writes the
    // inner bytes verbatim followed by a newline so stdout matches
    // the LLVM path byte-for-byte.
    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, string literal) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        output.stdout,
        b"hello\n",
        "expected interpreter to print `hello\\n`, got stdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_unless_branch_prints_three() {
    let scratch = scratch_dir("run_llvm_unless_branch");
    let fixture = write_fixture(
        &scratch,
        "unless_branch.exps",
        &dedent(UNLESS_BRANCH_SCRIPT_SOURCE),
    );

    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (unless branch) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "3",
        "expected LLVM backend to print `3` (`unless` arms swapped relative to `if`), \
         got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_unless_branch_prints_three() {
    let scratch = scratch_dir("run_interpreter_unless_branch");
    let fixture = write_fixture(
        &scratch,
        "unless_branch.exps",
        &dedent(UNLESS_BRANCH_SCRIPT_SOURCE),
    );

    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, unless branch) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "3",
        "expected interpreter to print `3` (`unless` arms swapped relative to `if`), \
         got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_float_arith_prints_four_point_zero() {
    let scratch = scratch_dir("run_llvm_float_arith");
    let fixture = write_fixture(
        &scratch,
        "float_arith.exps",
        &dedent(FLOAT_ARITH_SCRIPT_SOURCE),
    );

    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (float arith) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "4.0",
        "expected LLVM backend to print `4.0` (Rust `{{:?}}` form), got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_float_arith_prints_four_point_zero() {
    let scratch = scratch_dir("run_interpreter_float_arith");
    let fixture = write_fixture(
        &scratch,
        "float_arith.exps",
        &dedent(FLOAT_ARITH_SCRIPT_SOURCE),
    );

    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, float arith) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "4.0",
        "expected interpreter to print `4.0` (matching LLVM backend), got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_float_compare_prints_true() {
    let scratch = scratch_dir("run_llvm_float_compare");
    let fixture = write_fixture(
        &scratch,
        "float_compare.exps",
        &dedent(FLOAT_COMPARE_SCRIPT_SOURCE),
    );

    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (float compare) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "true",
        "expected LLVM backend to print `true`, got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_float_compare_prints_true() {
    let scratch = scratch_dir("run_interpreter_float_compare");
    let fixture = write_fixture(
        &scratch,
        "float_compare.exps",
        &dedent(FLOAT_COMPARE_SCRIPT_SOURCE),
    );

    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, float compare) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "true",
        "expected interpreter to print `true`, got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_intrinsic_print_prints_hello() {
    let scratch = scratch_dir("run_llvm_intrinsic_print");
    // Fixture stem is `Global` so the driver-derived package
    // matches the dispatch-table key (`Global.print`). The future
    // stdlib-loading slice will register intrinsics from real
    // `lib/global/src/...` files and this convention disappears.
    let fixture = write_fixture(
        &scratch,
        "Global.exps",
        &dedent(INTRINSIC_PRINT_SCRIPT_SOURCE),
    );

    // Drives the full alpha `@intrinsic` slice through the LLVM
    // backend: the script declares an intrinsic, calls it on a
    // string literal, and the binary's stdout is exactly `hello\n`
    // (no auto-print wrapper firing since the trailing is
    // `Unit`-typed).
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (intrinsic print) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        output.stdout,
        b"hello\n",
        "expected LLVM backend to print `hello\\n`, got stdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_intrinsic_print_prints_hello() {
    let scratch = scratch_dir("run_interpreter_intrinsic_print");
    let fixture = write_fixture(
        &scratch,
        "Global.exps",
        &dedent(INTRINSIC_PRINT_SCRIPT_SOURCE),
    );

    // Backend symmetry with the LLVM test above: the interpreter
    // routes the call through
    // `expo_alpha_ir_eval::intrinsics::global_print`, which writes
    // `hello\n` and returns `Value::Unit`. The driver's auto-print
    // skips `Unit`, so stdout is `hello\n` byte-for-byte.
    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, intrinsic print) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        output.stdout,
        b"hello\n",
        "expected interpreter to print `hello\\n`, got stdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_llvm_script_struct_field_prints_five() {
    let scratch = scratch_dir("run_llvm_struct_field");
    let fixture = write_fixture(
        &scratch,
        "struct_field.exps",
        &dedent(STRUCT_FIELD_SCRIPT_SOURCE),
    );

    // Drives the alpha struct slice through the LLVM backend
    // end-to-end. The trailing `Point { x: 5, y: 10 }.x` lowers
    // through `IRInstruction::StructInit` (alloca + per-field
    // store) and `IRInstruction::FieldGet` (alloca + GEP + load),
    // the auto-print wrapper hands the loaded i64 to
    // `__expo_alpha_print_i64`, and stdout is `5\n`.
    let output = run_expo(&["alpha", "run", "--backend=llvm", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run --backend=llvm` (struct field) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "5",
        "expected LLVM backend to print `5` (Point.x), got stdout:\n{stdout}",
    );

    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn alpha_run_interpreter_script_struct_field_prints_five() {
    let scratch = scratch_dir("run_interpreter_struct_field");
    let fixture = write_fixture(
        &scratch,
        "struct_field.exps",
        &dedent(STRUCT_FIELD_SCRIPT_SOURCE),
    );

    // Backend symmetry with the LLVM test above. The interpreter
    // builds a `Value::Struct { symbol: TestApp.Point, fields:
    // [Int(5), Int(10)] }` for the literal, projects field 0
    // through `IRInstruction::FieldGet`, and the driver's
    // auto-print emits `5\n`.
    let output = run_expo(&["alpha", "run", fixture.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "expected `expo alpha run` (interpreter, struct field) to exit 0, got {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "5",
        "expected interpreter to print `5` (Point.x), got stdout:\n{stdout}",
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
