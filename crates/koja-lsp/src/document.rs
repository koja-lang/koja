//! The cached analysis of one open document, the position helpers
//! that read it, and the prelude every handler opens with.
//!
//! A handler that works on the AST alone, such as folding, takes
//! [`Backend::with_state`]. One that asks about types takes
//! [`Backend::with_analysis`] and gets a [`Doc`], the analysis
//! opened on the active file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::File;
use koja_ast::span::{FileId, Span};
use koja_parser::{ParsedProgram, SourceTable};
use koja_query::symbol::symbol_at;
use koja_query::{Analysis, ReferenceIndex, Symbol};
use koja_typecheck::GlobalRegistry;

use crate::backend::Backend;
use crate::convert::{PositionEncoding, Positions, path_to_uri};

/// Cached analysis of a single open document, from its last
/// completed run. The text the editor holds right now lives in
/// [`Buffers`](crate::buffer::Buffers) and may be newer.
///
/// `parsed` holds every file in the bundle as typecheck left it,
/// whether typecheck succeeded or reported errors. `registry` is the
/// registry from the same run, `None` only when parsing failed. The
/// two together back every navigation query, so hover, definition,
/// and references keep working while the program has type errors.
pub(crate) struct DocumentState {
    pub(crate) active_path: PathBuf,
    pub(crate) active_package: String,
    /// The client's character encoding, fixed at `initialize`.
    pub(crate) encoding: PositionEncoding,
    pub(crate) parsed: ParsedProgram,
    pub(crate) registry: Option<Box<GlobalRegistry>>,
    /// The path and text of every bundled file as this run saw it,
    /// in the parse order spans refer to. Position conversion needs
    /// the line a span sits on.
    pub(crate) sources: SourceTable,
    /// True when the run produced an error diagnostic. Rename
    /// refuses to act on such a program.
    pub(crate) has_errors: bool,
    /// Paths of the user's project files, the active buffer
    /// included. Everything else in the bundle is stdlib.
    pub(crate) project_paths: HashSet<PathBuf>,
    /// The directory holding the project's `koja.toml`, `None` for a
    /// file outside any project.
    pub(crate) project_root: Option<PathBuf>,
    /// Reference index over `project_paths`.
    pub(crate) index: ReferenceIndex,
}

impl DocumentState {
    /// The currently-edited file.
    pub(crate) fn active_file(&self) -> Option<&File> {
        self.parsed
            .get(&self.active_path)
            .map(|parsed_file| &parsed_file.ast)
    }

    /// Query view over the cached program. `None` when parsing
    /// failed and no registry exists.
    pub(crate) fn analysis(&self) -> Option<Analysis<'_>> {
        let registry = self.registry.as_deref()?;
        Some(Analysis::new(
            self.parsed.iter().map(|parsed_file| &parsed_file.ast),
            registry,
            &self.sources,
            self.has_errors,
        ))
    }

    pub(crate) fn is_project_file(&self, path: &Path) -> bool {
        self.project_paths.contains(path)
    }

    /// The id of the active file in this run's file table.
    pub(crate) fn active_file_id(&self) -> Option<FileId> {
        self.sources.file_id(&self.active_path)
    }

    /// Position conversion over the text of `file`. An id outside the
    /// table converts over the active file, the same fallback the
    /// diagnostics grouping uses.
    pub(crate) fn positions(&self, file: FileId) -> Positions<'_> {
        let text = self
            .sources
            .text_of(file)
            .or_else(|| self.sources.text_of(self.active_file_id()?))
            .unwrap_or("");
        Positions::new(self.encoding, text)
    }

    /// Position conversion over the active file.
    pub(crate) fn active_positions(&self) -> Positions<'_> {
        self.positions(self.active_file_id().unwrap_or(FileId::UNKNOWN))
    }

    /// The LSP range of a span, over the text of the span's file.
    pub(crate) fn range_of(&self, span: &Span) -> Range {
        self.positions(span.file).range(span)
    }
}

impl std::fmt::Debug for DocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentState").finish()
    }
}

/// A document's analysis opened on its active file. Built by
/// [`Backend::with_analysis`] for the handlers that read types.
pub(crate) struct Doc<'a> {
    pub(crate) analysis: Analysis<'a>,
    /// The active file's id in this run's table.
    pub(crate) file: FileId,
    /// Position conversion over the active file.
    pub(crate) positions: Positions<'a>,
    pub(crate) state: &'a DocumentState,
}

impl<'a> Doc<'a> {
    /// The active file as typecheck left it.
    pub(crate) fn active_file(&self) -> Option<&'a File> {
        self.analysis.file(self.file)
    }

    /// The Koja line and column, both counted from 1, under an LSP
    /// position in the active file.
    pub(crate) fn line_column(&self, position: Position) -> (u32, u32) {
        self.positions.line_column(position)
    }

    /// The symbol under an LSP position in the active file.
    pub(crate) fn symbol_at(&self, position: Position) -> Option<Symbol<'a>> {
        let (line, column) = self.line_column(position);
        symbol_at(&self.analysis, &self.state.index, self.file, line, column)
    }

    /// The LSP range of a span, over the text of the span's file.
    pub(crate) fn range_of(&self, span: &Span) -> Range {
        self.state.range_of(span)
    }

    /// The URI of the file a span points into, or `fallback` when
    /// the file id does not resolve.
    pub(crate) fn uri_of(&self, file: FileId, fallback: &Uri) -> Uri {
        self.analysis
            .path_of(file)
            .and_then(path_to_uri)
            .unwrap_or_else(|| fallback.clone())
    }
}

impl Backend {
    /// Run `f` on the cached state of `uri`. `Ok(None)` when the
    /// document is not open or has no completed run yet.
    pub(crate) async fn with_state<T>(
        &self,
        uri: &Uri,
        f: impl FnOnce(&DocumentState) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let docs = self.documents.read().await;
        match docs.get(uri) {
            Some(state) => f(state),
            None => Ok(None),
        }
    }

    /// Run `f` on the analysis of `uri`, opened on its active file.
    /// `Ok(None)` when there is no state, when parsing failed and
    /// left no registry, or when the active file is not in the
    /// run's table.
    pub(crate) async fn with_analysis<T>(
        &self,
        uri: &Uri,
        f: impl FnOnce(Doc<'_>) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        self.with_state(uri, |state| {
            let Some(analysis) = state.analysis() else {
                return Ok(None);
            };
            let Some(file) = analysis.file_id(&state.active_path) else {
                return Ok(None);
            };
            let positions = state.positions(file);
            f(Doc {
                analysis,
                file,
                positions,
                state,
            })
        })
        .await
    }
}
