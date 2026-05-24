//! Human-readable rendering of pipeline types for hover and
//! completion. Mirrors `display_resolution` in `koja-typecheck`
//! (today private); duplicated here as a small local printer rather
//! than promoting the upstream helper.

use koja_ast::ast::PassMode;
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_typecheck::{
    FunctionSignature, GlobalRegistry, ResolvedEnumVariant, ResolvedParam, ResolvedProtocolMethod,
    ResolvedStructField, ResolvedVariantData,
};

/// Render a [`ResolvedType`] as a short, user-facing string.
pub(crate) fn format_resolved_type(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            let rendered_params = params
                .iter()
                .map(|p| {
                    let inner = format_resolved_type(&p.ty, registry);
                    match p.mode {
                        PassMode::Move => format!("move {inner}"),
                        PassMode::Borrow | PassMode::Copy => inner,
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fn ({rendered_params}) -> {}",
                format_resolved_type(ret, registry)
            )
        }
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } => {
            let head = registry
                .get(*id)
                .map(|entry| entry.identifier.last().to_string())
                .unwrap_or_else(|| format!("<id {id}>"));
            if type_args.is_empty() {
                head
            } else {
                let args = type_args
                    .iter()
                    .map(|a| format_resolved_type(a, registry))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{head}<{args}>")
            }
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
        | ResolvedType::Unresolved => "<unresolved>".to_string(),
        ResolvedType::Union(members) => members
            .iter()
            .map(|m| format_resolved_type(m, registry))
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

/// Render a [`FunctionSignature`] under `display_name`, including
/// type-parameter names from the owning [`RegistryEntry`].
pub(crate) fn format_function_signature(
    display_name: &str,
    sig: &FunctionSignature,
    type_params: &[String],
    registry: &GlobalRegistry,
) -> String {
    let tp = if type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", type_params.join(", "))
    };
    let params_str = sig
        .params
        .iter()
        .map(|p| format_param(p, registry))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn {display_name}{tp}({params_str}) -> {}",
        format_resolved_type(&sig.return_type, registry)
    )
}

fn format_param(p: &ResolvedParam, registry: &GlobalRegistry) -> String {
    let ty = format_resolved_type(&p.ty, registry);
    match p.mode {
        PassMode::Move => format!("move {}: {}", p.name, ty),
        _ => format!("{}: {}", p.name, ty),
    }
}

/// Render a struct's hover signature: `struct Name<Tp,...>` followed
/// by each field on its own indented line.
pub(crate) fn format_struct_def(
    name: &str,
    type_params: &[String],
    fields: &[ResolvedStructField],
    registry: &GlobalRegistry,
) -> String {
    let tp = if type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", type_params.join(", "))
    };
    let fields_str = fields
        .iter()
        .map(|f| format!("  {}: {}", f.name, format_resolved_type(&f.ty, registry)))
        .collect::<Vec<_>>()
        .join("\n");
    if fields_str.is_empty() {
        format!("struct {name}{tp}\nend")
    } else {
        format!("struct {name}{tp}\n{fields_str}\nend")
    }
}

/// Render an enum's hover signature: `enum Name<Tp,...>` followed by
/// each variant on its own indented line.
pub(crate) fn format_enum_def(
    name: &str,
    type_params: &[String],
    variants: &[ResolvedEnumVariant],
    registry: &GlobalRegistry,
) -> String {
    let tp = if type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", type_params.join(", "))
    };
    let variants_str = variants
        .iter()
        .map(|v| match &v.data {
            ResolvedVariantData::Unit => format!("  {}", v.name),
            ResolvedVariantData::Tuple(types) => {
                let ts = types
                    .iter()
                    .map(|t| format_resolved_type(t, registry))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("  {}({ts})", v.name)
            }
            ResolvedVariantData::Struct(fields) => {
                let fs = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, format_resolved_type(&f.ty, registry)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("  {}{{{fs}}}", v.name)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if variants_str.is_empty() {
        format!("enum {name}{tp}\nend")
    } else {
        format!("enum {name}{tp}\n{variants_str}\nend")
    }
}

/// Render a protocol's hover signature: `protocol Name<Tp,...>`
/// followed by each method on its own indented line.
pub(crate) fn format_protocol_def(
    name: &str,
    type_params: &[String],
    methods: &[ResolvedProtocolMethod],
    registry: &GlobalRegistry,
) -> String {
    let tp = if type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", type_params.join(", "))
    };
    let methods_str = methods
        .iter()
        .map(|m| {
            let ps = m
                .non_self_params
                .iter()
                .map(|p| format_param(p, registry))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "  fn {}({ps}) -> {}",
                m.name,
                format_resolved_type(&m.return_type, registry)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if methods_str.is_empty() {
        format!("protocol {name}{tp}\nend")
    } else {
        format!("protocol {name}{tp}\n{methods_str}\nend")
    }
}
