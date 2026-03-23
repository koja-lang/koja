use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

    let mut failures = Vec::new();

    for file in &files {
        let test_name = file.file_stem().unwrap().to_string_lossy().to_string();
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
            failures.push(format!("{test_name}: output mismatch\n{diff}"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "\n{} test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}

#[test]
fn lang_import_tests() {
    let dir = lang_dir().join("imports");
    if !dir.exists() {
        return;
    }

    let main_file = dir.join("main.expo");
    if !main_file.exists() {
        return;
    }

    let expected_path = dir.join("main.stdout");
    assert!(expected_path.exists(), "missing imports/main.stdout");

    let (stdout, stderr, code) = run_expo(&main_file);
    assert!(
        code == 0,
        "imports/main.expo failed with code {code}\nstderr:\n{stderr}"
    );
    assert_output_matches("imports/main", &stdout, &expected_path);
}

#[test]
fn lang_compile_fail_tests() {
    let dir = lang_dir().join("compile_fail");
    if !dir.exists() {
        return;
    }

    let files = collect_expo_files(&dir);
    let mut failures = Vec::new();

    for file in &files {
        let test_name = format!(
            "compile_fail/{}",
            file.file_stem().unwrap().to_string_lossy()
        );
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
            "\n{} compile_fail test(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n---\n")
        );
    }
}
