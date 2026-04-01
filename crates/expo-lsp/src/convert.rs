//! Shared conversion utilities for the Expo LSP.
//!
//! Provides common helpers for span-to-range conversion, module file
//! resolution, and doc extraction from external files.

use std::path::PathBuf;

use tower_lsp_server::ls_types::*;

use expo_ast::span::Span;

/// Converts an Expo compiler [`Span`] to an LSP [`Range`].
///
/// Expo spans are 1-indexed; LSP ranges are 0-indexed.
pub(crate) fn span_to_range(span: &Span) -> Range {
    Range {
        start: Position::new(
            span.start.line.saturating_sub(1),
            span.start.column.saturating_sub(1),
        ),
        end: Position::new(
            span.end.line.saturating_sub(1),
            span.end.column.saturating_sub(1),
        ),
    }
}

/// Resolves a module import path to a `.expo` file on disk.
///
/// First tries project-aware resolution: walks up from the current file to
/// find `expo.toml`, and if the import starts with the project name,
/// strips it and resolves relative to the project's src directories.
/// Falls back to resolving relative to the current file's directory.
pub(crate) fn resolve_module_file(current_uri: &Uri, module_path: &[String]) -> Option<PathBuf> {
    let uri_str = current_uri.as_str();
    let file_path = uri_str.strip_prefix("file://")?;
    let current = PathBuf::from(file_path);
    let dir = current.parent()?;

    if let Some(resolved) = resolve_via_project(dir, module_path) {
        return Some(resolved);
    }

    let mut candidate = dir.to_path_buf();
    for segment in module_path {
        candidate.push(segment);
    }
    candidate.set_extension("expo");

    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

fn resolve_via_project(start_dir: &std::path::Path, module_path: &[String]) -> Option<PathBuf> {
    let (project_root, config) = find_project_config(start_dir)?;

    let first = module_path.first()?;
    if first != &config.name {
        return None;
    }

    let rest = &module_path[1..];
    let relative = rest
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("/");

    for src in &config.src {
        let file_path = project_root.join(src).join(format!("{relative}.expo"));
        if file_path.exists() {
            return Some(file_path);
        }
    }

    None
}

#[derive(serde::Deserialize)]
struct ExpoToml {
    project: ProjectInfo,
}

#[derive(serde::Deserialize)]
struct ProjectInfo {
    name: String,
    #[serde(default = "default_src")]
    src: Vec<String>,
}

fn default_src() -> Vec<String> {
    vec!["src".to_string()]
}

fn find_project_config(start_dir: &std::path::Path) -> Option<(PathBuf, ProjectInfo)> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("expo.toml");
        if candidate.exists() {
            let source = std::fs::read_to_string(&candidate).ok()?;
            let parsed: ExpoToml = toml::from_str(&source).ok()?;
            return Some((dir, parsed.project));
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Parses the file at `uri_str` and extracts the `@doc` annotation for
/// the item named `name`.
pub(crate) fn find_doc_from_uri(uri_str: &str, name: &str) -> Option<String> {
    let path = uri_str.strip_prefix("file://")?;
    let source = std::fs::read_to_string(path).ok()?;
    let parsed = expo_parser::parse(&source);
    crate::lookup::find_doc_for(&parsed.module, name)
}
