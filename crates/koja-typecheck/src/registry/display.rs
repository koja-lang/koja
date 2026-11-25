//! Text for a [`ResolvedType`]. Diagnostics, the registry dump, and
//! the editor tools all show a type through [`display_type`]. They
//! differ only in how they name the head of a named type, which is
//! the [`DisplayStyle`].

use koja_ast::identifier::{AnonymousKind, Identifier, Resolution, ResolvedType};

use super::GlobalRegistry;

/// How [`display_type`] names the head of a named type.
#[derive(Clone, Copy, Debug)]
pub enum DisplayStyle {
    /// `package.A.B`, the stable key. For the registry dump.
    Qualified,
    /// The last path segment. For hover and completion.
    Short,
    /// The name as source code spells it, such as `IO.Error`. For
    /// diagnostics, where two nested types that share a last segment
    /// must stay apart.
    Source,
}

/// Show `ty` as text. Each `Global` head gets its name from the
/// registry in the given `style`, and type arguments are spelled out
/// (`Option<fn (Int) -> Int>`). A head that has no name shows as a
/// placeholder in angle brackets, such as `<unresolved>`.
pub fn display_type(ty: &ResolvedType, registry: &GlobalRegistry, style: DisplayStyle) -> String {
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => format!(
            "fn ({}) -> {}",
            join_types(params, ", ", registry, style),
            display_type(ret, registry, style),
        ),
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            format!("({})", join_types(elements, ", ", registry, style))
        }
        ResolvedType::Anonymous(AnonymousKind::Union { members }) => {
            join_types(members, " | ", registry, style)
        }
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } => {
            let head = match registry.get(*id) {
                Some(entry) => head_name(&entry.identifier, style),
                None => format!("<id {id}>"),
            };
            if type_args.is_empty() {
                return head;
            }
            format!("{head}<{}>", join_types(type_args, ", ", registry, style))
        }
        ResolvedType::Named {
            resolution: Resolution::Local(local_id),
            ..
        } => format!("<local {local_id}>"),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => registry
            .type_param_name(*owner, *index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        ResolvedType::Named {
            resolution: Resolution::Unresolved,
            ..
        }
        | ResolvedType::Failed
        | ResolvedType::Unresolved => "<unresolved>".to_string(),
    }
}

/// The name of a `Global` head in the given `style`.
fn head_name(identifier: &Identifier, style: DisplayStyle) -> String {
    match style {
        DisplayStyle::Qualified => identifier.qualified_name(),
        DisplayStyle::Short => identifier.last().to_string(),
        DisplayStyle::Source => identifier.source_name(),
    }
}

/// Show each of `types` and join them with `separator`.
fn join_types(
    types: &[ResolvedType],
    separator: &str,
    registry: &GlobalRegistry,
    style: DisplayStyle,
) -> String {
    types
        .iter()
        .map(|ty| display_type(ty, registry, style))
        .collect::<Vec<_>>()
        .join(separator)
}
