//! Project configuration parser.
//!
//! Reads `koja.toml` and extracts a [`ProjectConfig`] via TOML deserialization.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Top-level TOML structure: `[project]` + optional `[dependencies]`
/// and `[tasks]`.
#[derive(Deserialize)]
struct KojaToml {
    /// The project dependencies.
    #[serde(default)]
    dependencies: HashMap<String, DepConfig>,
    /// The project configuration.
    project: ProjectConfig,
    /// Exported tasks, task name -> implementing type.
    #[serde(default)]
    tasks: HashMap<String, String>,
}

/// A single dependency declaration from `[dependencies]`.
///
/// The raw TOML shape. [`DepConfig::source`] validates it into a
/// [`DepSource`].
#[derive(Debug, Deserialize)]
pub struct DepConfig {
    pub branch: Option<String>,
    pub git: Option<String>,
    pub github: Option<String>,
    pub path: Option<String>,
    pub rev: Option<String>,
    pub tag: Option<String>,
}

/// A validated dependency source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DepSource {
    Git { reference: GitRef, url: String },
    Path(String),
}

/// Which ref a git dependency pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRef {
    Branch(String),
    DefaultBranch,
    Rev(String),
    Tag(String),
}

impl GitRef {
    /// Canonical requirement string stored in `koja.lock`. A lock
    /// entry is stale when this no longer matches the manifest.
    pub fn requirement(&self) -> String {
        match self {
            GitRef::Branch(branch) => format!("branch = {branch}"),
            GitRef::DefaultBranch => "default-branch".to_string(),
            GitRef::Rev(rev) => format!("rev = {rev}"),
            GitRef::Tag(tag) => format!("tag = {tag}"),
        }
    }
}

impl DepConfig {
    /// Validate the raw declaration into a [`DepSource`]: exactly one
    /// of `path`/`git`/`github`, at most one ref selector, and the
    /// `github` slug normalized to its full URL.
    pub fn source(&self, alias: &str) -> Result<DepSource, String> {
        let origins = [&self.path, &self.git, &self.github];
        if origins.iter().filter(|origin| origin.is_some()).count() != 1 {
            return Err(format!(
                "dependency `{alias}` must declare exactly one of `path`, `git`, or `github`"
            ));
        }

        if let Some(path) = &self.path {
            if self.branch.is_some() || self.rev.is_some() || self.tag.is_some() {
                return Err(format!(
                    "dependency `{alias}`: `branch`, `tag`, and `rev` only apply to git dependencies"
                ));
            }
            return Ok(DepSource::Path(path.clone()));
        }

        let url = match (&self.git, &self.github) {
            (Some(url), None) => url.clone(),
            (None, Some(slug)) => github_url(alias, slug)?,
            _ => unreachable!("exactly one origin checked above"),
        };
        warn_embedded_credentials(alias, &url);

        let reference = match (&self.branch, &self.rev, &self.tag) {
            (None, None, None) => GitRef::DefaultBranch,
            (Some(branch), None, None) => GitRef::Branch(branch.clone()),
            (None, Some(rev), None) => GitRef::Rev(rev.clone()),
            (None, None, Some(tag)) => GitRef::Tag(tag.clone()),
            _ => {
                return Err(format!(
                    "dependency `{alias}` may pin at most one of `branch`, `tag`, or `rev`"
                ));
            }
        };
        Ok(DepSource::Git { reference, url })
    }
}

/// Expand a `github = "owner/repo"` slug to its canonical URL. Only
/// the full URL ever reaches the lockfile and the mirror cache, so
/// switching a dep between `github` and the equivalent `git` form
/// never invalidates a lock entry.
fn github_url(alias: &str, slug: &str) -> Result<String, String> {
    let mut segments = slug.split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some(owner), Some(repo), None)
            if !owner.is_empty() && !repo.is_empty() && !slug.contains(char::is_whitespace) =>
        {
            Ok(format!("https://github.com/{owner}/{repo}"))
        }
        _ => Err(format!(
            "dependency `{alias}`: `github` must be an `owner/repo` slug, got `{slug}`"
        )),
    }
}

