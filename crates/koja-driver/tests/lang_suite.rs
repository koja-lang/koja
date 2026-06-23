use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

extern crate libc;

fn lang_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lang")
        .canonicalize()
        .expect("tests/lang directory not found")
}

fn koja_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koja"))
}

fn collect_test_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("failed to read test dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "koja" || e == "kojs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn library_path() -> Option<String> {
    if let Ok(val) = std::env::var("LIBRARY_PATH") {
        return Some(val);
    }
    if cfg!(target_os = "macos") {
        for candidate in ["/opt/homebrew/lib", "/usr/local/lib"] {
            if Path::new(candidate).is_dir() {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

const TEST_TIMEOUT: Duration = Duration::from_secs(45);

/// The two execution backends every parity-eligible fixture runs under.
/// Both must produce the fixture's golden stdout, which (transitively)
/// pins interpreter <-> LLVM output parity.
const BACKENDS: [&str; 2] = ["llvm", "interpreter"];

/// Fixtures (by file stem) that run under LLVM only. These `signal(...)`
/// a child then **busy-wait** on `alive?()` for it to die — which works
/// under the multi-threaded native scheduler (the child runs on another
/// worker) but livelocks under the single-threaded cooperative
/// interpreter: the spin loop has no yield point, so the signalled child
/// is starved. Cooperative parity here needs preemptive yield-checks at
/// loop back-edges (Phase 5 A1, not yet implemented). They still run
/// under LLVM, where the reclaim assertion is what matters.
const LLVM_ONLY: &[&str] = &["message_reclaim", "signal_only", "spawn_reclaim"];

/// Whether `file` runs under the interpreter in addition to LLVM.
fn eval_eligible(file: &Path) -> bool {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    !LLVM_ONLY.contains(&stem.as_ref())
}

/// The backends a single-file fixture runs under: both, unless it is an
/// [`LLVM_ONLY`] fixture.
fn backends_for(file: &Path) -> &'static [&'static str] {
    if eval_eligible(file) {
        &BACKENDS
    } else {
        &BACKENDS[..1]
    }
}

fn run_koja(file: &Path) -> (String, String, i32) {
    run_koja_backend(file, "llvm")
}

fn run_koja_backend(file: &Path, backend: &str) -> (String, String, i32) {
    let backend_flag = format!("--backend={backend}");
    run_with_timeout(|cmd| {
        cmd.arg("run").arg(&backend_flag).arg(file);
    })
}

fn run_with_timeout(configure: impl FnOnce(&mut Command)) -> (String, String, i32) {
    let mut cmd = Command::new(koja_bin());
    configure(&mut cmd);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", &lib_path);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to execute koja");
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (
                    String::new(),
                    format!("test timed out (killed after {}s)", TEST_TIMEOUT.as_secs()),
                    -1,
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return (String::new(), format!("wait error: {e}"), -1),
        }
    }

    let output = child.wait_with_output().expect("failed to collect output");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

// ---------------------------------------------------------------------------
// Shared runners
// ---------------------------------------------------------------------------

fn run_pass_dir(dir: &Path, label: &str) {
    let files = collect_test_files(dir);
    assert!(
        !files.is_empty(),
        "no .koja/.kojs test files found in {label} ({})",
        dir.display()
    );

    let mut failures = Vec::new();

    for file in &files {
        let test_name = format!("{label}/{}", file.file_stem().unwrap().to_string_lossy());
        let expected_path = file.with_extension("stdout");

        if !expected_path.exists() {
            failures.push(format!("{test_name}: missing .stdout file"));
            continue;
        }
        let expected = fs::read_to_string(&expected_path).unwrap();

        // Run under every eligible backend and assert each against the
        // golden; both matching the same golden is interpreter <-> LLVM
        // parity by transitivity.
        for &backend in backends_for(file) {
            let (stdout, stderr, code) = run_koja_backend(file, backend);
            if code != 0 {
                failures.push(format!(
                    "{test_name} ({backend}): exited with code {code}\nstderr:\n{stderr}"
                ));
                continue;
            }
            if stdout != expected {
                let diff = diff_lines(&stdout, &expected);
                failures.push(format!("{test_name} ({backend}): output mismatch\n{diff}"));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "\n{} {label} test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}

/// LLVM-only by nature: the failure is raised before any backend runs
/// (typecheck / lowering), so the diagnostic is backend-independent and
/// running the interpreter too would assert the identical stderr.
fn run_compile_fail_dir(dir: &Path, label: &str) {
    if !dir.exists() {
        return;
    }

    let files = collect_test_files(dir);
    let mut failures = Vec::new();

    for file in &files {
        let test_name = format!("{label}/{}", file.file_stem().unwrap().to_string_lossy());
        let expected_path = file.with_extension("stdout");

        if !expected_path.exists() {
            failures.push(format!("{test_name}: missing .stdout file"));
            continue;
        }

        let (_stdout, stderr, code) = run_koja(file);

        if code == 0 {
            failures.push(format!(
                "{test_name}: expected compilation failure but succeeded"
            ));
            continue;
        }

        let expected = fs::read_to_string(&expected_path).unwrap();
        let pattern = expected.trim();
        if !stderr.contains(pattern) {
            failures.push(format!(
                "{test_name}: stderr does not contain expected pattern\n\
                 expected pattern: {pattern:?}\n\
                 actual stderr:\n{stderr}"
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "\n{} {label} test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}

/// LLVM-only: the two backends surface runtime faults through different
/// channels (an LLVM panic stacktrace vs. the interpreter's
/// `RuntimeError`), so the expected-stderr pattern is backend-specific
/// rather than a cross-backend parity assertion.
fn run_runtime_fail_dir(dir: &Path, label: &str) {
    if !dir.exists() {
        return;
    }

    let files = collect_test_files(dir);
    let mut failures = Vec::new();

    for file in &files {
        let test_name = format!("{label}/{}", file.file_stem().unwrap().to_string_lossy());
        let expected_path = file.with_extension("stderr");

        if !expected_path.exists() {
            failures.push(format!("{test_name}: missing .stderr file"));
            continue;
        }

        let (_stdout, stderr, code) = run_koja(file);

        if code == 0 {
            failures.push(format!(
                "{test_name}: expected runtime failure but exited with 0"
            ));
            continue;
        }

        let expected = fs::read_to_string(&expected_path).unwrap();
        let pattern = expected.trim();
        if !stderr.contains(pattern) {
            failures.push(format!(
                "{test_name}: stderr does not contain expected pattern\n\
                 expected pattern: {pattern:?}\n\
                 actual stderr:\n{stderr}"
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "\n{} {label} test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}

fn run_project_dir(dir: &Path, label: &str) {
    run_project_dir_with(dir, label, &[]);
}

/// Run a project fixture under both backends, locking in interpreter ↔
/// LLVM parity for the project execution path (same stdout, same exit
/// code). The entry process installs the cooperative runtime under the
/// interpreter, so `spawn` / `receive` / lifecycle behave as under LLVM.
fn run_project_dir_with(dir: &Path, label: &str, extra_args: &[&str]) {
    for backend in BACKENDS {
        run_project_dir_backend(dir, label, backend, extra_args);
    }
}

/// Run a project fixture through `koja run --backend=<backend>` and
/// assert stdout / exit code against the fixture's expectations.
fn run_project_dir_backend(dir: &Path, label: &str, backend: &str, extra_args: &[&str]) {
    assert!(dir.exists(), "test fixture {label}/ not found");

    let expected_path = dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing {label}/expected.stdout");

    let dir_owned = dir.to_path_buf();
    let backend_flag = format!("--backend={backend}");
    let extra: Vec<String> = extra_args.iter().map(|s| s.to_string()).collect();
    let (stdout, stderr, code) = run_with_timeout(|cmd| {
        cmd.arg("run").arg(&backend_flag).current_dir(&dir_owned);
        if !extra.is_empty() {
            cmd.arg("--");
            for a in &extra {
                cmd.arg(a);
            }
        }
    });

    let expected_code = if dir.join("expected.exit_code").exists() {
        fs::read_to_string(dir.join("expected.exit_code"))
            .unwrap()
            .trim()
            .parse::<i32>()
            .expect("expected.exit_code must be an integer")
    } else {
        0
    };

    assert!(
        code == expected_code,
        "koja run ({backend}) in {label}: expected exit code {expected_code}, got {code}\nstderr:\n{stderr}"
    );

    let expected = fs::read_to_string(&expected_path).unwrap();
    if stdout != expected {
        let diff = diff_lines(&stdout, &expected);
        panic!("\n--- FAIL: {label} ({backend}) ---\n{diff}");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn diff_lines(actual: &str, expected: &str) -> String {
    let actual_lines: Vec<&str> = actual.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let mut diff = String::new();
    let max = actual_lines.len().max(expected_lines.len());
    for i in 0..max {
        let a = actual_lines.get(i).unwrap_or(&"<missing>");
        let e = expected_lines.get(i).unwrap_or(&"<missing>");
        if a != e {
            diff.push_str(&format!(
                "  line {}: expected {:?}, got {:?}\n",
                i + 1,
                e,
                a
            ));
        }
    }
    diff
}

// ---------------------------------------------------------------------------
// Macro + test declarations
// ---------------------------------------------------------------------------

macro_rules! lang_test_dir {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_pass_dir(&lang_dir().join($dir), $dir);
        }
    };
    ($name:ident, $dir:expr, compile_fail) => {
        #[test]
        fn $name() {
            run_compile_fail_dir(&lang_dir().join($dir), $dir);
        }
    };
    ($name:ident, $dir:expr, runtime_fail) => {
        #[test]
        fn $name() {
            run_runtime_fail_dir(&lang_dir().join($dir), $dir);
        }
    };
    ($name:ident, $dir:expr, project) => {
        #[test]
        fn $name() {
            run_project_dir(&lang_dir().join($dir), $dir);
        }
    };
    ($name:ident, $dir:expr, project, $($arg:expr),+) => {
        #[test]
        fn $name() {
            run_project_dir_with(&lang_dir().join($dir), $dir, &[$($arg),+]);
        }
    };
}

// Pass tests
lang_test_dir!(lang_basics, "basics");
lang_test_dir!(lang_functions, "functions");
lang_test_dir!(lang_types, "types");
lang_test_dir!(lang_generics, "generics");
lang_test_dir!(lang_protocols, "protocols");
lang_test_dir!(lang_collections, "collections");
lang_test_dir!(lang_ownership, "ownership");
lang_test_dir!(lang_io, "io");
// Memory-reclaim regressions: process payload lifecycles (spawn
// config/state, delivered + undelivered messages, stale replies,
// signal-only teardown) and the match-subject release, asserted via
// `koja_rt_live_blocks` deltas.
lang_test_dir!(lang_memory, "memory");

// Failure tests
lang_test_dir!(lang_compile_fail, "compile_fail", compile_fail);
lang_test_dir!(lang_runtime_fail, "runtime_fail", runtime_fail);

/// Backtrace smoke test: a debug `koja run` of the `panic_backtrace`
/// fixture must surface the user's Koja call chain — the `crash()`
/// frame attributed to the fixture's source file. Guards against silent
/// regressions in DWARF emission, frame-pointer maintenance, or the
/// runtime symbolizer that would collapse the trace to "<no frames>".
#[test]
fn lang_panic_backtrace_frames() {
    let file = lang_dir().join("runtime_fail").join("panic_backtrace.kojs");
    let (_stdout, stderr, code) = run_koja(&file);

    assert!(
        code != 0,
        "panic_backtrace: expected nonzero exit, got {code}"
    );
    for needle in [
        "** (panic) called unwrap on None",
        "crash()",
        "panic_backtrace.kojs:",
    ] {
        assert!(
            stderr.contains(needle),
            "expected backtrace stderr to contain {needle:?}, got:\n{stderr}"
        );
    }
}

// Multi-file project tests
lang_test_dir!(lang_project, "project", project);
lang_test_dir!(lang_diamond, "diamond", project);
lang_test_dir!(lang_cross_ref, "cross_ref", project);
lang_test_dir!(lang_local_dep, "local_dep", project);
lang_test_dir!(lang_alias_dep, "alias_dep", project);
lang_test_dir!(lang_pkg_fn, "pkg_fn", project);

/// Canary for the TypeIdentifier migration: two packages each define
/// `struct Config`, used from a root package via aliases. Today the bare-name
/// entries in `TypeContext::name_index` are last-write-wins, so the pipeline's own
/// references to `Config` resolve to beta.Config (or vice versa) and the
/// program fails at typecheck. This test must pass once the migration is
/// complete; until then it is the oracle that we are actually fixing the bug.
#[test]
fn lang_package_collision() {
    run_project_dir(&lang_dir().join("package_collision"), "package_collision");
}
// Project fixtures run under both backends (see `run_project_dir_with`),
// pinning interpreter ↔ LLVM parity for the project execution path.
lang_test_dir!(lang_kernel_exit, "kernel_exit", project);
lang_test_dir!(lang_process_entry, "process_entry", project);
lang_test_dir!(lang_process_exit, "process_exit", project);
lang_test_dir!(lang_process_argv, "process_argv", project, "hello", "world");
lang_test_dir!(lang_receive_after, "receive_after", project);

/// LLVM-only: links a user-provided C static library (`@link`), which the
/// interpreter cannot resolve (no linker / dlopen path for arbitrary
/// symbols).
#[test]
fn lang_ffi() {
    let dir = lang_dir().join("ffi");
    assert!(dir.exists(), "test fixture ffi/ not found");

    let c_src = dir.join("ffi_helper.c");
    let lib_path = dir.join("libffi_helper.a");

    let obj = dir.join("ffi_helper.o");
    let cc_status = Command::new("cc")
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(&c_src)
        .status()
        .expect("failed to run cc");
    assert!(cc_status.success(), "C compilation failed");

    let ar_status = Command::new("ar")
        .args(["rcs"])
        .arg(&lib_path)
        .arg(&obj)
        .status()
        .expect("failed to run ar");
    assert!(ar_status.success(), "ar failed");

    let _ = fs::remove_file(&obj);

    let ffi_lib_path = match library_path() {
        Some(existing) => format!("{}:{}", dir.display(), existing),
        None => dir.display().to_string(),
    };
    let output = Command::new(koja_bin())
        .arg("run")
        .arg("--backend=llvm")
        .current_dir(&dir)
        .env("LIBRARY_PATH", &ffi_lib_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute koja");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    let _ = fs::remove_file(&lib_path);

    assert!(
        code == 0,
        "ffi: expected exit code 0, got {code}\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );

    let expected = fs::read_to_string(dir.join("expected.stdout")).unwrap();
    if stdout != expected {
        let diff = diff_lines(&stdout, &expected);
        panic!("\n--- FAIL: ffi ---\n{diff}");
    }
}

/// Lifecycle signal delivery under both backends: the compiled binary
/// receives SIGTERM directly; under `koja run --backend=interpreter` the
/// entry process runs in-process, so the signal lands on the
/// interpreter's latched handlers. Both must produce the same stdout and
/// exit code.
#[test]
fn lang_process_lifecycle() {
    let dir = lang_dir().join("process_lifecycle");
    run_signal_test(&dir, "process_lifecycle", libc::SIGTERM);
    run_signal_test_interpreted(&dir, "process_lifecycle", libc::SIGTERM);
}

/// RUNTIME-GAPS #3: a process blocked in a synchronous `accept` must wake
/// when a lifecycle signal arrives. Mirrors `lang_process_lifecycle` but
/// the entry process is parked in I/O (`WaitingIO`), not `receive`.
#[test]
fn lang_process_io_signal() {
    let dir = lang_dir().join("process_io_signal");
    run_signal_test(&dir, "process_io_signal", libc::SIGTERM);
    run_signal_test_interpreted(&dir, "process_io_signal", libc::SIGTERM);
}

/// Regression for `IOReady` union-message delivery under both backends:
/// the fixture watches STDIN and must receive the reactor's readiness
/// event through its `handle` (tag-2 dispatch) instead of trapping.
/// Pre-filled STDIN makes the fd readable immediately. The interpreter
/// now drives its own cooperative reactor (see `koja-ir-eval/src/reactor`),
/// so the watch -> `IOReady` path holds there too.
#[test]
fn lang_process_io() {
    for backend in BACKENDS {
        run_process_io(backend);
    }
}

fn run_process_io(backend: &str) {
    use std::io::Write;

    let dir = lang_dir().join("process_io");
    assert!(dir.exists(), "test fixture process_io/ not found");
    let expected = fs::read_to_string(dir.join("expected.stdout")).unwrap();

    let backend_flag = format!("--backend={backend}");
    let mut cmd = Command::new(koja_bin());
    cmd.arg("run").arg(&backend_flag).current_dir(&dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", &lib_path);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to execute koja");
    // Bytes wait in the pipe (closing the write end leaves it at readable
    // EOF), so the fd is ready by the time the process watches it.
    child
        .stdin
        .take()
        .expect("child stdin already taken")
        .write_all(b"go\n")
        .expect("failed to pre-fill stdin");

    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "process_io ({backend}): timed out after {}s",
                    TEST_TIMEOUT.as_secs()
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("process_io ({backend}): wait error: {e}"),
        }
    }

    let output = child.wait_with_output().expect("failed to collect output");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    assert!(
        code == 1,
        "process_io ({backend}): expected exit code 1 (StopReason.Shutdown), got {code}\nstderr:\n{stderr}"
    );
    if stdout != expected {
        panic!(
            "\n--- FAIL: process_io ({backend}) ---\n{}",
            diff_lines(&stdout, &expected)
        );
    }
}

// ---------------------------------------------------------------------------
// Signal test runner
// ---------------------------------------------------------------------------

fn run_signal_test(dir: &Path, label: &str, signal: libc::c_int) {
    assert!(dir.exists(), "test fixture {label}/ not found");

    let binary = dir.join("build").join("debug").join(label);

    let build_out = {
        let mut cmd = Command::new(koja_bin());
        cmd.arg("build")
            .arg("-o")
            .arg(binary.to_str().unwrap())
            .current_dir(dir);
        if let Some(lib_path) = library_path() {
            cmd.env("LIBRARY_PATH", &lib_path);
        }
        cmd.output().expect("failed to build")
    };
    assert!(
        build_out.status.success(),
        "koja build failed for {label}:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    let child = Command::new(&binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run compiled binary");

    signal_child_and_assert(dir, label, signal, child);
}

/// Like [`run_signal_test`] but executes the fixture via
/// `koja run --backend=interpreter`. The interpreter runs the entry
/// process inside the `koja` process itself, so the signal goes to the
/// spawned `koja` pid directly.
fn run_signal_test_interpreted(dir: &Path, label: &str, signal: libc::c_int) {
    assert!(dir.exists(), "test fixture {label}/ not found");

    let mut cmd = Command::new(koja_bin());
    cmd.arg("run").arg("--backend=interpreter").current_dir(dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", &lib_path);
    }
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run koja");

    signal_child_and_assert(dir, label, signal, child);
}

/// Shared back half of the signal tests: wait for the fixture's ready
/// line, deliver `signal`, then assert stdout and exit code against the
/// fixture's expectations.
fn signal_child_and_assert(
    dir: &Path,
    label: &str,
    signal: libc::c_int,
    mut child: std::process::Child,
) {
    let expected_path = dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing {label}/expected.stdout");

    let pid = child.id() as libc::pid_t;

    // Wait for the runtime to print its first ready-line before
    // signalling. The first line of `expected.stdout` doubles as a
    // ready handshake: the process prints it only after its
    // lifecycle/signal handlers are installed, so this is more
    // robust under parallel-test load than a fixed sleep, where a
    // 500 ms grace can fire SIGTERM before the runtime has finished
    // wiring up its handlers and the kernel kills the process
    // outright (exit -1, empty stdout).
    let expected_text = fs::read_to_string(&expected_path).unwrap();
    let ready_line = expected_text
        .lines()
        .next()
        .expect("expected.stdout must contain at least one ready line");
    let mut early_stdout =
        wait_for_ready_line(&mut child, ready_line, Duration::from_secs(10), label);

    unsafe {
        libc::kill(pid, signal);
    }

    let output = child.wait_with_output().expect("failed to collect output");
    early_stdout.extend_from_slice(&output.stdout);
    let stdout = String::from_utf8_lossy(&early_stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    let expected_code = if dir.join("expected.exit_code").exists() {
        fs::read_to_string(dir.join("expected.exit_code"))
            .unwrap()
            .trim()
            .parse::<i32>()
            .expect("expected.exit_code must be an integer")
    } else {
        0
    };

    assert!(
        code == expected_code,
        "{label}: expected exit code {expected_code}, got {code}\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );

    if stdout != expected_text {
        let diff = diff_lines(&stdout, &expected_text);
        panic!("\n--- FAIL: {label} ---\n{diff}");
    }
}

/// Reads `child`'s stdout on a background thread until either a line
/// matching `ready_line` arrives or `timeout` elapses. Returns the
/// bytes read so far so the caller can prepend them to whatever
/// `wait_with_output()` collects after the signal lands. Panics on
/// timeout — a runtime that never prints its ready line is broken.
fn wait_for_ready_line(
    child: &mut std::process::Child,
    ready_line: &str,
    timeout: Duration,
    label: &str,
) -> Vec<u8> {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;

    let stdout = child.stdout.take().expect("child stdout already taken");
    let target = ready_line.to_string();
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();

    let reader_handle = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut accumulated = Vec::new();
        let outcome = loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    break Err(format!(
                        "child closed stdout before printing ready line `{target}`; got:\n{}",
                        String::from_utf8_lossy(&accumulated),
                    ));
                }
                Ok(_) => {
                    accumulated.extend_from_slice(line.as_bytes());
                    if line.trim_end_matches('\n') == target {
                        break Ok(());
                    }
                }
                Err(err) => break Err(format!("read_line failed: {err}")),
            }
        };
        // BufReader::into_inner drops anything already pulled into
        // its internal buffer but not yet returned via read_line.
        // Capture that tail so we don't lose lines a fast child
        // printed back-to-back with the ready line.
        accumulated.extend_from_slice(reader.buffer());
        let stdout = reader.into_inner();
        let payload = match outcome {
            Ok(()) => Ok(accumulated),
            Err(msg) => Err(msg),
        };
        let _ = tx.send(payload);
        stdout
    });

    let result = rx.recv_timeout(timeout);
    let stdout = reader_handle
        .join()
        .expect("ready-line reader thread panicked");
    // Re-attach stdout so `wait_with_output` can collect the rest.
    child.stdout = Some(stdout);

    match result {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(msg)) => {
            let _ = child.kill();
            panic!("{label}: {msg}");
        }
        Err(_) => {
            let _ = child.kill();
            panic!("{label}: timed out after {timeout:?} waiting for ready line `{ready_line}`");
        }
    }
}

// ---------------------------------------------------------------------------
// Standalone project-specific tests (build, check, release)
// ---------------------------------------------------------------------------

#[test]
fn lang_project_build_test() {
    let project_dir = lang_dir().join("project");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/project/ not found");
    }

    let binary_path = std::env::temp_dir().join("koja_test_project_build");
    let _ = std::fs::remove_file(&binary_path);

    let mut cmd = Command::new(koja_bin());
    cmd.arg("build")
        .arg("-o")
        .arg(binary_path.to_str().unwrap())
        .current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute koja build");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "koja build failed in project dir\nstderr:\n{stderr}"
    );
    assert!(
        binary_path.exists(),
        "expected binary at {}",
        binary_path.display()
    );
    let _ = std::fs::remove_file(&binary_path);
}

#[test]
fn lang_project_check_test() {
    let project_dir = lang_dir().join("project");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/project/ not found");
    }

    let mut cmd = Command::new(koja_bin());
    cmd.arg("check").current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute koja check");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "koja check failed in project dir\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Project: OK"),
        "expected 'Project: OK' in stdout, got: {stdout}"
    );
}

/// Locks in the duplicate-package-name rule: a project that names itself
/// `Greeter` and also depends on a `Greeter` package must fail to build with
/// a clear error message. Same rule catches `name = "Global"` collisions and
/// duplicate transitive deps.
#[test]
fn lang_dup_pkg_name() {
    let dir = lang_dir().join("dup_pkg_name");
    assert!(dir.exists(), "test fixture dup_pkg_name/ not found");

    let mut cmd = Command::new(koja_bin());
    cmd.arg("check").current_dir(&dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute koja check");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        !output.status.success(),
        "expected koja check to fail for duplicate package name, but it succeeded\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("duplicate package name `Greeter`"),
        "expected stderr to mention duplicate package name `Greeter`, got:\n{stderr}"
    );
}

#[test]
fn lang_release_build_test() {
    let project_dir = lang_dir().join("project");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/project/ not found");
    }

    let binary_path = std::env::temp_dir().join("koja_test_release_build");
    let _ = std::fs::remove_file(&binary_path);

    let mut cmd = Command::new(koja_bin());
    cmd.arg("build")
        .arg("--release")
        .arg("-o")
        .arg(binary_path.to_str().unwrap())
        .current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd
        .output()
        .expect("failed to execute koja build --release");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "koja build --release failed\nstderr:\n{stderr}"
    );
    assert!(
        binary_path.exists(),
        "expected release binary at {}",
        binary_path.display()
    );

    let run_output = Command::new(&binary_path)
        .output()
        .expect("failed to run release binary");
    assert!(
        run_output.status.success(),
        "release binary exited with {:?}",
        run_output.status.code()
    );

    let _ = std::fs::remove_file(&binary_path);
}

/// Run `koja test <extra args>` in the `test_trace` fixture and return
/// `(stdout, stderr, exit_code)`.
fn run_koja_test_trace(extra_args: &[&str]) -> (String, String, i32) {
    let project_dir = lang_dir().join("test_trace");
    assert!(
        project_dir.exists(),
        "test fixture tests/lang/test_trace/ not found"
    );

    let mut cmd = Command::new(koja_bin());
    cmd.arg("test").args(extra_args).current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    // The colored path is gated on NO_COLOR being unset; clear it so the
    // assertions are stable regardless of the surrounding environment.
    cmd.env_remove("NO_COLOR");

    let output = cmd.output().expect("failed to execute koja test");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

/// `koja test --trace` groups by struct, prints each test's name with
/// its `path:line` plus same-line result + timing, honors `--no-color`,
/// and (with color) rewrites each completed line whole in the result
/// color. Both invocations live in one test because they share the
/// fixture's build dir/binary path; running them as separate `#[test]`s
/// would race under the parallel harness.
#[test]
fn lang_test_trace() {
    // No-color: clean appended output, no ANSI escapes.
    let (stdout, stderr, code) = run_koja_test_trace(&["--trace", "--no-color"]);
    assert_eq!(
        code, 0,
        "expected all fixture tests to pass\nstderr:\n{stderr}"
    );
    for needle in [
        "AlphaTest",
        "BetaTest",
        "first alpha test (test/alpha_test.koja:",
        "beta passes (test/beta_test.koja:",
        "... ok (",
        "ms)",
    ] {
        assert!(
            stdout.contains(needle),
            "expected trace stdout to contain {needle:?}, got:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains('\u{1b}'),
        "expected --no-color to strip ANSI escapes, got:\n{stdout}"
    );

    // Color: each completed line is rewritten whole in green via a
    // leading CR (the uncolored pre-run name stays as the crash anchor).
    let (stdout, stderr, code) = run_koja_test_trace(&["--trace"]);
    assert_eq!(
        code, 0,
        "expected all fixture tests to pass\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("\r\u{1b}[32m  first alpha test (test/alpha_test.koja:"),
        "expected a carriage-return whole-line green rewrite, got:\n{stdout:?}"
    );
}
