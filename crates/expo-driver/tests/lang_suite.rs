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

fn expo_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_expo"))
}

fn collect_expo_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("failed to read test dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "expo") {
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

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn run_expo(file: &Path) -> (String, String, i32) {
    run_with_timeout(|cmd| {
        cmd.arg("run").arg(file);
    })
}

fn run_with_timeout(configure: impl FnOnce(&mut Command)) -> (String, String, i32) {
    let mut cmd = Command::new(expo_bin());
    configure(&mut cmd);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", &lib_path);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to execute expo");
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (
                    String::new(),
                    "test timed out (killed after 30s)".to_string(),
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
    let files = collect_expo_files(dir);
    assert!(
        !files.is_empty(),
        "no .expo test files found in {label} ({})",
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

        let (stdout, stderr, code) = run_expo(file);

        if code != 0 {
            failures.push(format!(
                "{test_name}: exited with code {code}\nstderr:\n{stderr}"
            ));
            continue;
        }

        let expected = fs::read_to_string(&expected_path).unwrap();
        if stdout != expected {
            let diff = diff_lines(&stdout, &expected);
            failures.push(format!("{test_name}: output mismatch\n{diff}"));
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

fn run_compile_fail_dir(dir: &Path, label: &str) {
    if !dir.exists() {
        return;
    }

    let files = collect_expo_files(dir);
    let mut failures = Vec::new();

    for file in &files {
        let test_name = format!("{label}/{}", file.file_stem().unwrap().to_string_lossy());
        let expected_path = file.with_extension("stdout");

        if !expected_path.exists() {
            failures.push(format!("{test_name}: missing .stdout file"));
            continue;
        }

        let (_stdout, stderr, code) = run_expo(file);

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

fn run_runtime_fail_dir(dir: &Path, label: &str) {
    if !dir.exists() {
        return;
    }

    let files = collect_expo_files(dir);
    let mut failures = Vec::new();

    for file in &files {
        let test_name = format!("{label}/{}", file.file_stem().unwrap().to_string_lossy());
        let expected_path = file.with_extension("stderr");

        if !expected_path.exists() {
            failures.push(format!("{test_name}: missing .stderr file"));
            continue;
        }

        let (_stdout, stderr, code) = run_expo(file);

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

fn run_project_dir_with(dir: &Path, label: &str, extra_args: &[&str]) {
    assert!(dir.exists(), "test fixture {label}/ not found");

    let expected_path = dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing {label}/expected.stdout");

    let dir_owned = dir.to_path_buf();
    let extra: Vec<String> = extra_args.iter().map(|s| s.to_string()).collect();
    let (stdout, stderr, code) = run_with_timeout(|cmd| {
        cmd.arg("run").current_dir(&dir_owned);
        if !extra.is_empty() {
            cmd.arg("--");
            for a in &extra {
                cmd.arg(a);
            }
        }
    });

    let expected_code = dir
        .join("expected.exit_code")
        .exists()
        .then(|| {
            fs::read_to_string(dir.join("expected.exit_code"))
                .unwrap()
                .trim()
                .parse::<i32>()
                .expect("expected.exit_code must be an integer")
        })
        .unwrap_or(0);

    assert!(
        code == expected_code,
        "expo run in {label}: expected exit code {expected_code}, got {code}\nstderr:\n{stderr}"
    );

    let expected = fs::read_to_string(&expected_path).unwrap();
    if stdout != expected {
        let diff = diff_lines(&stdout, &expected);
        panic!("\n--- FAIL: {label} ---\n{diff}");
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
lang_test_dir!(lang_control_flow, "control_flow");
lang_test_dir!(lang_functions, "functions");
lang_test_dir!(lang_types, "types");
lang_test_dir!(lang_generics, "generics");
lang_test_dir!(lang_protocols, "protocols");
lang_test_dir!(lang_collections, "collections");
lang_test_dir!(lang_ownership, "ownership");
lang_test_dir!(lang_binary, "binary");
lang_test_dir!(lang_stdlib, "stdlib");
lang_test_dir!(lang_io, "io");

// Failure tests
lang_test_dir!(lang_compile_fail, "compile_fail", compile_fail);
lang_test_dir!(lang_runtime_fail, "runtime_fail", runtime_fail);

// Multi-file project tests
lang_test_dir!(lang_project, "project", project);
lang_test_dir!(lang_diamond, "diamond", project);
lang_test_dir!(lang_cross_ref, "cross_ref", project);
lang_test_dir!(lang_local_dep, "local_dep", project);
lang_test_dir!(lang_alias_dep, "alias_dep", project);
lang_test_dir!(lang_process_entry, "process_entry", project);
lang_test_dir!(lang_process_exit, "process_exit", project);
lang_test_dir!(lang_process_argv, "process_argv", project, "hello", "world");

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
    let output = Command::new(expo_bin())
        .arg("run")
        .current_dir(&dir)
        .env("LIBRARY_PATH", &ffi_lib_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute expo");
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

#[test]
fn lang_process_lifecycle() {
    run_signal_test(
        &lang_dir().join("process_lifecycle"),
        "process_lifecycle",
        libc::SIGTERM,
    );
}

// ---------------------------------------------------------------------------
// Signal test runner
// ---------------------------------------------------------------------------

fn run_signal_test(dir: &Path, label: &str, signal: libc::c_int) {
    assert!(dir.exists(), "test fixture {label}/ not found");

    let expected_path = dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing {label}/expected.stdout");

    let binary = dir.join("target").join("debug").join(label);

    let build_out = {
        let mut cmd = Command::new(expo_bin());
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
        "expo build failed for {label}:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    let child = Command::new(&binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run compiled binary");

    let pid = child.id() as libc::pid_t;
    std::thread::sleep(Duration::from_millis(500));

    unsafe {
        libc::kill(pid, signal);
    }

    let output = child.wait_with_output().expect("failed to collect output");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    let expected_code = dir
        .join("expected.exit_code")
        .exists()
        .then(|| {
            fs::read_to_string(dir.join("expected.exit_code"))
                .unwrap()
                .trim()
                .parse::<i32>()
                .expect("expected.exit_code must be an integer")
        })
        .unwrap_or(0);

    assert!(
        code == expected_code,
        "{label}: expected exit code {expected_code}, got {code}\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );

    let expected = fs::read_to_string(&expected_path).unwrap();
    if stdout != expected {
        let diff = diff_lines(&stdout, &expected);
        panic!("\n--- FAIL: {label} ---\n{diff}");
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

    let binary_path = std::env::temp_dir().join("expo_test_project_build");
    let _ = std::fs::remove_file(&binary_path);

    let mut cmd = Command::new(expo_bin());
    cmd.arg("build")
        .arg("-o")
        .arg(binary_path.to_str().unwrap())
        .current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute expo build");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expo build failed in project dir\nstderr:\n{stderr}"
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

    let mut cmd = Command::new(expo_bin());
    cmd.arg("check").current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute expo check");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expo check failed in project dir\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("test_project: OK"),
        "expected 'test_project: OK' in stdout, got: {stdout}"
    );
}

#[test]
fn lang_release_build_test() {
    let project_dir = lang_dir().join("project");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/project/ not found");
    }

    let binary_path = std::env::temp_dir().join("expo_test_release_build");
    let _ = std::fs::remove_file(&binary_path);

    let mut cmd = Command::new(expo_bin());
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
        .expect("failed to execute expo build --release");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expo build --release failed\nstderr:\n{stderr}"
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
