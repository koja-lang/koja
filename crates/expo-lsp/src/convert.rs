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
/// find `project.expo`, and if the import starts with the project name,
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

struct ProjectInfo {
    name: String,
    src: Vec<String>,
}

fn find_project_config(start_dir: &std::path::Path) -> Option<(PathBuf, ProjectInfo)> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("project.expo");
        if candidate.exists() {
            let source = std::fs::read_to_string(&candidate).ok()?;
            let info = parse_project_info(&source)?;
            return Some((dir, info));
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn parse_project_info(source: &str) -> Option<ProjectInfo> {
    use expo_ast::ast::*;

    let wrapped = format!("fn __project__\n{source}\nend");
    let result = expo_parser::parse(&wrapped);
    for item in &result.module.items {
        let Item::Function(func) = item else { continue };
        for stmt in &func.body {
            let Statement::Expr(Expr::StructConstruction {
                type_path, fields, ..
            }) = stmt
            else {
                continue;
            };
            if type_path.first().map(|s| s.as_str()) != Some("Project") {
                continue;
            }
            let mut proj_name = None;
            let mut src = vec!["src".to_string()];
            for field in fields {
                match field.name.as_str() {
                    "name" => proj_name = extract_string_value(&field.value),
                    "src" => {
                        if let Some(list) = extract_string_list_value(&field.value) {
                            src = list;
                        }
                    }
                    _ => {}
                }
            }
            return Some(ProjectInfo {
                name: proj_name?,
                src,
            });
        }
    }
    None
}

fn extract_string_value(expr: &expo_ast::ast::Expr) -> Option<String> {
    if let expo_ast::ast::Expr::String { parts, .. } = expr
        && parts.len() == 1
        && let expo_ast::ast::StringPart::Literal { value, .. } = &parts[0]
    {
        Some(value.clone())
    } else {
        None
    }
}

fn extract_string_list_value(expr: &expo_ast::ast::Expr) -> Option<Vec<String>> {
    if let expo_ast::ast::Expr::List { elements, .. } = expr {
        elements.iter().map(extract_string_value).collect()
    } else {
        None
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
