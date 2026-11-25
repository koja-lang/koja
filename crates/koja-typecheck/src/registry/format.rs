//! Compact tree-style registry rendering for
//! `koja check --emit-ast` as a sidecar to the AST printer.
//!
//! Format mirrors [`koja_ast::format_file`]: a header line with the
//! entry count, then one indented `<id> <kind> <qualified_name> @<span>`
//! row per entry, ordered by id so AST `<id>` references line up
//! one-to-one with rows here. Function entries render their signature
//! inline (`fn (p: Global.Int) -> Global.Int`). Unlifted functions
//! render as `fn <unlifted>`.
//!
//! Always trailing-newline-terminated. Empty registries render just
//! the header.

use std::fmt::Write as _;

use koja_ast::identifier::ResolvedType;

use super::{
    Conformance, ConformanceScope, ConstantDefinition, DisplayStyle, EnumDefinition,
    FunctionSignature, GlobalKind, GlobalRegistry, ProtocolDefinition, ResolvedEnumVariant,
    ResolvedProtocolMethod, ResolvedVariantData, StructDefinition, display_type,
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
            format_kind(&entry.kind, &entry.type_params, registry),
            entry.identifier.qualified_name(),
            entry.span,
        )
        .expect("writing into a String cannot fail");
    }
    out
}

fn format_kind(kind: &GlobalKind, type_params: &[String], registry: &GlobalRegistry) -> String {
    match kind {
        GlobalKind::Builtin(def) => format!(
            "builtin{}",
            format_conformances(&def.conformances, type_params, registry)
        ),
        GlobalKind::Constant(None) => "const <unlifted>".to_string(),
        GlobalKind::Constant(Some(def)) => format_constant(def, registry),
        GlobalKind::Enum(None) => "enum".to_string(),
        GlobalKind::Enum(Some(def)) => format_enum(def, type_params, registry),
        GlobalKind::Function(definition) => definition.signature.as_ref().map_or_else(
            || "fn <unlifted>".to_string(),
            |sig| format_signature(sig, registry),
        ),
        GlobalKind::Protocol(None) => "protocol".to_string(),
        GlobalKind::Protocol(Some(def)) => format_protocol(def, registry),
        GlobalKind::Struct(None) => "struct".to_string(),
        GlobalKind::Struct(Some(def)) => format_struct(def, type_params, registry),
        GlobalKind::TypeAlias(None) => "type alias <unlifted>".to_string(),
        GlobalKind::TypeAlias(Some(definition)) => {
            format!(
                "type = {}",
                display_type(&definition.expansion, registry, DisplayStyle::Qualified)
            )
        }
    }
}

fn format_enum(def: &EnumDefinition, type_params: &[String], registry: &GlobalRegistry) -> String {
    let variants = def
        .variants
        .iter()
        .map(|v| format_variant(v, registry))
        .collect::<Vec<_>>()
        .join(", ");
    let conformances = format_conformances(&def.conformances, type_params, registry);
    format!("enum {{{variants}}}{conformances}")
}

fn format_variant(variant: &ResolvedEnumVariant, registry: &GlobalRegistry) -> String {
    match &variant.data {
        ResolvedVariantData::Struct(fields) => {
            let payload = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}",
                        f.name,
                        display_type(&f.ty, registry, DisplayStyle::Qualified)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}{{{payload}}}", variant.name)
        }
        ResolvedVariantData::Tuple(types) => {
            let payload = types
                .iter()
                .map(|ty| display_type(ty, registry, DisplayStyle::Qualified))
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
        .map(|p| {
            format!(
                "{}: {}",
                p.name,
                display_type(&p.ty, registry, DisplayStyle::Qualified)
            )
        })
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
        display_type(&method.return_type, registry, DisplayStyle::Qualified),
    )
}

