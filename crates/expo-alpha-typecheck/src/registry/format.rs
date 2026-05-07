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

use super::{
    EnumDefinition, FunctionSignature, GlobalKind, GlobalRegistry, ProtocolDefinition,
    ProtocolImplDefinition, ResolvedEnumVariant, ResolvedProtocolMethod, ResolvedVariantData,
    StructDefinition,
};

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
        GlobalKind::Enum(None) => "enum".to_string(),
        GlobalKind::Enum(Some(def)) => format_enum(def, registry),
        GlobalKind::Function(None) => "fn <unlifted>".to_string(),
        GlobalKind::Function(Some(sig)) => format_signature(sig, registry),
        GlobalKind::Protocol(None) => "protocol".to_string(),
        GlobalKind::Protocol(Some(def)) => format_protocol(def, registry),
        GlobalKind::ProtocolImpl(None) => "impl <unlifted>".to_string(),
        GlobalKind::ProtocolImpl(Some(def)) => format_protocol_impl(def, registry),
        GlobalKind::Struct(None) => "struct".to_string(),
        GlobalKind::Struct(Some(def)) => format_struct(def, registry),
    }
}

fn format_protocol_impl(def: &ProtocolImplDefinition, registry: &GlobalRegistry) -> String {
    let target = format_resolved(&def.target, registry);
    let protocol = format_resolved(&def.protocol, registry);
    let methods = def
        .method_ids
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    format!("impl {protocol} for {target} {{{methods}}}")
}

fn format_enum(def: &EnumDefinition, registry: &GlobalRegistry) -> String {
    let variants = def
        .variants
        .iter()
        .map(|v| format_variant(v, registry))
        .collect::<Vec<_>>()
        .join(", ");
    format!("enum {{{variants}}}")
}

fn format_variant(variant: &ResolvedEnumVariant, registry: &GlobalRegistry) -> String {
    match &variant.data {
        ResolvedVariantData::Struct(fields) => {
            let payload = fields
                .iter()
                .map(|f| format!("{}: {}", f.name, format_resolved(&f.ty, registry)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}{{{payload}}}", variant.name)
        }
        ResolvedVariantData::Tuple(types) => {
            let payload = types
                .iter()
                .map(|ty| format_resolved(ty, registry))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({payload})", variant.name)
        }
        ResolvedVariantData::Unit => variant.name.clone(),
    }
}

fn format_protocol(def: &ProtocolDefinition, registry: &GlobalRegistry) -> String {
    let methods = def
        .methods
        .iter()
        .map(|method| format_protocol_method(method, registry))
        .collect::<Vec<_>>()
        .join(", ");
    format!("protocol {{{methods}}}")
}

fn format_protocol_method(method: &ResolvedProtocolMethod, registry: &GlobalRegistry) -> String {
    let suffix = if method.has_default { " = default" } else { "" };
    let receiver = match method.dispatch {
        super::Dispatch::Instance => "self",
        super::Dispatch::Static => "",
    };
    let non_self = method
        .non_self_params
        .iter()
        .map(|p| format!("{}: {}", p.name, format_resolved(&p.ty, registry)))
        .collect::<Vec<_>>()
        .join(", ");
    let params = match (receiver, non_self.as_str()) {
        ("", rest) => rest.to_string(),
        (recv, "") => recv.to_string(),
        (recv, rest) => format!("{recv}, {rest}"),
    };
    format!(
        "fn {}({params}) -> {}{suffix}",
        method.name,
        format_resolved(&method.return_type, registry),
    )
}

fn format_struct(def: &StructDefinition, registry: &GlobalRegistry) -> String {
    let fields = def
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name, format_resolved(&f.ty, registry)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("struct {{{fields}}}")
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
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => format!("<id {id}>"),
        },
        Resolution::Local(local_id) => format!("<local {local_id}>"),
        Resolution::TypeParam { owner, index } => registry
            .type_param_name(owner, index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        Resolution::Unresolved => "<unresolved>".to_string(),
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
