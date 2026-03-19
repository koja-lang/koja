//! Diagnostics pipeline for the Expo LSP.
//!
//! Handles parsing, import resolution, type checking, and conversion of
//! Expo compiler diagnostics into LSP diagnostics.

use std::collections::HashMap;

use tower_lsp_server::ls_types::*;

use expo_ast::ast::{Diagnostic as ExpoDiagnostic, ImportTarget, Item, Severity as ExpoSeverity};
use expo_typecheck::context::TypeContext;

use crate::backend::{Backend, DocumentState};
use crate::convert::{resolve_module_file, span_to_range};

impl Backend {
    /// Runs the full diagnostic pipeline on the given source text:
    /// parse, resolve imports, type-check, then publish LSP diagnostics.
    pub(crate) async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        let parse_result = expo_parser::parse(text);

        let mut all_diags: Vec<ExpoDiagnostic> = parse_result.errors;

        let (ctx, imported_origins, module_uris) = if all_diags
            .iter()
            .all(|d| !matches!(d.severity, ExpoSeverity::Error))
        {
            let mut module_contexts: HashMap<String, TypeContext> = HashMap::new();
            let mut origins: HashMap<String, String> = HashMap::new();
            let mut mod_uris: HashMap<String, String> = HashMap::new();

            for item in &parse_result.module.items {
                if let Item::Import(import) = item {
                    let (resolve_path, module_key, qualifier) = match &import.target {
                        ImportTarget::Item(name) => {
                            let mut full = import.path.clone();
                            full.push(name.clone());
                            let key = full.join(".");
                            (full, key, Some(name.clone()))
                        }
                        _ => {
                            let key = import.path.join(".");
                            let q = import.path.last().cloned();
                            (import.path.clone(), key, q)
                        }
                    };

                    if let Some(dep_path) = resolve_module_file(&uri, &resolve_path)
                        && let Ok(dep_source) = std::fs::read_to_string(&dep_path)
                    {
                        let dep_parsed = expo_parser::parse(&dep_source);
                        let dep_ctx = expo_typecheck::collect_module(&dep_parsed.module);
                        let dep_uri = format!("file://{}", dep_path.display());

                        for (name, sig) in &dep_ctx.functions {
                            if sig.visibility == expo_ast::ast::Visibility::Public {
                                origins.insert(name.clone(), dep_uri.clone());
                            }
                        }
                        for name in dep_ctx.structs.keys() {
                            origins.insert(name.clone(), dep_uri.clone());
                        }
                        for name in dep_ctx.enums.keys() {
                            origins.insert(name.clone(), dep_uri.clone());
                        }

                        if let Some(q) = qualifier {
                            mod_uris.insert(q, dep_uri);
                        }

                        module_contexts.insert(module_key, dep_ctx);
                    }
                }
            }

            let mut ctx = expo_typecheck::collect_module(&parse_result.module);
            expo_typecheck::merge_stdlib(&self.stdlib_ctx, &mut ctx);
            expo_typecheck::re_resolve_generics(&mut ctx);
            expo_typecheck::mark_recursive_fields(&mut ctx);
            expo_typecheck::resolve_imports(&parse_result.module, &mut ctx, &module_contexts);
            expo_typecheck::check_module(&parse_result.module, &mut ctx);
            all_diags.extend(ctx.diagnostics.clone());
            (ctx, origins, mod_uris)
        } else {
            (TypeContext::new(), HashMap::new(), HashMap::new())
        };

        {
            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    module: parse_result.module,
                    ctx,
                    source: text.to_string(),
                    imported_origins,
                    module_uris,
                },
            );
        }

        let lsp_diags: Vec<Diagnostic> = all_diags.iter().map(to_lsp_diagnostic).collect();

        for d in &lsp_diags {
            eprintln!(
                "[expo-lsp] diag: {:?} L{}:{}-L{}:{} \"{}\"",
                d.severity,
                d.range.start.line,
                d.range.start.character,
                d.range.end.line,
                d.range.end.character,
                d.message,
            );
        }

        self.client
            .publish_diagnostics(uri, lsp_diags, version)
            .await;
    }
}

/// Converts an Expo compiler diagnostic to an LSP diagnostic.
fn to_lsp_diagnostic(d: &ExpoDiagnostic) -> Diagnostic {
    let severity = match d.severity {
        ExpoSeverity::Error => DiagnosticSeverity::ERROR,
        ExpoSeverity::Warning => DiagnosticSeverity::WARNING,
        ExpoSeverity::Note => DiagnosticSeverity::INFORMATION,
    };

    let message = match &d.hint {
        Some(hint) => format!("{}\n{}", d.message, hint),
        None => d.message.clone(),
    };

    Diagnostic {
        range: span_to_range(&d.span),
        severity: Some(severity),
        source: Some("expo".to_string()),
        message,
        ..Default::default()
    }
}
