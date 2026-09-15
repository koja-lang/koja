//! Rename planning. Decides whether the symbol under the cursor can
//! be renamed and, when it can, lists every span to rewrite.
//!
//! Rename edits source the user cannot see, so it refuses whenever
//! the answer might be incomplete. A program with type errors may
//! have unresolved references. A symbol declared outside the project
//! cannot have its declaration edited. A protocol method
//! implementation is named by the protocol. A `?`-suffixed name must
//! stay a predicate.

use std::fmt;

use koja_ast::identifier::Identifier;
use koja_ast::span::{FileId, Span};
use koja_typecheck::{GlobalKind, KEYWORDS};

use crate::Analysis;
use crate::index::{ReferenceIndex, SymbolKey};

/// Why a rename did not go ahead. `Display` gives the message an
/// editor shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameRefusal {
    /// Typecheck reported errors, so some references may be missing.
    ProgramHasErrors,
    /// The declaration is in a file outside the project, or was
    /// synthesized by the compiler.
    DeclaredOutsideProject,
    /// The symbol kind is never renamed. The payload names it.
    NotRenamable(&'static str),
    /// A reference spells the symbol under another name, such as a
    /// file `alias`.
    Aliased,
    /// The replacement is not one identifier of the right case.
    InvalidName(String),
}

impl fmt::Display for RenameRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProgramHasErrors => f.write_str("cannot rename while the program has errors"),
            Self::DeclaredOutsideProject => {
                f.write_str("cannot rename a symbol declared outside this project")
            }
            Self::NotRenamable(what) => write!(f, "cannot rename {what}"),
            Self::Aliased => {
                f.write_str("cannot rename a symbol that is referenced through an alias")
            }
            Self::InvalidName(reason) => write!(f, "invalid name: {reason}"),
        }
    }
}

impl std::error::Error for RenameRefusal {}

/// A rename that can go ahead.
#[derive(Clone, Debug)]
pub struct Rename {
    /// The current name, offered to the editor as the placeholder.
    pub name: String,
    /// The declaration's name span.
    pub declaration: Span,
    /// Every span to replace, the declaration included.
    pub spans: Vec<Span>,
}

/// Check that `key` can be renamed and collect its spans. Runs the
/// checks that do not depend on the new name, so an editor can call
/// it for `prepareRename`. `is_project_file` says which files the
/// user owns.
pub fn prepare_rename(
    analysis: &Analysis<'_>,
    index: &ReferenceIndex,
    key: SymbolKey,
    is_project_file: impl Fn(FileId) -> bool,
) -> Result<Rename, RenameRefusal> {
    if analysis.has_errors {
        return Err(RenameRefusal::ProgramHasErrors);
    }
    let declaration = index
        .declaration(key)
        .ok_or(RenameRefusal::DeclaredOutsideProject)?;
    if !is_project_file(declaration.span.file) {
        return Err(RenameRefusal::DeclaredOutsideProject);
    }
    if declaration.name == "self" {
        return Err(RenameRefusal::NotRenamable("`self`"));
    }
    if let SymbolKey::Global(id) = key {
        check_global_kind(analysis, id)?;
    }

    let mut spans = Vec::new();
    for occurrence in index.occurrences(key) {
        if occurrence.name != declaration.name {
            return Err(RenameRefusal::Aliased);
        }
        spans.push(occurrence.span);
    }
    Ok(Rename {
        name: declaration.name.clone(),
        declaration: declaration.span,
        spans,
    })
}

/// Builtins are compiler-owned. A method that implements a protocol
/// requirement takes its name from the protocol.
fn check_global_kind(
    analysis: &Analysis<'_>,
    id: koja_ast::identifier::GlobalRegistryId,
) -> Result<(), RenameRefusal> {
    let registry = analysis.registry;
    let Some(entry) = registry.get(id) else {
        return Err(RenameRefusal::DeclaredOutsideProject);
    };
    let GlobalKind::Function(definition) = &entry.kind else {
        if matches!(entry.kind, GlobalKind::Builtin(_)) {
            return Err(RenameRefusal::NotRenamable("a builtin type"));
        }
        return Ok(());
    };
    let path = entry.identifier.path();
    let Some((method, owner_path)) = path.split_last() else {
        return Ok(());
    };
    if owner_path.is_empty() {
        return Ok(());
    }
    let owner = Identifier::new(entry.identifier.package(), owner_path.to_vec());
    if let Some((owner_id, _)) = registry.lookup(&owner)
        && registry
            .protocol_declaring_method(owner_id, method, definition.arity)
            .is_some()
    {
        return Err(RenameRefusal::NotRenamable(
            "a method that implements a protocol",
        ));
    }
    Ok(())
}

/// Check that `new_name` is one identifier in the same case class
/// as `old_name`. Types start uppercase, everything else starts
/// lowercase or with `_`. A trailing `?` is allowed on lowercase
/// names only.
pub fn validate_new_name(old_name: &str, new_name: &str) -> Result<(), RenameRefusal> {
    let invalid = |reason: &str| Err(RenameRefusal::InvalidName(reason.to_string()));
    let mut chars = new_name.chars();
    let Some(first) = chars.next() else {
        return invalid("a name cannot be empty");
    };
    let old_is_type = old_name.starts_with(|c: char| c.is_ascii_uppercase());
    let new_is_type = first.is_ascii_uppercase();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return invalid("a name must start with a letter or `_`");
    }
    if old_is_type != new_is_type {
        return if old_is_type {
            invalid("a type name must start with an uppercase letter")
        } else {
            invalid("this name must start with a lowercase letter or `_`")
        };
    }
    let body = new_name.strip_suffix('?').unwrap_or(new_name);
    if body.is_empty() {
        return invalid("a name cannot be only `?`");
    }
    if new_is_type && body.len() != new_name.len() {
        return invalid("a type name cannot end in `?`");
    }
    if !body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return invalid(
            "a name may contain letters, digits, and `_`, with an optional trailing `?`",
        );
    }
    if KEYWORDS.contains(&new_name) || new_name == "self" || new_name == "Self" {
        return invalid("that name is a keyword");
    }
    Ok(())
}
