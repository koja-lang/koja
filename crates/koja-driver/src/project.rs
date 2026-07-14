//! Project configuration parser.
//!
//! Reads `koja.toml` and extracts a [`ProjectConfig`] via TOML deserialization.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Top-level TOML structure: `[project]` + optional `[dependencies]`.
#[derive(Deserialize)]
struct KojaToml {
    /// The project dependencies.
    #[serde(default)]
    dependencies: HashMap<String, DepConfig>,
    /// The project configuration.
    project: ProjectConfig,
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
    /// Output binary name. Falls back to the lowercased package name.
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub dependencies: HashMap<String, DepConfig>,
    #[serde(default)]
    pub description: Option<String>,
    /// The project entry point type. Must be a PascalCase type implementing `Process<C, M, R>`.
    #[serde(default)]
    pub entry: Option<String>,
    /// SPDX expression, e.g. "MIT OR Apache-2.0"
    #[serde(default)]
    pub license: Option<String>,
    /// The project name that identifies the package. Should be PascalCase.
    pub name: String,
    #[serde(default = "default_src")]
    pub src: Vec<String>,
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
    /// the package name lowercased (the PascalCase namespace `Gh` yields
    /// a `gh` binary).
    pub fn binary_name(&self) -> String {
        self.bin.clone().unwrap_or_else(|| self.name.to_lowercase())
    }

    /// Returns the entry value as a Process type name when it starts with an
    /// uppercase letter (PascalCase) — the only valid entry shape. The driver
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
    for (alias, dep) in &config.dependencies {
        dep.source(alias)?;
    }
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> ProjectConfig {
        let parsed: KojaToml = toml::from_str(source).expect("valid koja.toml");
        parsed.project
    }

    #[test]
    fn binary_name_honors_explicit_bin() {
        let config = parse(
            r#"
            [project]
            name = "Gh"
            version = "0.1.0"
            bin = "gh-cli"
            "#,
        );
        assert_eq!(config.binary_name(), "gh-cli");
    }

    #[test]
    fn binary_name_defaults_to_lowercased_package_name() {
        let config = parse(
            r#"
            [project]
            name = "Gh"
            version = "0.1.0"
            "#,
        );
        assert_eq!(config.binary_name(), "gh");
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
}
