//! Compact tree-style registry rendering for
//! `expo alpha check --emit-ast` as a sidecar to the AST printer.
//!
//! Format mirrors [`expo_ast::format_file`]: a header line with the
//! entry count, then one indented `<id> <kind> <qualified_name> @<span>`
//! row per entry, ordered by id so AST `<id>` references line up
//! one-to-one with rows here. Function entries render their signature
//! inline (`fn (p: Global.Int) -> Global.Int`); unlifted functions
//! render as `fn <unlifted>`.
//!
//! Always trailing-newline-terminated; empty registries render just
//! the header.

use std::fmt::Write as _;

use expo_ast::identifier::{Resolution, ResolvedType};

use super::{FunctionSignature, GlobalKind, GlobalRegistry};

pub fn format_registry(registry: &GlobalRegistry) -> String {
    let count = registry.len();
    let label = if count == 1 { "entry" } else { "entries" };
    let mut out = format!("Registry ({count} {label})\n");
    let mut rows: Vec<_> = registry.iter().collect();
    rows.sort_by_key(|(id, _)| *id);
    for (id, entry) in rows {
        writeln!(
            out,
            "  {id} {} {} @{}",
            format_kind(&entry.kind, registry),
            entry.identifier.qualified_name(),
            entry.span,
        )
        .expect("writing into a String cannot fail");
    }
    out
}

fn format_kind(kind: &GlobalKind, registry: &GlobalRegistry) -> String {
    match kind {
        GlobalKind::Enum => "enum".to_string(),
        GlobalKind::Function(None) => "fn <unlifted>".to_string(),
        GlobalKind::Function(Some(sig)) => format_signature(sig, registry),
        GlobalKind::Protocol => "protocol".to_string(),
        GlobalKind::Struct => "struct".to_string(),
    }
}

fn format_signature(sig: &FunctionSignature, registry: &GlobalRegistry) -> String {
    let params = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, format_resolved(&p.ty, registry)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn ({params}) -> {}",
        format_resolved(&sig.return_type, registry),
    )
}

fn format_resolved(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    let head = match ty.resolution {
        Resolution::Unresolved => "<unresolved>".to_string(),
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => format!("<id {id}>"),
        },
    };
    if ty.type_args.is_empty() {
        head
    } else {
        let args = ty
            .type_args
            .iter()
            .map(|arg| format_resolved(arg, registry))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{head}<{args}>")
    }
}
