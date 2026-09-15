//! The symbol under a cursor.

use koja_ast::identifier::ResolvedType;
use koja_ast::span::FileId;
use koja_typecheck::RegistryEntry;

use crate::Analysis;
use crate::index::{ReferenceIndex, SymbolKey};

/// What sits under the cursor, with enough detail for hover and
/// go-to-definition. The registry entry is borrowed for globals so
/// callers render signatures without a second lookup.
#[derive(Debug)]
pub struct Symbol<'a> {
    pub key: SymbolKey,
    pub name: String,
    pub kind: SymbolKind<'a>,
}

#[derive(Debug)]
pub enum SymbolKind<'a> {
    Global(&'a RegistryEntry),
    /// A local binding. `ty` prefers the declaration's stamped type
    /// and falls back to the type at the cursor.
    Local {
        ty: Option<ResolvedType>,
    },
    /// A type parameter. The owning declaration is in the key.
    TypeParam,
}

/// The symbol at the 1-indexed cursor position in `file`, or `None`
/// when the cursor is not on a name.
pub fn symbol_at<'a>(
    analysis: &Analysis<'a>,
    index: &ReferenceIndex,
    file: FileId,
    line: u32,
    col: u32,
) -> Option<Symbol<'a>> {
    let occurrence = index.occurrence_at(file, line, col)?;
    let kind = match occurrence.key {
        SymbolKey::Global(id) => SymbolKind::Global(analysis.registry.get(id)?),
        SymbolKey::Local { .. } => {
            let ty = index
                .declaration(occurrence.key)
                .and_then(|declaration| declaration.ty.clone())
                .or_else(|| occurrence.ty.clone());
            SymbolKind::Local { ty }
        }
        SymbolKey::TypeParam { .. } => SymbolKind::TypeParam,
    };
    Some(Symbol {
        key: occurrence.key,
        name: occurrence.name.clone(),
        kind,
    })
}
