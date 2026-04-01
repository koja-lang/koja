use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(4)
}

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

fn run_expo(file: &Path) -> (String, String, i32) {
    let mut cmd = Command::new(expo_bin());
    cmd.arg("run").arg(file);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute expo");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

fn assert_output_matches(test_name: &str, actual: &str, expected_path: &Path) {
    let expected = fs::read_to_string(expected_path)
        .unwrap_or_else(|_| panic!("missing expected output file: {}", expected_path.display()));

    if actual != expected {
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

        panic!(
            "\n--- FAIL: {test_name} ---\n\
             expected ({} lines):\n{expected}\n\
             actual ({} lines):\n{actual}\n\
             diff:\n{diff}",
            expected_lines.len(),
            actual_lines.len(),
        );
    }
}

#[test]
fn lang_tests() {
    let dir = lang_dir();
    let files = collect_expo_files(&dir);

    assert!(
        !files.is_empty(),
        "no .expo test files found in {}",
        dir.display()
    );

    let queue = Mutex::new(VecDeque::from_iter(files.iter()));
    let failures = Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..worker_count() {
            s.spawn(|| {
                loop {
                    let file = queue.lock().unwrap().pop_front();
                    let Some(file) = file else { break };

                    let test_name = file.file_stem().unwrap().to_string_lossy().to_string();
                    let expected_path = file.with_extension("stdout");

                    if !expected_path.exists() {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{test_name}: missing .stdout file"));
                        continue;
                    }

                    let (stdout, stderr, code) = run_expo(file);

                    if code != 0 {
                        failures.lock().unwrap().push(format!(
                            "{test_name}: exited with code {code}\nstderr:\n{stderr}"
                        ));
                        continue;
                    }

                    let expected = fs::read_to_string(&expected_path).unwrap();
                    if stdout != expected {
                        let actual_lines: Vec<&str> = stdout.lines().collect();
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
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{test_name}: output mismatch\n{diff}"));
                    }
                }
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        panic!(
            "\n{} test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}

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
fn lang_project_run_test() {
    let project_dir = lang_dir().join("project");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/project/ not found");
    }

    let expected_path = project_dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing project/expected.stdout");

    let mut cmd = Command::new(expo_bin());
    cmd.arg("run").current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute expo run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    assert!(
        code == 0,
        "expo run failed in project dir with code {code}\nstderr:\n{stderr}"
    );
    assert_output_matches("project/run", &stdout, &expected_path);
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
fn lang_diamond_import_test() {
    let project_dir = lang_dir().join("diamond");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/diamond/ not found");
    }

    let expected_path = project_dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing diamond/expected.stdout");

    let mut cmd = Command::new(expo_bin());
    cmd.arg("run").current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute expo run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    assert!(
        code == 0,
        "expo run failed in diamond dir with code {code}\nstderr:\n{stderr}"
    );
    assert_output_matches("diamond/run", &stdout, &expected_path);
}

#[test]
fn lang_cross_ref_test() {
    let project_dir = lang_dir().join("cross_ref");
    if !project_dir.exists() {
        panic!("test fixture tests/lang/cross_ref/ not found");
    }

    let expected_path = project_dir.join("expected.stdout");
    assert!(expected_path.exists(), "missing cross_ref/expected.stdout");

    let mut cmd = Command::new(expo_bin());
    cmd.arg("run").current_dir(&project_dir);
    if let Some(lib_path) = library_path() {
        cmd.env("LIBRARY_PATH", lib_path);
    }
    let output = cmd.output().expect("failed to execute expo run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    assert!(
        code == 0,
        "expo run failed in cross_ref dir with code {code}\nstderr:\n{stderr}"
    );
    assert_output_matches("cross_ref/run", &stdout, &expected_path);
}

#[test]
fn lang_compile_fail_tests() {
    let dir = lang_dir().join("compile_fail");
    if !dir.exists() {
        return;
    }

    let files = collect_expo_files(&dir);
    let queue = Mutex::new(VecDeque::from_iter(files.iter()));
    let failures = Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..worker_count() {
            s.spawn(|| {
                loop {
                    let file = queue.lock().unwrap().pop_front();
                    let Some(file) = file else { break };

                    let test_name = format!(
                        "compile_fail/{}",
                        file.file_stem().unwrap().to_string_lossy()
                    );
                    let expected_path = file.with_extension("stdout");

                    if !expected_path.exists() {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{test_name}: missing .stdout file"));
                        continue;
                    }

                    let (_stdout, stderr, code) = run_expo(file);

                    if code == 0 {
                        failures.lock().unwrap().push(format!(
                            "{test_name}: expected compilation failure but succeeded"
                        ));
                        continue;
                    }

                    let expected = fs::read_to_string(&expected_path).unwrap();
                    let pattern = expected.trim();
                    if !stderr.contains(pattern) {
                        failures.lock().unwrap().push(format!(
                            "{test_name}: stderr does not contain expected pattern\n\
                         expected pattern: {pattern:?}\n\
                         actual stderr:\n{stderr}"
                        ));
                    }
                }
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        panic!(
            "\n{} compile_fail test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}
