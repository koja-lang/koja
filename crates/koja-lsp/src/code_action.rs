//! Quick fixes from diagnostics that carry a [`Fix`].
//!
//! The compiler attaches a fix at the site that reports the
//! diagnostic. The server serializes it into the LSP diagnostic's
//! `data` field on publish, and the client hands that same field
//! back in `textDocument/codeAction`. The handler is stateless: it
//! reads the fixes out of the request and never looks at the
//! document.
//!
//! [`Fix`]: koja_ast::ast::Fix

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::Fix;
use koja_ast::span::Span;

use crate::backend::Backend;

/// The `data` payload on a diagnostic that carries a fix. Ranges are
/// already in LSP coordinates so the code action handler can build
/// the edit without the source.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FixData {
    pub(crate) edits: Vec<TextEdit>,
    pub(crate) title: String,
}

impl FixData {
    /// `range` converts a span over the text of the span's file.
    pub(crate) fn from_fix(fix: &Fix, range: &dyn Fn(&Span) -> Range) -> Self {
        Self {
            edits: fix
                .edits
                .iter()
                .map(|edit| TextEdit {
                    range: range(&edit.span),
                    new_text: edit.replacement.clone(),
                })
                .collect(),
            title: fix.title.clone(),
        }
    }

    /// The payload a diagnostic carried, if it carried one.
    fn from_diagnostic(diagnostic: &Diagnostic) -> Option<Self> {
        let data = diagnostic.data.clone()?;
        serde_json::from_value(data).ok()
    }

    /// The quick fix for `diagnostic` on `uri`.
    fn into_code_action(self, uri: &Uri, diagnostic: Diagnostic) -> CodeAction {
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), self.edits);
        CodeAction {
            title: self.title,
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic]),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            is_preferred: Some(true),
            ..Default::default()
        }
    }
}

impl Backend {
    pub(crate) async fn handle_code_action(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let actions: Vec<CodeActionOrCommand> = params
            .context
            .diagnostics
            .into_iter()
            .filter_map(|diagnostic| {
                let data = FixData::from_diagnostic(&diagnostic)?;
                Some(CodeActionOrCommand::CodeAction(
                    data.into_code_action(&uri, diagnostic),
                ))
            })
            .collect();
        Ok(Some(actions))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn diagnostic_with_data(data: Option<serde_json::Value>) -> Diagnostic {
        Diagnostic {
            message: "boom".to_string(),
            data,
            ..Default::default()
        }
    }

    #[test]
    fn payload_round_trips_through_data() {
        let payload = FixData {
            edits: vec![TextEdit {
                range: Range::new(Position::new(1, 2), Position::new(1, 8)),
                new_text: "if not".to_string(),
            }],
            title: "Replace `unless` with `if not`".to_string(),
        };
        let data = serde_json::to_value(&payload).unwrap();
        let read = FixData::from_diagnostic(&diagnostic_with_data(Some(data))).unwrap();
        assert_eq!(read.title, payload.title);
        assert_eq!(read.edits, payload.edits);
    }

    #[test]
    fn diagnostic_without_data_has_no_fix() {
        assert!(FixData::from_diagnostic(&diagnostic_with_data(None)).is_none());
    }

    #[test]
    fn code_action_edits_the_request_uri() {
        let uri = Uri::from_str("file:///proj/src/main.koja").unwrap();
        let payload = FixData {
            edits: vec![TextEdit {
                range: Range::default(),
                new_text: String::new(),
            }],
            title: "Drop the return value".to_string(),
        };
        let action = payload.into_code_action(&uri, diagnostic_with_data(None));
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.is_preferred, Some(true));
        let changes = action.edit.unwrap().changes.unwrap();
        assert_eq!(changes[&uri].len(), 1);
    }
}
