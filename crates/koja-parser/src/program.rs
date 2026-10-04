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
//! `koja-fmt`'s string-in/string-out contract).
//!
//! [`SourceTable`] is the path and text of every file by [`FileId`],
//! taken from a [`ParsedProgram`] before typecheck consumes it. Every
//! later stage that renders a span or converts a position reads it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use koja_ast::ast::{Diagnostic, File, Severity};
use koja_ast::span::FileId;

use crate::ParseMode;
use crate::parser::parse_in_file_at;

/// Derive a package's PascalCase code namespace from its lowercase
/// snake_case manifest name (`my_app` -> `MyApp`). Total in this
/// direction only. Names whose namespace can't be derived (acronyms
/// like `JSON`) declare it explicitly in their manifest.
pub fn derive_namespace(name: &str) -> String {
    name.split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// A single source file ready to be parsed.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// The package namespace this file belongs to. For project files
    /// this is the manifest's declared or derived namespace (see
    /// [`derive_namespace`]). For stdlib files it is `"Global"`, and
    /// for single-file eval / run paths it is the file stem.
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
/// (typecheck, codegen) do not have to thread the per-file identity
/// alongside the AST. Every span carries `file`.
pub fn parse_file(source: SourceFile, mode: ParseMode, file: FileId) -> ParsedFile {
    let result = parse_in_file_at(&source.source, mode, file, Some(&source.path));
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
/// `files` is keyed by `path` (each file's stable identity). `order`
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

    /// Resolve a span's [`FileId`] to the owning file path.
    pub fn path_of(&self, file: FileId) -> Option<&Path> {
        self.order.get(file.0 as usize).map(PathBuf::as_path)
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

    /// The path and text of every file, indexed like the spans the
    /// parse produced. Take it before typecheck consumes the program,
    /// since the checked program keeps the paths but not the text.
    pub fn source_table(&self) -> SourceTable {
        SourceTable::new(
            self.order
                .iter()
                .map(|path| (path.clone(), self.files[path].source.as_str())),
        )
    }
}

/// The path and text of every source file, indexed by [`FileId`].
/// A span whose file id misses the table resolves to nothing, and
/// callers render it without a location or fall back to a file of
/// their own choosing.
#[derive(Clone, Debug, Default)]
pub struct SourceTable {
    files: Vec<(PathBuf, Arc<str>)>,
    /// When true, every span resolves to the one entry. Covers bare
    /// parses whose spans carry [`FileId::UNKNOWN`].
    single: bool,
}

impl SourceTable {
    pub fn new<S: Into<Arc<str>>>(files: impl IntoIterator<Item = (PathBuf, S)>) -> Self {
        Self {
            files: files
                .into_iter()
                .map(|(path, text)| (path, text.into()))
                .collect(),
            single: false,
        }
    }

    /// One-file table that attributes every span to that file.
    pub fn single(path: impl Into<PathBuf>, text: impl Into<Arc<str>>) -> Self {
        Self {
            files: vec![(path.into(), text.into())],
            single: true,
        }
    }

    pub fn path_of(&self, file: FileId) -> Option<&Path> {
        self.resolve(file).map(|(path, _)| path.as_path())
    }

    pub fn text_of(&self, file: FileId) -> Option<&str> {
        self.resolve(file).map(|(_, text)| &**text)
    }

    /// The id of the file at `path`.
    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.files
            .iter()
            .position(|(candidate, _)| candidate == path)
            .map(|index| FileId(index as u32))
    }

    /// Every path in id order.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(|(path, _)| path.as_path())
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    fn resolve(&self, file: FileId) -> Option<&(PathBuf, Arc<str>)> {
        let index = if self.single { 0 } else { file.0 as usize };
        self.files.get(index)
    }
}

/// Parses a list of source files in input order, producing a
/// [`ParsedProgram`]. All files are parsed in the same `mode`. Each
/// file gets a [`FileId`] equal to its index in `order`.
pub fn parse_program(sources: Vec<SourceFile>, mode: ParseMode) -> ParsedProgram {
    let mut files = BTreeMap::new();
    let mut order = Vec::with_capacity(sources.len());
    for (index, source) in sources.into_iter().enumerate() {
        let parsed = parse_file(source, mode, FileId(index as u32));
        order.push(parsed.path.clone());
        files.insert(parsed.path.clone(), parsed);
    }
    ParsedProgram { files, order }
}
