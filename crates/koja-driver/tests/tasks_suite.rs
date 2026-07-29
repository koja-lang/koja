//! Integration tests for custom CLI tasks: `[tasks]` manifest
//! validation, `koja tasks` listing, and `koja run <task.name>`
//! dispatch for project-, dependency-, and toolchain-provided tasks
//! (including the self-hosted `koja.new` behind `koja new`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn koja_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koja"))
}

/// Per-test temp project dir, removed on drop.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "koja-tasks-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn koja(&self, args: &[&str]) -> Output {
        Command::new(koja_bin())
            .args(args)
            .current_dir(&self.root)
            .output()
            .expect("failed to run koja")
    }

    fn koja_ok(&self, args: &[&str]) -> String {
        let output = self.koja(args);
        assert!(
            output.status.success(),
            "koja {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn koja_err(&self, args: &[&str]) -> String {
        let output = self.koja(args);
        assert!(
            !output.status.success(),
            "koja {args:?} unexpectedly succeeded:\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).ok();
    }
}

const ENTRY: &str = "alias Process.Step
alias Process.StopReason

struct App
end

enum AppMsg
  Go
end

impl Process<App, AppMsg, String> for App
  fn start(config: App) -> Result<Self, StopReason>
    Result.Ok(config)
  end

  fn handle(self, msg: AppMsg, from: Option<ReplyTo<String>>) -> Step<Self>
    Step.Continue(self)
  end

  fn run(self) -> StopReason
    StopReason.Normal
  end
end
";

/// A `Koja.Task` impl that echoes its first argument, fails on
/// `boom`, and prints a default line with no args.
fn task_source(type_name: &str, greeting: &str) -> String {
    format!(
        "struct {type_name}\nend\n\n\
         impl Koja.Task for {type_name}\n  \
           fn run(args: List<String>) -> Result<(), String>\n    \
             match args.get(0)\n      \
               Option.Some(first) when first == \"boom\" -> Result.Err(\"asked to fail\")\n      \
               Option.Some(first) ->\n        \
                 IO.puts(\"{greeting} #{{first}}\")\n        \
                 Result.Ok(())\n      \
               Option.None ->\n        \
                 IO.puts(\"{greeting}\")\n        \
                 Result.Ok(())\n    \
             end\n  \
           end\n\
         end\n"
    )
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Scaffold a project named `myapp` exporting `myapp.greet`.
fn scaffold_project(fx: &Fixture, tasks_table: &str) {
    write(
        &fx.root.join("koja.toml"),
        &format!(
            "[project]\nname = \"myapp\"\nversion = \"0.1.0\"\nentry = \"App\"\n\n{tasks_table}"
        ),
    );
    write(&fx.root.join("src/app.koja"), ENTRY);
    write(
        &fx.root.join("src/greet.koja"),
        &task_source("Greet", "hello"),
    );
}

#[test]
fn project_task_runs_with_args_and_error_exit() {
    let fx = Fixture::new("run");
    scaffold_project(&fx, "[tasks]\n\"myapp.greet\" = \"Greet\"\n");

    let stdout = fx.koja_ok(&["run", "myapp.greet"]);
    assert_eq!(stdout.trim(), "hello");

    let stdout = fx.koja_ok(&["run", "myapp.greet", "--", "world"]);
    assert_eq!(stdout.trim(), "hello world");

    let stderr = fx.koja_err(&["run", "myapp.greet", "--", "boom"]);
    assert!(
        stderr.contains("error: asked to fail"),
        "Err should reach stderr: {stderr}"
    );
}

#[test]
fn dependency_task_is_listed_and_runs() {
    let fx = Fixture::new("dep");
    write(
        &fx.root.join("koja.toml"),
        "[project]\nname = \"myapp\"\nversion = \"0.1.0\"\nentry = \"App\"\n\n\
         [dependencies]\ntooling = { path = \"libs/tooling\" }\n\n\
         [tasks]\n\"myapp.greet\" = \"Greet\"\n",
    );
    write(&fx.root.join("src/app.koja"), ENTRY);
    write(
        &fx.root.join("src/greet.koja"),
        &task_source("Greet", "hello"),
    );
    write(
        &fx.root.join("libs/tooling/koja.toml"),
        "[project]\nname = \"tooling\"\nversion = \"0.1.0\"\n\n\
         [tasks]\n\"tooling.lint\" = \"Lint\"\n",
    );
    write(
        &fx.root.join("libs/tooling/src/lint.koja"),
        &task_source("Lint", "linting"),
    );

    let listing = fx.koja_ok(&["tasks"]);
    assert!(listing.contains("myapp.greet"), "listing: {listing}");
    assert!(listing.contains("tooling.lint"), "listing: {listing}");

    let stdout = fx.koja_ok(&["run", "tooling.lint", "--", "fast"]);
    assert_eq!(stdout.trim(), "linting fast");
}

#[test]
fn unknown_task_points_at_the_listing() {
    let fx = Fixture::new("unknown");
    scaffold_project(&fx, "[tasks]\n\"myapp.greet\" = \"Greet\"\n");

    let stderr = fx.koja_err(&["run", "myapp.missing"]);
    assert!(
        stderr.contains("no task named `myapp.missing`") && stderr.contains("koja tasks"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn task_name_must_carry_the_package_prefix() {
    let fx = Fixture::new("prefix");
    scaffold_project(&fx, "[tasks]\n\"greet\" = \"Greet\"\n");

    let stderr = fx.koja_err(&["tasks"]);
    assert!(
        stderr.contains("must be named `myapp.<task>`"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn task_type_must_exist_and_implement_the_protocol() {
    let fx = Fixture::new("conform");
    scaffold_project(&fx, "[tasks]\n\"myapp.greet\" = \"Nope\"\n");
    let stderr = fx.koja_err(&["run", "myapp.greet"]);
    assert!(
        stderr.contains("type `Nope`, which does not exist"),
        "unexpected stderr: {stderr}"
    );

    scaffold_project(&fx, "[tasks]\n\"myapp.greet\" = \"App\"\n");
    let stderr = fx.koja_err(&["run", "myapp.greet"]);
    assert!(
        stderr.contains("does not implement `Koja.Task`"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn listing_always_offers_the_toolchain_tasks() {
    let fx = Fixture::new("empty");
    scaffold_project(&fx, "");

    let stdout = fx.koja_ok(&["tasks"]);
    assert_eq!(stdout.trim(), "koja.new  New");
}

#[test]
fn tasks_and_task_runs_work_without_a_project() {
    let fx = Fixture::new("projectless");

    let listing = fx.koja_ok(&["tasks"]);
    assert!(listing.contains("koja.new"), "listing: {listing}");

    fx.koja_ok(&["run", "koja.new", "--", "my_app"]);
    assert!(fx.root.join("my_app/koja.toml").exists());
}

/// The minor-version stamp `koja new` writes into `koja.toml`,
/// mirroring the scaffold's "minimum compiler = the one that
/// generated it" rule.
fn toolchain_minor() -> &'static str {
    env!("CARGO_PKG_VERSION")
        .rsplit_once('.')
        .map_or(env!("CARGO_PKG_VERSION"), |(minor, _)| minor)
}

fn read_scaffold_file(fx: &Fixture, relative: &str) -> String {
    fs::read_to_string(fx.root.join("my_app").join(relative))
        .unwrap_or_else(|e| panic!("cannot read scaffolded {relative}: {e}"))
}

#[test]
fn new_scaffolds_a_complete_working_project() {
    let fx = Fixture::new("new");
    let stdout = fx.koja_ok(&["new", "my_app"]);
    assert_eq!(stdout.trim(), "created project 'my_app'");

    let toml = read_scaffold_file(&fx, "koja.toml");
    assert_eq!(
        toml,
        format!(
            "[project]\nentry = \"App\"\nkoja = \"{}\"\nname = \"my_app\"\nversion = \"0.1.0\"\n",
            toolchain_minor()
        )
    );
    assert_eq!(read_scaffold_file(&fx, ".gitignore"), "/build\n/deps\n");

    let app = read_scaffold_file(&fx, "src/app.koja");
    assert!(app.contains("impl Process<(), (), ()> for App"), "{app}");
    assert!(app.contains("\"Hello, #{name}!\""), "{app}");

    let app_test = read_scaffold_file(&fx, "test/app_test.koja");
    assert!(app_test.contains("@test \"greet builds a greeting message\""));

    // The scaffold typechecks from the first command.
    let output = Command::new(koja_bin())
        .arg("check")
        .current_dir(fx.root.join("my_app"))
        .output()
        .expect("failed to run koja check");
    assert!(
        output.status.success(),
        "koja check failed in scaffold:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn new_rejects_bad_names_and_existing_directories() {
    let fx = Fixture::new("new-errors");

    let stderr = fx.koja_err(&["new", "MyApp"]);
    assert!(
        stderr.contains(
            "error: project name must be lowercase snake_case (like `my_app`). \
             The code namespace is derived from it (`my_app` -> `MyApp`)"
        ),
        "unexpected stderr: {stderr}"
    );

    fs::create_dir(fx.root.join("taken")).unwrap();
    let stderr = fx.koja_err(&["new", "taken"]);
    assert!(
        stderr.contains("error: directory 'taken' already exists"),
        "unexpected stderr: {stderr}"
    );
}
