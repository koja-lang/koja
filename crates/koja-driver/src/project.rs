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
#[derive(Debug, Deserialize)]
pub struct DepConfig {
    pub path: Option<String>,
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
}
