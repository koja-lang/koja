//! File-level `alias` declaration resolution.
//!
//! Validates each `alias` against the known package types, reports duplicate
//! `local_name` entries, and inserts resolved aliases into the context's
//! `type_aliases` and `file_aliases` maps so they participate in subsequent
//! bare-name lookups.

use std::collections::BTreeMap;

use expo_ast::ast::{Item, Module};
use expo_ast::span::Span;

use crate::context::TypeContext;
use crate::types::{Type, TypeIdentifier};

/// Resolves `alias` declarations in a file, validating against known package
/// types and inserting resolved aliases into `ctx.type_aliases` so they are
/// visible during type checking of this file. Duplicate `local_name` entries
/// within the same file are reported as errors so two `alias`es never
/// silently shadow each other (e.g. `alias alpha.Config` + `alias beta.Config`).
pub fn resolve_file_aliases(file: &Module, ctx: &mut TypeContext) {
    let mut seen: BTreeMap<String, Span> = BTreeMap::new();
    for item in &file.items {
        if let Item::Alias(a) = item {
            if a.path.len() != 2 {
                ctx.error(
                    format!(
                        "alias path must be `package.Type`, got `{}`",
                        a.path.join(".")
                    ),
                    a.span,
                );
                continue;
            }
            let pkg = &a.path[0];
            let type_name = &a.path[1];
            if !ctx.is_package_type(pkg, type_name) {
                ctx.error(format!("unknown package type `{pkg}.{type_name}`"), a.span);
                continue;
            }
            if let Some(prev_span) = seen.get(&a.local_name) {
                ctx.error_with_hint(
                    format!(
                        "duplicate alias `{}`: a local name can refer to only one type",
                        a.local_name
                    ),
                    format!(
                        "the previous alias for `{}` was at line {}",
                        a.local_name, prev_span.start.line
                    ),
                    a.span,
                );
                continue;
            }
            seen.insert(a.local_name.clone(), a.span);
            let resolved = Type::Named {
                identifier: TypeIdentifier::new(pkg, type_name),
                type_args: vec![],
            };
            ctx.file_aliases
                .insert(a.local_name.clone(), resolved.clone());
            ctx.type_aliases.insert(a.local_name.clone(), resolved);
        }
    }
}
