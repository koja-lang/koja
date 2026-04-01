//! Diagnostics pipeline for the Expo LSP.
//!
//! Handles parsing, type checking, and conversion of Expo compiler
//! diagnostics into LSP diagnostics.

use tower_lsp_server::ls_types::*;

use expo_ast::ast::{Diagnostic as ExpoDiagnostic, Severity as ExpoSeverity};
use expo_typecheck::context::TypeContext;

use crate::backend::{Backend, DocumentState};
use crate::convert::span_to_range;

impl Backend {
    /// Runs the full diagnostic pipeline on the given source text:
    /// parse, type-check, then publish LSP diagnostics.
    pub(crate) async fn diagnose(&self, uri: Uri, text: &str, version: Option<i32>) {
        let parse_result = expo_parser::parse(text);

        let mut all_diags: Vec<ExpoDiagnostic> = parse_result.errors;

        let ctx = if all_diags
            .iter()
            .all(|d| !matches!(d.severity, ExpoSeverity::Error))
        {
            let mut all_for_names: Vec<&expo_ast::ast::Module> =
                self.stdlib_modules.iter().collect();
            all_for_names.push(&parse_result.module);
            let global_names = expo_typecheck::collect_all_names(&all_for_names);

            let mut ctx = expo_typecheck::collect_module(&parse_result.module, &global_names);
            ctx.merge(&self.stdlib_ctx);
            expo_typecheck::mark_recursive_fields(&mut ctx);
            expo_typecheck::check_module(&parse_result.module, &mut ctx);
            all_diags.extend(ctx.diagnostics.clone());
            ctx
        } else {
            TypeContext::new()
        };

        {
            let mut docs = self.documents.write().await;
            docs.insert(
                uri.as_str().to_string(),
                DocumentState {
                    module: parse_result.module,
                    ctx,
                    source: text.to_string(),
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