/// Warn when a URL embeds `user:token@` credentials. koja.toml is
/// usually committed, so tokens belong in git credential helpers or
/// `insteadOf` rewrites, never in the manifest.
fn warn_embedded_credentials(alias: &str, url: &str) {
    let Some((_, rest)) = url.split_once("://") else {
        return;
    };
    let Some((userinfo, _)) = rest.split_once('@') else {
        return;
    };
    if userinfo.contains(':') && !userinfo.contains('/') {
        eprintln!(
            "warning: dependency `{alias}` embeds credentials in its URL; \
             use a git credential helper or `insteadOf` rewrite instead"
        );
    }
}

/// Parsed project configuration from an `koja.toml` file.
#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub authors: Vec<String>,
    /// Output binary name. Falls back to the package name.
    #[serde(default)]
    pub bin: Option<String>,
    /// Explicit PascalCase code namespace, for packages whose namespace
    /// can't be derived from `name` (acronyms: `name = "json"` +
    /// `namespace = "JSON"`). See [`ProjectConfig::namespace`].
    #[serde(default, rename = "namespace")]
    pub declared_namespace: Option<String>,
    #[serde(default)]
    pub dependencies: HashMap<String, DepConfig>,
    #[serde(default)]
    pub description: Option<String>,
    /// The project entry point type. Must be a PascalCase type implementing `Process<C, M, R>`.
    #[serde(default)]
    pub entry: Option<String>,
    /// Minimum compiler version, e.g. "0.15.0". A bare version, no operators.
    #[serde(default)]
    pub koja: Option<String>,
    /// SPDX expression, e.g. "MIT OR Apache-2.0"
    #[serde(default)]
    pub license: Option<String>,
    /// The lowercase snake_case package identity, used for dependency
    /// keys, `deps/` directories, and task name prefixes.
    pub name: String,
    #[serde(default = "default_src")]
    pub src: Vec<String>,
    /// Exported tasks from the top-level `[tasks]` table. Each maps a
    /// task name (prefixed with this package's `name`, e.g.
    /// `"postgres.migrate"`) to the type implementing `Koja.Task`.
    #[serde(default, skip_deserializing)]
    pub tasks: HashMap<String, String>,
    #[serde(default = "default_test")]
    pub test: Vec<String>,
    /// The project version. Should be a semantic version string.
    pub version: String,
}

fn default_src() -> Vec<String> {
    vec!["src".to_string()]
}

fn default_test() -> Vec<String> {
    vec!["test".to_string()]
}

impl ProjectConfig {
    /// Output binary name: the explicit `bin` field when set, otherwise
    /// the (already lowercase) package name.
    pub fn binary_name(&self) -> String {
        self.bin.clone().unwrap_or_else(|| self.name.clone())
    }

    /// The PascalCase code namespace, taken from the declared `namespace`
    /// field when set and otherwise derived from `name` (`my_app` -> `MyApp`).
    /// This is the string stamped on every source file and used for
    /// qualified access (`Postgres.Connection`), while `name` stays
    /// the lowercase identity.
    pub fn namespace(&self) -> String {
        self.declared_namespace
            .clone()
            .unwrap_or_else(|| koja_parser::derive_namespace(&self.name))
    }

    /// Returns the entry value as a Process type name when it starts with an
    /// uppercase letter (PascalCase), the only valid entry shape. The driver
    /// rejects lowercase entries with a pointer at `.kojs` scripts.
    pub fn entry_type_name(&self) -> Option<&str> {
        self.entry
            .as_deref()
            .filter(|e| e.starts_with(|c: char| c.is_ascii_uppercase()))
    }
}

