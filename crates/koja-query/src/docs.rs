//! `@doc` text for a registry entry.

use koja_ast::ast::*;
use koja_ast::identifier::GlobalRegistryId;
use koja_ast::span::Span;

use crate::Analysis;

/// The `@doc` string on the declaration behind `id`, found by
/// matching the entry's `name_span` against the declaring file.
/// Synthesized declarations have no source and return `None`.
pub fn doc_for(analysis: &Analysis<'_>, id: GlobalRegistryId) -> Option<String> {
    let entry = analysis.registry.get(id)?;
    if entry.name_span.synthetic {
        return None;
    }
    let file = analysis.file(entry.name_span.file)?;
    doc_at(file, entry.name_span)
}

/// The `@doc` string on the declaration in `file` whose name sits at
/// `name_span`.
pub fn doc_at(file: &File, name_span: Span) -> Option<String> {
    doc_in_items(&file.items, name_span)
}

fn doc_in_items(items: &[Item], name_span: Span) -> Option<String> {
    for item in items {
        let found = match item {
            Item::Alias(_) | Item::Test(_) => None,
            Item::Builtin(decl) => doc_if_named(&decl.annotations, decl.name(), name_span)
                .or_else(|| doc_in_functions(&decl.functions, name_span)),
            Item::Constant(constant) => {
                doc_if_named(&constant.annotations, &constant.name, name_span)
            }
            Item::Enum(decl) => doc_if_named(&decl.annotations, decl.name(), name_span)
                .or_else(|| doc_in_functions(&decl.functions, name_span))
                .or_else(|| doc_in_items(&decl.nested, name_span)),
            Item::Extend(block) => doc_in_members(&block.members, name_span),
            Item::Function(function) => {
                doc_if_named(&function.annotations, &function.name, name_span)
            }
            Item::Impl(block) => doc_in_members(&block.members, name_span),
            Item::Protocol(decl) => {
                doc_if_named(&decl.annotations, &decl.name, name_span).or_else(|| {
                    decl.methods
                        .iter()
                        .find_map(|m| doc_if_named(&m.annotations, &m.name, name_span))
                })
            }
            Item::Struct(decl) => doc_if_named(&decl.annotations, decl.name(), name_span)
                .or_else(|| doc_in_functions(&decl.functions, name_span))
                .or_else(|| doc_in_items(&decl.nested, name_span)),
            Item::TypeAlias(alias) => doc_if_named(&alias.annotations, &alias.name, name_span),
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn doc_in_functions(functions: &[Function], name_span: Span) -> Option<String> {
    functions
        .iter()
        .find_map(|f| doc_if_named(&f.annotations, &f.name, name_span))
}

fn doc_in_members(members: &[ImplMember], name_span: Span) -> Option<String> {
    members.iter().find_map(|member| match member {
        ImplMember::Function(f) => doc_if_named(&f.annotations, &f.name, name_span),
        ImplMember::TypeAlias(alias) => doc_if_named(&alias.annotations, &alias.name, name_span),
    })
}

/// `Some(doc)` only when `name` sits at `name_span`. A declaration
/// without `@doc` at that span yields `None` and the search moves on,
/// which is harmless because name spans are unique within a file.
fn doc_if_named(annotations: &[Annotation], name: &Name, name_span: Span) -> Option<String> {
    if name.span != name_span {
        return None;
    }
    annotation_doc(annotations)
}

/// The string payload of a `@doc "text"` annotation.
pub fn annotation_doc(annotations: &[Annotation]) -> Option<String> {
    annotations.iter().find_map(|a| match a.kind() {
        AnnotationKind::Doc(DocAttr::Text(text)) => Some(text),
        _ => None,
    })
}
