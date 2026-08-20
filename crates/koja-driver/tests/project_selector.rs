use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn koja_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koja"))
}

fn lang_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lang")
        .join(name)
}

fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "koja-project-selector-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn run_from(cwd: &Path, args: &[&str]) -> Output {
    Command::new(koja_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run koja")
}

fn assert_success(output: &Output, command: &str) {
    assert!(
        output.status.success(),
        "{command} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn selector_runs_and_checks_project_from_another_directory() {
    let cwd = temp_dir("run-check");
    let project = lang_fixture("project");
    let project_arg = project.to_str().unwrap();

    let run = run_from(&cwd, &["run", "-S", project_arg]);
    assert_success(&run, "koja run -S");
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        fs::read_to_string(project.join("expected.stdout")).unwrap()
    );

    let check = run_from(&cwd, &["check", "--project", project_arg]);
    assert_success(&check, "koja check --project");
    assert!(String::from_utf8_lossy(&check.stdout).contains("project: OK"));

    fs::remove_dir_all(cwd).ok();
}

#[test]
fn selector_supports_project_wide_commands() {
    let cwd = temp_dir("commands");
    let project = lang_fixture("project");
    let project_arg = project.to_str().unwrap();

    for (label, args) in [
        ("tasks", vec!["tasks", "-S", project_arg]),
        ("deps", vec!["deps", "-S", project_arg]),
        ("format", vec!["format", "--check", "-S", project_arg]),
    ] {
        let output = run_from(&cwd, &args);
        assert_success(&output, &format!("koja {label} -S"));
    }

    let doc_dir = cwd.join("doc-output");
    let doc_arg = doc_dir.to_str().unwrap();
    let doc = run_from(&cwd, &["doc", "-S", project_arg, "--output", doc_arg]);
    assert_success(&doc, "koja doc -S");
    assert!(doc_dir.join("index.html").is_file());

    let test_project = lang_fixture("test_trace");
    let tests = run_from(&cwd, &["test", "-S", test_project.to_str().unwrap()]);
    assert_success(&tests, "koja test -S");

    fs::remove_dir_all(cwd).ok();
}

#[test]
fn selector_loads_project_shell_session() {
    let cwd = temp_dir("shell");
    let project = lang_fixture("project");
    let mut child = Command::new(koja_bin())
        .args(["shell", "-S", project.to_str().unwrap()])
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start koja shell");
    child.stdin.as_mut().unwrap().write_all(b":quit\n").unwrap();
    let output = child
        .wait_with_output()
        .expect("failed to wait for koja shell");
    assert_success(&output, "koja shell -S");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("loading project `project`"),
        "shell did not load selected project:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    fs::remove_dir_all(cwd).ok();
}

#[test]
fn selector_does_not_change_program_working_directory() {
    let root = temp_dir("cwd");
    let project = root.join("project");
    let caller = root.join("caller");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(&caller).unwrap();
    fs::write(
        project.join("koja.toml"),
        "[project]\nname = \"selected\"\nversion = \"0.1.0\"\nentry = \"App\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/app.koja"),
        r#"alias Process.Step
alias Process.StopReason

struct App: Process<(), (), ()>
  fn start(config: ()) -> Self ! StopReason
    App{}
  end

  fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
    Step.Continue(self)
  end

  fn run(self) -> StopReason
    File.write("selector-marker.txt", "ok").unwrap()
    StopReason.Normal
  end
end
"#,
    )
    .unwrap();

    let output = run_from(&caller, &["run", "-S", "../project"]);
    assert_success(&output, "koja run -S");
    assert!(caller.join("selector-marker.txt").is_file());
    assert!(!project.join("selector-marker.txt").exists());

    let docs = run_from(&caller, &["doc", "-S", "../project", "--project-only"]);
    assert_success(&docs, "koja doc -S");
    assert!(project.join("doc/index.html").is_file());
    assert!(!caller.join("doc").exists());

    fs::remove_dir_all(root).ok();
}

#[test]
fn selector_reports_invalid_and_unused_paths() {
    let cwd = temp_dir("errors");
    let missing = cwd.join("missing");
    let invalid = run_from(&cwd, &["run", "-S", missing.to_str().unwrap()]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("cannot resolve project directory"));

    let no_manifest = cwd.join("no-manifest");
    fs::create_dir_all(&no_manifest).unwrap();
    let invalid = run_from(&cwd, &["run", "-S", no_manifest.to_str().unwrap()]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("no `koja.toml` found"));

    let project = lang_fixture("project");
    let script = lang_fixture("basics/script_early_return.kojs");
    let unused = run_from(
        &cwd,
        &[
            "run",
            "-S",
            project.to_str().unwrap(),
            script.to_str().unwrap(),
        ],
    );
    assert!(!unused.status.success());
    assert!(
        String::from_utf8_lossy(&unused.stderr)
            .contains("cannot be used with an explicit source file")
    );

    fs::remove_dir_all(cwd).ok();
}