/// Attempts to load an `koja.toml` file from the given directory.
///
/// Returns `Ok(Some(config))` if the file exists and is valid,
/// `Ok(None)` if no `koja.toml` exists, or `Err` for malformed files.
pub fn load_project(dir: &Path) -> Result<Option<ProjectConfig>, String> {
    let toml_path = dir.join("koja.toml");
    if !toml_path.exists() {
        return Ok(None);
    }

    let source =
        fs::read_to_string(&toml_path).map_err(|e| format!("error reading koja.toml: {e}"))?;

    let parsed: KojaToml =
        toml::from_str(&source).map_err(|e| format!("koja.toml parse error: {e}"))?;

    let mut config = parsed.project;
    config.dependencies = parsed.dependencies;
    config.tasks = parsed.tasks;
    for (alias, dep) in &config.dependencies {
        dep.source(alias)?;
    }

    check_package_identity(&config)?;
    check_tasks(&config)?;
    let current = parse_version(env!("CARGO_PKG_VERSION")).expect("crate version is X.Y.Z");
    check_koja_version(&config, current)?;
    Ok(Some(config))
}

/// Resolve an explicit project directory without changing the process
/// working directory.
pub fn resolve_project_root(path: &Path) -> Result<PathBuf, String> {
    let root = path.canonicalize().map_err(|err| {
        format!(
            "cannot resolve project directory `{}`: {err}",
            path.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!(
            "project path `{}` is not a directory",
            path.display()
        ));
    }
    if !root.join("koja.toml").is_file() {
        return Err(format!(
            "no `koja.toml` found in project directory `{}`",
            path.display()
        ));
    }
    Ok(root)
}

/// Validate the `[tasks]` table. Every task name is namespaced under
/// this package's `name` (`postgres.migrate` from package `postgres`),
/// which makes task collisions across the dependency graph structurally
/// impossible. Values name the PascalCase type implementing `Koja.Task`.
fn check_tasks(config: &ProjectConfig) -> Result<(), String> {
    let name = &config.name;
    for (task, task_type) in &config.tasks {
        let rest = task.strip_prefix(name).and_then(|r| r.strip_prefix('.'));
        let valid_rest = rest.is_some_and(|r| {
            !r.is_empty()
                && r.split('.').all(|segment| {
                    segment.starts_with(|c: char| c.is_ascii_lowercase())
                        && segment
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                })
        });
        if !valid_rest {
            return Err(format!(
                "task `{task}` must be named `{name}.<task>` (lowercase snake_case segments), \
                 e.g. `{name}.migrate`"
            ));
        }
        if !task_type.starts_with(|c: char| c.is_ascii_uppercase()) {
            return Err(format!(
                "task `{task}` must name a PascalCase type implementing `Koja.Task`, got `{task_type}`"
            ));
        }
    }
    Ok(())
}

/// Validate the `name` / `namespace` split. `name` is the lowercase
/// snake_case identity, `namespace` (when declared) the PascalCase
/// code-facing name. Rejections hint at the snake_case rewrite and, when
/// derivation wouldn't round-trip, the `namespace` override to keep.
fn check_package_identity(config: &ProjectConfig) -> Result<(), String> {
    let name = &config.name;
    let valid_name = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid_name {
        let suggested = suggest_snake_case(name);
        let mut message =
            format!("package name `{name}` must be lowercase snake_case (like `{suggested}`)");
        if koja_parser::derive_namespace(&suggested) != *name {
            message.push_str(&format!(
                ". Keep `{name}` as the code namespace with `namespace = \"{name}\"`"
            ));
        }
        return Err(message);
    }

    if let Some(namespace) = &config.declared_namespace {
        let valid_namespace = namespace.starts_with(|c: char| c.is_ascii_uppercase())
            && namespace.chars().all(|c| c.is_ascii_alphanumeric());
        if !valid_namespace {
            return Err(format!(
                "package `{name}`: namespace `{namespace}` must be PascalCase (like `{}`)",
                koja_parser::derive_namespace(name)
            ));
        }
    }
    Ok(())
}

/// Best-effort snake_case rewrite of an invalid package name, for the
/// error hint. Case boundaries become underscores and consecutive
/// capitals collapse (`CrossRef` -> `cross_ref`, `JSON` -> `json`).
fn suggest_snake_case(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    if out.is_empty() || !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        return format!("my_{out}");
    }
    out
}

/// Parse a bare `X.Y` or `X.Y.Z` version into a comparable triple.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut numbers = version.split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    let patch = match numbers.next() {
        Some(patch) => patch.parse().ok()?,
        None => 0,
    };
    match numbers.next() {
        Some(_) => None,
        None => Some((major, minor, patch)),
    }
}

