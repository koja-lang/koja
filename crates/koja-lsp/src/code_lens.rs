//! A run-test lens on each test in the active file.
//!
//! The lens carries the client command `koja.runTest` with the
//! project root and the `file:line` spec id that `koja test --only`
//! takes. The server does not run anything. The editor extension
//! owns the command so the result lands in its test UI. A client
//! that does not register the command shows a lens that does
//! nothing.

use std::path::Path;

use serde_json::Value;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::span::Span;
use koja_query::test_sites::tests_in_file;

use crate::backend::Backend;
use crate::convert::Positions;

/// The command id the client registers.
pub(crate) const RUN_TEST_COMMAND: &str = "koja.runTest";

impl Backend {
    pub(crate) async fn handle_code_lens(
        &self,
        params: CodeLensParams,
    ) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };
        let Some(root) = state.project_root.as_deref() else {
            return Ok(None);
        };
        let Some(analysis) = state.analysis() else {
            return Ok(None);
        };
        let Some(file) = analysis.file_id(&state.active_path) else {
            return Ok(None);
        };
        let Some(relative) = relative_path(root, &state.active_path) else {
            return Ok(None);
        };

        let positions = state.positions(file);
        let lenses = tests_in_file(&analysis, file)
            .into_iter()
            .map(|span| run_test_lens(root, &relative, &span, &positions))
            .collect();
        Ok(Some(lenses))
    }
}

/// `path` relative to `root` with forward slashes, the spec id form.
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let segments: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(segments.join("/"))
}

/// One lens on the test's first line.
fn run_test_lens(root: &Path, relative: &str, span: &Span, positions: &Positions<'_>) -> CodeLens {
    let line = span.start.line;
    let start = positions.position(&span.start);
    CodeLens {
        range: Range::new(start, start),
        command: Some(Command {
            title: "Run test".to_string(),
            command: RUN_TEST_COMMAND.to_string(),
            arguments: Some(vec![
                Value::String(root.to_string_lossy().into_owned()),
                Value::String(format!("{relative}:{line}")),
            ]),
        }),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use koja_ast::span::{FileId, Position as SpanPosition};

    use crate::convert::PositionEncoding;

    use super::*;

    #[test]
    fn relative_path_uses_forward_slashes() {
        let root = PathBuf::from("/proj");
        let path = PathBuf::from("/proj/test/stack_test.koja");
        assert_eq!(
            relative_path(&root, &path).as_deref(),
            Some("test/stack_test.koja")
        );
        assert!(relative_path(&root, Path::new("/elsewhere/a.koja")).is_none());
    }

    #[test]
    fn lens_carries_root_and_spec_id() {
        let at = |line, column| SpanPosition {
            offset: 0,
            line,
            column,
        };
        let span = Span::new(at(14, 3), at(20, 6), FileId(0));
        let text = "\n".repeat(13) + "  test \"x\"\n";
        let positions = Positions::new(PositionEncoding::Utf16, &text);
        let lens = run_test_lens(
            Path::new("/proj"),
            "test/stack_test.koja",
            &span,
            &positions,
        );
        assert_eq!(lens.range.start, Position::new(13, 2));
        let command = lens.command.unwrap();
        assert_eq!(command.command, RUN_TEST_COMMAND);
        assert_eq!(
            command.arguments.unwrap(),
            vec![
                Value::String("/proj".to_string()),
                Value::String("test/stack_test.koja:14".to_string()),
            ]
        );
    }
}
