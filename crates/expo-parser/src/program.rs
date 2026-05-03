//! Single- and multi-file parsing entry points with a richer
//! input/output bundle than the bare-string [`crate::parse`] primitive.
//!
//! [`SourceFile`] carries a file's identity (`package`, `path`) alongside
//! its contents so [`parse_file`] can populate `ast.path` for downstream
//! diagnostic attribution without callers having to remember to set it.
//! The resulting [`ParsedFile`] keeps that identity attached to the AST
//! and parse diagnostics it produced.
//!
//! [`parse_program`] bundles a list of `SourceFile`s into a
//! [`ParsedProgram`] -- a path-keyed file bag with deterministic input
//! order, the canonical shape multi-file consumers (the driver pipeline)
//! thread through the rest of the compiler.
//!
//! The bare [`crate::parse`] primitive remains for callers without a
//! file context (REPL session input, proptest-synthesized strings,
//! `expo-fmt`'s string-in/string-out contract).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use expo_ast::ast::{Diagnostic, File, Severity};

use crate::parse;

/// A single source file ready to be parsed.
#[derive(Debug)]
pub struct SourceFile {
    /// The package this file belongs to. For project files this is the
    /// declared project name; for stdlib files this is `"Global"`; for
    /// single-file eval / run paths this is the file stem.
    pub package: String,
    /// Filesystem path (or a synthetic identifier like `<Global.io>` for
    /// embedded sources). Used for diagnostic attribution and as a
    /// stable identity across the pipeline.
    pub path: PathBuf,
    /// File contents.
    pub source: String,
}

/// The result of parsing a single [`SourceFile`].
#[derive(Debug)]
pub struct ParsedFile {
    pub package: String,
    pub path: PathBuf,
    pub source: String,
    pub ast: File,
    pub diagnostics: Vec<Diagnostic>,
}

impl ParsedFile {
    /// Whether the parse produced any error-severity diagnostics.
    /// Warnings alone do not count.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// Parses a single [`SourceFile`] into a [`ParsedFile`]. Populates
/// `ast.path` and `ast.package` from the source so downstream stages
/// (typecheck, codegen) don't have to thread the per-file identity
/// alongside the AST.
pub fn parse_file(source: SourceFile) -> ParsedFile {
    let result = parse(&source.source);
    let mut ast = result.ast;
    ast.path = Some(source.path.clone());
    ast.package = source.package.clone();
    ParsedFile {
        package: source.package,
        path: source.path,
        source: source.source,
        ast,
        diagnostics: result.errors,
    }
}

/// All parsed files in one program.
///
/// `files` is keyed by `path` (each file's stable identity); `order`
/// preserves the input order so downstream stages walk files
/// deterministically (today's convention: stdlib first, then project
/// files in scan order).
#[derive(Debug)]
pub struct ParsedProgram {
    pub files: BTreeMap<PathBuf, ParsedFile>,
    pub order: Vec<PathBuf>,
}

impl ParsedProgram {
    /// True when any file produced an error-severity diagnostic during
    /// parsing.
    pub fn has_errors(&self) -> bool {
        self.files.values().any(|f| f.has_errors())
    }

    /// Iterate files in input order.
    pub fn iter(&self) -> impl Iterator<Item = &ParsedFile> {
        self.order.iter().map(|p| &self.files[p])
    }

    pub fn get(&self, path: &Path) -> Option<&ParsedFile> {
        self.files.get(path)
    }

    pub fn get_mut(&mut self, path: &Path) -> Option<&mut ParsedFile> {
        self.files.get_mut(path)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// Parses a list of source files in input order, producing a
/// [`ParsedProgram`].
pub fn parse_program(sources: Vec<SourceFile>) -> ParsedProgram {
    let mut files = BTreeMap::new();
    let mut order = Vec::with_capacity(sources.len());
    for source in sources {
        let parsed = parse_file(source);
        order.push(parsed.path.clone());
        files.insert(parsed.path.clone(), parsed);
    }
    ParsedProgram { files, order }
}