/// Enforce the manifest's `koja` minimum against the running compiler.
fn check_koja_version(config: &ProjectConfig, current: (u64, u64, u64)) -> Result<(), String> {
    let Some(required) = &config.koja else {
        return Ok(());
    };
    let name = &config.name;

    let Some(minimum) = parse_version(required) else {
        if required.starts_with(|c: char| !c.is_ascii_digit()) {
            return Err(format!(
                "package `{name}`: `koja` takes a bare minimum version like \"0.15.0\", got `{required}`"
            ));
        }
        return Err(format!(
            "package `{name}`: `koja` must be an `X.Y` or `X.Y.Z` version, got `{required}`"
        ));
    };

    if current < minimum {
        let (major, minor, patch) = current;
        return Err(format!(
            "package `{name}` requires koja >= {required}, but this is koja {major}.{minor}.{patch}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> ProjectConfig {
        let parsed: KojaToml = toml::from_str(source).expect("valid koja.toml");
        let mut config = parsed.project;
        config.tasks = parsed.tasks;
        config
    }

    #[test]
    fn binary_name_honors_explicit_bin() {
        let config = parse(
            r#"
            [project]
            name = "gh"
            version = "0.1.0"
            bin = "gh-cli"
            "#,
        );
        assert_eq!(config.binary_name(), "gh-cli");
    }

    #[test]
    fn binary_name_defaults_to_package_name() {
        let config = parse(
            r#"
            [project]
            name = "gh"
            version = "0.1.0"
            "#,
        );
        assert_eq!(config.binary_name(), "gh");
    }

    #[test]
    fn namespace_derives_from_snake_case_name() {
        let config = parse(
            r#"
            [project]
            name = "my_app"
            version = "0.1.0"
            "#,
        );
        assert_eq!(config.namespace(), "MyApp");
    }

    #[test]
    fn namespace_honors_explicit_declaration() {
        let config = parse(
            r#"
            [project]
            name = "json"
            namespace = "JSON"
            version = "0.1.0"
            "#,
        );
        assert_eq!(config.namespace(), "JSON");
    }

    #[test]
    fn identity_rejects_pascal_case_name_with_snake_hint() {
        let config = parse(
            r#"
            [project]
            name = "CrossRef"
            version = "0.1.0"
            "#,
        );
        let err = check_package_identity(&config).unwrap_err();
        assert!(err.contains("`cross_ref`"), "got: {err}");
    }

    #[test]
    fn identity_hints_namespace_override_when_derivation_lossy() {
        let config = parse(
            r#"
            [project]
            name = "JSON"
            version = "0.1.0"
            "#,
        );
        let err = check_package_identity(&config).unwrap_err();
        assert!(err.contains("namespace = \"JSON\""), "got: {err}");
    }

    #[test]
    fn identity_rejects_invalid_namespace() {
        let config = parse(
            r#"
            [project]
            name = "my_app"
            namespace = "my_app"
            version = "0.1.0"
            "#,
        );
        let err = check_package_identity(&config).unwrap_err();
        assert!(err.contains("PascalCase"), "got: {err}");
    }

    #[test]
    fn identity_accepts_snake_case_names() {
        for name in ["gh", "my_app", "http2", "a_b_c"] {
            let config = parse(&format!(
                "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"
            ));
            assert_eq!(check_package_identity(&config), Ok(()), "name: {name}");
        }
    }

    fn parse_with_task(task: &str, task_type: &str) -> ProjectConfig {
        parse(&format!(
            "[project]\nname = \"postgres\"\nversion = \"0.1.0\"\n\n[tasks]\n\"{task}\" = \"{task_type}\"\n"
        ))
    }

    #[test]
    fn tasks_accept_package_prefixed_names() {
        for task in ["postgres.migrate", "postgres.db.migrate", "postgres.gen_2"] {
            let config = parse_with_task(task, "Migrate");
            assert_eq!(check_tasks(&config), Ok(()), "task: {task}");
        }
    }

    #[test]
    fn tasks_reject_unprefixed_or_malformed_names() {
        for task in [
            "migrate",
            "db.migrate",
            "postgres",
            "postgres.",
            "postgres.Migrate",
            "postgresql.migrate",
        ] {
            let err = check_tasks(&parse_with_task(task, "Migrate")).unwrap_err();
            assert!(err.contains("`postgres.<task>`"), "task {task}: {err}");
        }
    }

    #[test]
    fn tasks_reject_lowercase_type_values() {
        let err = check_tasks(&parse_with_task("postgres.migrate", "migrate")).unwrap_err();
        assert!(err.contains("PascalCase type"), "got: {err}");
    }

    fn dep(source: &str) -> Result<DepSource, String> {
        let config: DepConfig = toml::from_str(source).expect("valid dep table");
        config.source("dep")
    }

    #[test]
    fn github_slug_normalizes_to_full_url() {
        assert_eq!(
            dep(r#"github = "koja-lang/postgres""#),
            Ok(DepSource::Git {
                reference: GitRef::DefaultBranch,
                url: "https://github.com/koja-lang/postgres".to_string(),
            })
        );
        assert!(dep(r#"github = "not-a-slug""#).is_err());
        assert!(dep(r#"github = "too/many/parts""#).is_err());
    }

    #[test]
    fn git_deps_accept_at_most_one_ref_selector() {
        assert_eq!(
            dep(r#"git = "https://example.com/x.git"
                   tag = "v1.0""#),
            Ok(DepSource::Git {
                reference: GitRef::Tag("v1.0".to_string()),
                url: "https://example.com/x.git".to_string(),
            })
        );
        assert!(
            dep(r#"git = "https://example.com/x.git"
                   tag = "v1.0"
                   branch = "main""#)
            .is_err()
        );
    }

    #[test]
    fn dep_declares_exactly_one_origin() {
        assert!(dep("").is_err());
        assert!(
            dep(r#"path = "libs/x"
                   github = "a/b""#)
            .is_err()
        );
        assert!(
            dep(r#"path = "libs/x"
                   tag = "v1.0""#)
            .is_err(),
            "ref selectors only apply to git deps"
        );
    }

    #[test]
    fn requirement_strings_are_canonical() {
        assert_eq!(GitRef::DefaultBranch.requirement(), "default-branch");
        assert_eq!(
            GitRef::Branch("main".to_string()).requirement(),
            "branch = main"
        );
        assert_eq!(GitRef::Tag("v1.0".to_string()).requirement(), "tag = v1.0");
        assert_eq!(GitRef::Rev("abc".to_string()).requirement(), "rev = abc");
    }

    fn check(required: &str, current: (u64, u64, u64)) -> Result<(), String> {
        let config = parse(&format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\nkoja = \"{required}\"\n"
        ));
        check_koja_version(&config, current)
    }

    #[test]
    fn koja_minimum_passes_when_satisfied() {
        assert_eq!(check("0.15.0", (0, 15, 0)), Ok(()));
        assert_eq!(check("0.15.0", (0, 15, 1)), Ok(()));
        assert_eq!(check("0.15", (0, 15, 0)), Ok(()), "missing patch means 0");
        assert_eq!(check("0.9.9", (0, 15, 0)), Ok(()));
    }

    #[test]
    fn koja_minimum_fails_with_both_versions_named() {
        let err = check("0.15.0", (0, 14, 1)).unwrap_err();
        assert_eq!(
            err,
            "package `pkg` requires koja >= 0.15.0, but this is koja 0.14.1"
        );
    }

    #[test]
    fn koja_rejects_operators_with_a_hint() {
        let err = check("~> 0.15", (0, 15, 0)).unwrap_err();
        assert!(err.contains("bare minimum version"), "got: {err}");
        let err = check(">= 0.15.0", (0, 15, 0)).unwrap_err();
        assert!(err.contains("bare minimum version"), "got: {err}");
    }

    #[test]
    fn koja_rejects_malformed_versions() {
        for bad in ["0", "0.15.0.1", "0.x", "0.15-beta"] {
            let err = check(bad, (0, 15, 0)).unwrap_err();
            assert!(err.contains("`X.Y` or `X.Y.Z`"), "got: {err}");
        }
    }
}