fn format_struct(
    def: &StructDefinition,
    type_params: &[String],
    registry: &GlobalRegistry,
) -> String {
    let fields = def
        .fields
        .iter()
        .map(|f| {
            format!(
                "{}: {}",
                f.name,
                display_type(&f.ty, registry, DisplayStyle::Qualified)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let conformances = format_conformances(&def.conformances, type_params, registry);
    format!("struct {{{fields}}}{conformances}")
}

/// Render a struct/enum's protocol conformance map as a trailing
/// `:[Proto, Proto<Arg>]` annotation. A concrete record on a
/// generic target names its instantiation as `Proto for Self<Int>`,
/// a conditional record names its bounds as `Proto for Self<T: P>`.
/// Empty maps render to nothing so unconformant types stay visually
/// identical to pre-conformance renders. Args are rendered via
/// [`display_type`] for stability.
fn format_conformances(
    conformances: &std::collections::BTreeMap<
        koja_ast::identifier::GlobalRegistryId,
        Vec<Conformance>,
    >,
    type_params: &[String],
    registry: &GlobalRegistry,
) -> String {
    if conformances.is_empty() {
        return String::new();
    }
    let entries = conformances
        .iter()
        .flat_map(|(protocol_id, records)| {
            let head = registry
                .get(*protocol_id)
                .map(|e| e.identifier.qualified_name())
                .unwrap_or_else(|| format!("<id {protocol_id}>"));
            records
                .iter()
                .map(move |record| format_conformance(&head, record, type_params, registry))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" :[{entries}]")
}

fn format_conformance(
    head: &str,
    record: &Conformance,
    type_params: &[String],
    registry: &GlobalRegistry,
) -> String {
    let protocol = if record.protocol_args.is_empty() {
        head.to_string()
    } else {
        format!(
            "{head}<{}>",
            format_type_args(&record.protocol_args, registry)
        )
    };
    match &record.scope {
        ConformanceScope::Concrete(args) if !args.is_empty() => {
            format!("{protocol} for Self<{}>", format_type_args(args, registry))
        }
        ConformanceScope::Parameterized { bounds } if bounds.iter().any(|b| !b.is_empty()) => {
            format!(
                "{protocol} for Self<{}>",
                format_bounded_params(bounds, type_params, registry)
            )
        }
        _ => protocol,
    }
}

/// Render a conditional record's param slots as `T: P & Q, U`,
/// using the target's own param names.
fn format_bounded_params(
    bounds: &[Vec<super::ResolvedProtocolBound>],
    type_params: &[String],
    registry: &GlobalRegistry,
) -> String {
    bounds
        .iter()
        .enumerate()
        .map(|(index, slot_bounds)| {
            let name = type_params
                .get(index)
                .map(String::as_str)
                .unwrap_or("?")
                .to_string();
            if slot_bounds.is_empty() {
                return name;
            }
            let rendered = slot_bounds
                .iter()
                .map(|bound| {
                    let head = registry
                        .get(bound.protocol_id)
                        .map(|e| e.identifier.qualified_name())
                        .unwrap_or_else(|| format!("<id {}>", bound.protocol_id));
                    if bound.args.is_empty() {
                        head
                    } else {
                        format!("{head}<{}>", format_type_args(&bound.args, registry))
                    }
                })
                .collect::<Vec<_>>()
                .join(" & ");
            format!("{name}: {rendered}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_type_args(args: &[ResolvedType], registry: &GlobalRegistry) -> String {
    args.iter()
        .map(|a| display_type(a, registry, DisplayStyle::Qualified))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_constant(def: &ConstantDefinition, registry: &GlobalRegistry) -> String {
    format!(
        "const: {}",
        display_type(&def.ty, registry, DisplayStyle::Qualified)
    )
}

fn format_signature(sig: &FunctionSignature, registry: &GlobalRegistry) -> String {
    let params = sig
        .params
        .iter()
        .map(|p| {
            format!(
                "{}: {}",
                p.name,
                display_type(&p.ty, registry, DisplayStyle::Qualified)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn ({params}) -> {}",
        display_type(&sig.return_type, registry, DisplayStyle::Qualified),
    )
}
