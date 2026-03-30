//! Project configuration parser.
//!
//! Reads `project.expo` and extracts a [`ProjectConfig`] by parsing the file
//! with the standard Expo parser and walking the AST for a `Project{...}`
//! struct construction.

use std::fs;
use std::path::Path;

use expo_ast::ast::{Expr, Item, Statement, StringPart};

/// Parsed project configuration from a `project.expo` file.
#[derive(Debug)]
pub struct ProjectConfig {
    pub name: String,
    pub version: String,
    pub src: Vec<String>,
    pub entry: Option<String>,
}

/// Attempts to load a `project.expo` file from the given directory.
///
/// Returns `Ok(Some(config))` if the file exists and is valid,
/// `Ok(None)` if no `project.expo` exists, or `Err` for malformed files.
pub fn load_project(dir: &Path) -> Result<Option<ProjectConfig>, String> {
    let project_path = dir.join("project.expo");
    if !project_path.exists() {
        return Ok(None);
    }

    let source = fs::read_to_string(&project_path)
        .map_err(|e| format!("error reading project.expo: {e}"))?;

    let config = parse_project_config(&source)?;
    Ok(Some(config))
}

/// Parses a `Project{...}` struct construction from source text.
///
/// The file is wrapped in a dummy function body so the existing expression
/// parser handles the `TypeIdent{...}` construction without changes to the
/// parser's top-level item dispatch.
fn parse_project_config(source: &str) -> Result<ProjectConfig, String> {
    let wrapped = format!("fn __project__\n{source}\nend");
    let result = expo_parser::parse(&wrapped);

    if !result.errors.is_empty() {
        let msgs: Vec<&str> = result.errors.iter().map(|d| d.message.as_str()).collect();
        return Err(format!("project.expo parse error: {}", msgs.join(", ")));
    }

    for item in &result.module.items {
        if let Item::Function(func) = item {
            for stmt in &func.body {
                if let Some(config) = try_extract_project(stmt) {
                    return config;
                }
            }
        }
    }

    Err("project.expo must contain a Project{...} expression".to_string())
}

/// Tries to extract a `ProjectConfig` from a statement containing a
/// `Project{...}` struct construction.
fn try_extract_project(stmt: &Statement) -> Option<Result<ProjectConfig, String>> {
    let expr = match stmt {
        Statement::Expr(e) => e,
        _ => return None,
    };

    let (type_path, fields) = match expr {
        Expr::StructConstruction {
            type_path, fields, ..
        } => (type_path, fields),
        _ => return None,
    };

    if type_path.len() != 1 || type_path[0] != "Project" {
        return None;
    }

    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut src: Option<Vec<String>> = None;
    let mut entry: Option<String> = None;

    for field in fields {
        match field.name.as_str() {
            "name" => name = extract_string(&field.value),
            "version" => version = extract_string(&field.value),
            "src" => src = extract_string_list(&field.value),
            "entry" => entry = extract_string(&field.value),
            other => {
                return Some(Err(format!("project.expo: unknown field `{other}`")));
            }
        }
    }

    let name = match name {
        Some(n) => n,
        None => {
            return Some(Err(
                "project.expo: missing required field `name`".to_string()
            ));
        }
    };
    let version = match version {
        Some(v) => v,
        None => {
            return Some(Err(
                "project.expo: missing required field `version`".to_string()
            ));
        }
    };

    Some(Ok(ProjectConfig {
        name,
        version,
        src: src.unwrap_or_else(|| vec!["src".to_string()]),
        entry,
    }))
}

/// Extracts a plain string value from a string expression.
///
/// Handles `Expr::String` with a single `StringPart::Literal` (no interpolation).
fn extract_string(expr: &Expr) -> Option<String> {
    if let Expr::String { parts, .. } = expr
        && parts.len() == 1
        && let StringPart::Literal { value, .. } = &parts[0]
    {
        return Some(value.clone());
    }
    None
}

/// Extracts a list of string literals from a `List{elements}` expression.
fn extract_string_list(expr: &Expr) -> Option<Vec<String>> {
    if let Expr::List { elements, .. } = expr {
        let mut result = Vec::new();
        for elem in elements {
            result.push(extract_string(elem)?);
        }
        Some(result)
    } else {
        None
    }
}
