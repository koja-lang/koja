//! Questions about a typechecked Koja program, asked by position or
//! by symbol.
//!
//! The crate reads the annotations typecheck left on the AST and the
//! [`GlobalRegistry`] it built. It never resolves a name itself. Every
//! answer is a [`Span`], a registry id, a [`ResolvedType`], or a
//! string, so an editor server, a documentation tool, or a shell can
//! all sit on top of it without protocol types leaking in.
//!
//! [`Analysis`] is the entry point. Build one from a [`CheckedProgram`]
//! or, when typecheck reported errors, from the [`CheckFailure`]. The
//! failure path still carries every stamp typecheck wrote, so
//! navigation keeps working while the program does not compile.
//!
//! [`Span`]: koja_ast::span::Span
//! [`ResolvedType`]: koja_ast::identifier::ResolvedType

pub mod display;
pub mod docs;
pub mod expr_at;
pub mod index;
pub mod position;
pub mod rename;
pub mod signature;
pub mod symbol;
pub mod visit;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use koja_ast::ast::File;
use koja_ast::span::FileId;
use koja_typecheck::{CheckFailure, CheckedProgram, GlobalRegistry};

pub use index::{Occurrence, ReferenceIndex, Role, SymbolKey};
pub use symbol::Symbol;

/// A typechecked program viewed for queries. Borrows the files and
/// the registry from whichever of [`CheckedProgram`] or
/// [`CheckFailure`] produced them.
pub struct Analysis<'a> {
    files: Vec<&'a File>,
    by_id: HashMap<FileId, usize>,
    pub registry: &'a GlobalRegistry,
    /// File table indexed by [`FileId`].
    pub source_paths: &'a [PathBuf],
    /// True when typecheck reported at least one error. Queries that
    /// must not act on a half-resolved program, such as rename, check
    /// this.
    pub has_errors: bool,
}

impl<'a> Analysis<'a> {
    /// Build from loose parts. `files` are the post-typecheck ASTs.
    pub fn new(
        files: impl IntoIterator<Item = &'a File>,
        registry: &'a GlobalRegistry,
        source_paths: &'a [PathBuf],
        has_errors: bool,
    ) -> Self {
        let files: Vec<&'a File> = files.into_iter().collect();
        let by_id = files
            .iter()
            .enumerate()
            .map(|(i, file)| (file.span.file, i))
            .collect();
        Self {
            files,
            by_id,
            registry,
            source_paths,
            has_errors,
        }
    }

    pub fn from_checked(checked: &'a CheckedProgram) -> Self {
        let files = checked.packages.iter().flat_map(|pkg| pkg.files.iter());
        Self::new(files, &checked.registry, &checked.source_paths, false)
    }

    /// `None` when the failure came from the parser, which leaves no
    /// registry behind.
    pub fn from_failure(failure: &'a CheckFailure) -> Option<Self> {
        let registry = failure.registry.as_deref()?;
        let files = failure.partial.iter().map(|parsed| &parsed.ast);
        Some(Self::new(files, registry, &failure.source_paths, true))
    }

    /// Every file in the program, in table order.
    pub fn files(&self) -> &[&'a File] {
        &self.files
    }

    pub fn file(&self, id: FileId) -> Option<&'a File> {
        self.by_id.get(&id).map(|&i| self.files[i])
    }

    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.source_paths
            .iter()
            .position(|candidate| candidate == path)
            .map(|i| FileId(i as u32))
    }

    pub fn path_of(&self, file: FileId) -> Option<&'a Path> {
        self.source_paths.get(file.0 as usize).map(PathBuf::as_path)
    }
}
