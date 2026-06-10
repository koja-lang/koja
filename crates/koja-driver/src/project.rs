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
    project: ProjectConfig,
    #[serde(default)]
    dependencies: HashMap<String, DepConfig>,
}

/// A single dependency declaration from `[dependencies]`.
#[derive(Debug, Deserialize)]
pub struct DepConfig {
    pub path: Option<String>,
}

/// Parsed project configuration from an `koja.toml` file.
#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub version: String,
    #[serde(default = "default_src")]
    pub src: Vec<String>,
    #[serde(default = "default_test")]
    pub test: Vec<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(skip)]
    pub dependencies: HashMap<String, DepConfig>,
}

fn default_src() -> Vec<String> {
    vec!["src".to_string()]
}

fn default_test() -> Vec<String> {
    vec!["test".to_string()]
}

impl ProjectConfig {
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
