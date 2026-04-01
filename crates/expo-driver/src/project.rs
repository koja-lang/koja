//! Project configuration parser.
//!
//! Reads `expo.toml` and extracts a [`ProjectConfig`] via TOML deserialization.

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Top-level TOML structure: `[project]` table.
#[derive(Deserialize)]
struct ExpoToml {
    project: ProjectConfig,
}

/// Parsed project configuration from an `expo.toml` file.
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
}

fn default_src() -> Vec<String> {
    vec!["src".to_string()]
}

fn default_test() -> Vec<String> {
    vec!["test".to_string()]
}

/// Attempts to load an `expo.toml` file from the given directory.
///
/// Returns `Ok(Some(config))` if the file exists and is valid,
/// `Ok(None)` if no `expo.toml` exists, or `Err` for malformed files.
pub fn load_project(dir: &Path) -> Result<Option<ProjectConfig>, String> {
    let toml_path = dir.join("expo.toml");
    if !toml_path.exists() {
        return Ok(None);
    }

    let source =
        fs::read_to_string(&toml_path).map_err(|e| format!("error reading expo.toml: {e}"))?;

    let parsed: ExpoToml =
        toml::from_str(&source).map_err(|e| format!("expo.toml parse error: {e}"))?;

    Ok(Some(parsed.project))
}
