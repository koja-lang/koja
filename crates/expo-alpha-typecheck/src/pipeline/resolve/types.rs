//! Registry-backed [`ResolvedType`] predicates and rendering used
//! across the resolve sub-pass.
//!
//! "Primitive" here means a preloaded `Global.<name>` stdlib stub; see
//! [`GlobalRegistry::with_stdlib_stubs`]. Constructors for those
//! [`ResolvedType`]s live on the registry itself
//! ([`GlobalRegistry::primitive`]) since both `lift_signatures` and
//! `resolve` produce them.

use expo_ast::identifier::{Resolution, ResolvedType};

use crate::registry::GlobalRegistry;

/// Does `ty` resolve to the preloaded `Global.<name>` stdlib stub?
pub(super) fn is_primitive(ty: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    let Resolution::Global(id) = ty.resolution else {
        return false;
    };
    if !ty.type_args.is_empty() {
        return false;
    }
    let Some(entry) = registry.get(id) else {
        return false;
    };
    entry.identifier.is_in_global() && entry.identifier.last() == name
}

/// Human-readable rendering of a [`ResolvedType`] for diagnostics:
/// dereferences `Global` heads through the registry so users see
/// `Int` rather than an opaque `#0`.
pub(super) fn display_resolution(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty.resolution {
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.last().to_string(),
            None => format!("<id {id}>"),
        },
        Resolution::Local(local_id) => format!("<local {local_id}>"),
        Resolution::Unresolved => "<unresolved>".to_string(),
    }
}
