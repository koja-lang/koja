//! Integration tests for git dependencies, `koja.lock`, and the
//! `koja deps` command family.
//!
//! No network: fixture repos are `git init`ed in temp dirs and
//! referenced by local path URL, with `KOJA_HOME` pointed at a
//! per-test cache so nothing leaks into `~/.koja`. Offline behavior
//! is proven by renaming the "remote" away and asserting commands
//! still succeed (or fail with the actionable message).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn koja_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koja"))
}

/// Per-test temp tree: `home/` (KOJA_HOME), `project/`, `repos/`.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "koja-deps-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    /// Scaffold the root project with the given `[dependencies]`.
    fn write_project(&self, deps: &[(&str, String)]) {
        write_package(&self.project(), "Root", deps);
    }

    /// Scaffold a dependency repo and commit it on `main`.
    fn make_repo(&self, dir: &str, package: &str, deps: &[(&str, String)]) -> PathBuf {
        let repo = self.root.join("repos").join(dir);
        write_package(&repo, package, deps);
        git(&repo, &["init", "-q", "-b", "main"]);
        commit_all(&repo);
        repo
    }

    fn koja(&self, args: &[&str]) -> Output {
        self.koja_with_home(&self.home(), args)
    }

    fn koja_with_home(&self, home: &Path, args: &[&str]) -> Output {
        Command::new(koja_bin())
            .args(args)
            .current_dir(self.project())
            .env("KOJA_HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
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

    fn lock_contents(&self) -> String {
        fs::read_to_string(self.project().join("koja.lock")).expect("koja.lock exists")
    }

    /// The pinned rev for `name` in koja.lock.
    fn locked_rev(&self, name: &str) -> String {
        let lock = self.lock_contents();
        let entry = lock
            .split("[[package]]")
            .find(|block| block.contains(&format!("name = \"{name}\"")))
            .unwrap_or_else(|| panic!("no lock entry for {name}:\n{lock}"));
        entry
            .lines()
            .find_map(|line| line.strip_prefix("rev = \""))
            .map(|rest| rest.trim_end_matches('"').to_string())
            .unwrap_or_else(|| panic!("no rev in lock entry for {name}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // deps/ trees are read-only, so flip everything writable
        // before removing.
        let _ = Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&self.root)
            .output();
        fs::remove_dir_all(&self.root).ok();
    }
}

/// Write a minimal koja package: `koja.toml` plus one `src` struct.
fn write_package(dir: &Path, name: &str, deps: &[(&str, String)]) {
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();

    let mut manifest = format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
    if !deps.is_empty() {
        manifest.push_str("\n[dependencies]\n");
        for (alias, spec) in deps {
            manifest.push_str(&format!("{alias} = {spec}\n"));
        }
    }
    fs::write(dir.join("koja.toml"), manifest).unwrap();
    fs::write(
        src.join(format!("{}.koja", name.to_lowercase())),
        format!("struct {name}\nend\n"),
    )
    .unwrap();
}

fn dep_git(repo: &Path) -> String {
    format!("{{ git = \"{}\" }}", repo.display())
}

fn dep_git_tag(repo: &Path, tag: &str) -> String {
    format!("{{ git = \"{}\", tag = \"{tag}\" }}", repo.display())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(dir: &Path) {
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "commit"]);
}

fn head_rev(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"]).trim().to_string()
}

#[test]
fn get_pins_annotated_tag_to_peeled_commit_and_check_runs_offline() {
    let fx = Fixture::new("offline");
    let repo = fx.make_repo("greeter", "Greeter", &[]);
    git(&repo, &["tag", "-a", "v1.0", "-m", "release"]);
    let tag_object = git(&repo, &["rev-parse", "v1.0"]).trim().to_string();
    let commit = git(&repo, &["rev-parse", "v1.0^{commit}"])
        .trim()
        .to_string();
    fx.write_project(&[("greeter", dep_git_tag(&repo, "v1.0"))]);

    fx.koja_ok(&["deps", "get"]);

    assert_eq!(fx.locked_rev("Greeter"), commit);
    assert_ne!(
        commit, tag_object,
        "annotated tag object must not be the pin"
    );

    let dep_root = fx.project().join("deps").join("Greeter");
    let marker = fs::read_to_string(dep_root.join(".koja-rev")).unwrap();
    assert_eq!(marker.trim(), commit);
    let source = dep_root.join("src").join("greeter.koja");
    assert!(source.is_file(), "dep source not materialized");
    assert!(
        fs::metadata(&source).unwrap().permissions().readonly(),
        "materialized dep should be read-only"
    );

    let status = fx.koja_ok(&["deps"]);
    assert!(status.contains("Greeter"), "status missing dep: {status}");
    assert!(status.contains("ok"), "status should report ok: {status}");

    // Take the "remote" away entirely: check must stay offline.
    fs::rename(&repo, repo.with_extension("gone")).unwrap();
    fx.koja_ok(&["check"]);
}

#[test]
fn check_without_lock_or_with_stale_lock_names_the_fix() {
    let fx = Fixture::new("stale");
    let repo = fx.make_repo("greeter", "Greeter", &[]);
    git(&repo, &["tag", "v1.0"]);
    fx.write_project(&[("greeter", dep_git_tag(&repo, "v1.0"))]);

    let stderr = fx.koja_err(&["check"]);
    assert!(
        stderr.contains("koja deps get"),
        "missing-lock error should name the fix: {stderr}"
    );

    fx.koja_ok(&["deps", "get"]);
    fx.koja_ok(&["check"]);

    // A manifest edit (tag bump) makes the pin stale.
    fx.write_project(&[("greeter", dep_git_tag(&repo, "v2.0"))]);
    let stderr = fx.koja_err(&["check"]);
    assert!(
        stderr.contains("koja deps get"),
        "stale-lock error should name the fix: {stderr}"
    );
}

#[test]
fn branch_pin_survives_get_and_moves_on_update() {
    let fx = Fixture::new("update");
    let repo = fx.make_repo("greeter", "Greeter", &[]);
    let first = head_rev(&repo);
    fx.write_project(&[(
        "greeter",
        format!("{{ git = \"{}\", branch = \"main\" }}", repo.display()),
    )]);

    fx.koja_ok(&["deps", "get"]);
    assert_eq!(fx.locked_rev("Greeter"), first);

    // The branch moves. `get` keeps the pin, `update` re-resolves.
    fs::write(repo.join("note.txt"), "moved\n").unwrap();
    commit_all(&repo);
    let second = head_rev(&repo);
    assert_ne!(first, second);

    fx.koja_ok(&["deps", "get"]);
    assert_eq!(fx.locked_rev("Greeter"), first);

    fx.koja_ok(&["deps", "update"]);
    assert_eq!(fx.locked_rev("Greeter"), second);
    let marker = fs::read_to_string(fx.project().join("deps/Greeter/.koja-rev")).unwrap();
    assert_eq!(marker.trim(), second);
}

#[test]
fn transitive_deps_resolve_and_diamonds_dedupe() {
    let fx = Fixture::new("diamond");
    let shared = fx.make_repo("shared", "Shared", &[]);
    let left = fx.make_repo("left", "Left", &[("shared", dep_git(&shared))]);
    let right = fx.make_repo("right", "Right", &[("shared", dep_git(&shared))]);
    fx.write_project(&[("left", dep_git(&left)), ("right", dep_git(&right))]);

    fx.koja_ok(&["deps", "get"]);
    for name in ["Left", "Right", "Shared"] {
        assert!(
            fx.project()
                .join("deps")
                .join(name)
                .join("koja.toml")
                .is_file(),
            "deps/{name} not materialized"
        );
    }

    let lock = fx.lock_contents();
    assert_eq!(lock.matches("[[package]]").count(), 3);
    let (left_at, right_at, shared_at) = (
        lock.find("\"Left\"").unwrap(),
        lock.find("\"Right\"").unwrap(),
        lock.find("\"Shared\"").unwrap(),
    );
    assert!(
        left_at < right_at && right_at < shared_at,
        "lock not sorted:\n{lock}"
    );

    // Resolution is deterministic: a second get is byte-identical.
    fx.koja_ok(&["deps", "get"]);
    assert_eq!(fx.lock_contents(), lock);

    fx.koja_ok(&["check"]);
}

#[test]
fn conflicting_requirements_for_one_source_error() {
    let fx = Fixture::new("conflict");
    let shared = fx.make_repo("shared", "Shared", &[]);
    git(&shared, &["tag", "v1.0"]);
    fs::write(shared.join("note.txt"), "more\n").unwrap();
    commit_all(&shared);
    git(&shared, &["tag", "v2.0"]);

    let left = fx.make_repo("left", "Left", &[("shared", dep_git_tag(&shared, "v1.0"))]);
    let right = fx.make_repo(
        "right",
        "Right",
        &[("shared", dep_git_tag(&shared, "v2.0"))],
    );
    fx.write_project(&[("left", dep_git(&left)), ("right", dep_git(&right))]);

    let stderr = fx.koja_err(&["deps", "get"]);
    assert!(
        stderr.contains("conflicting requirements"),
        "expected conflicting-requirements error: {stderr}"
    );
}

#[test]
fn duplicate_package_name_from_two_sources_errors() {
    let fx = Fixture::new("dupname");
    let first = fx.make_repo("first", "Dup", &[]);
    let second = fx.make_repo("second", "Dup", &[]);
    fx.write_project(&[("first", dep_git(&first)), ("second", dep_git(&second))]);

    let stderr = fx.koja_err(&["deps", "get"]);
    assert!(
        stderr.contains("duplicate package name `Dup`"),
        "expected duplicate-name error: {stderr}"
    );
}

#[test]
fn dependency_cycle_errors() {
    let fx = Fixture::new("cycle");
    let b = fx.make_repo("b", "B", &[]);
    let a = fx.make_repo("a", "A", &[("b", dep_git(&b))]);
    write_package(&b, "B", &[("a", dep_git(&a))]);
    commit_all(&b);
    fx.write_project(&[("a", dep_git(&a))]);

    let stderr = fx.koja_err(&["deps", "get"]);
    assert!(
        stderr.contains("dependency cycle"),
        "expected cycle error: {stderr}"
    );
}

#[test]
fn clean_rebuilds_from_cache_and_cache_miss_is_an_error() {
    let fx = Fixture::new("clean");
    let repo = fx.make_repo("greeter", "Greeter", &[]);
    fx.write_project(&[("greeter", dep_git(&repo))]);
    fx.koja_ok(&["deps", "get"]);

    // Remote gone: everything below is offline.
    fs::rename(&repo, repo.with_extension("gone")).unwrap();

    fx.koja_ok(&["deps", "clean"]);
    assert!(!fx.project().join("deps").exists());

    // A fresh KOJA_HOME has no mirror for the pinned commit.
    let cold_home = fx.root.join("cold-home");
    fs::create_dir_all(&cold_home).unwrap();
    let output = fx.koja_with_home(&cold_home, &["check"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not fetched") && stderr.contains("koja deps get"),
        "expected cache-miss error: {stderr}"
    );

    // The warm cache re-materializes without the remote.
    fx.koja_ok(&["check"]);
    assert!(fx.project().join("deps/Greeter/koja.toml").is_file());

    fx.koja_ok(&["deps", "clean", "--cache"]);
    assert!(!fx.home().join("cache").join("git").exists());
}

#[test]
fn removed_deps_are_pruned_from_the_lock() {
    let fx = Fixture::new("prune");
    let keep = fx.make_repo("keep", "Keep", &[]);
    let drop = fx.make_repo("drop", "Drop", &[]);
    fx.write_project(&[("drop", dep_git(&drop)), ("keep", dep_git(&keep))]);

    fx.koja_ok(&["deps", "get"]);
    assert!(fx.lock_contents().contains("\"Drop\""));

    fx.write_project(&[("keep", dep_git(&keep))]);
    fx.koja_ok(&["deps", "get"]);
    let lock = fx.lock_contents();
    assert!(!lock.contains("\"Drop\""), "pruned entry survived:\n{lock}");
    assert!(lock.contains("\"Keep\""));
}

#[test]
fn path_dep_of_a_git_dep_cannot_escape_its_checkout() {
    let fx = Fixture::new("escape");
    let outside = fx.root.join("outside");
    write_package(&outside, "Outside", &[]);

    let sneaky = fx.make_repo(
        "sneaky",
        "Sneaky",
        &[("outside", "{ path = \"../../../outside\" }".to_string())],
    );
    fx.write_project(&[("sneaky", dep_git(&sneaky))]);

    let stderr = fx.koja_err(&["deps", "get"]);
    assert!(
        stderr.contains("escapes"),
        "expected checkout-escape error: {stderr}"
    );
}
