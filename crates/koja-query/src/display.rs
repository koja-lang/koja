//! Human-readable rendering of registry types for hover and
//! completion. A type goes through `display_type` in
//! `koja-typecheck` with the short name style.

use koja_ast::ast::{TypeExpr, path_text};
use koja_typecheck::{
    DisplayStyle, FunctionSignature, GlobalRegistry, ResolvedEnumVariant, ResolvedParam,
    ResolvedProtocolMethod, ResolvedStructField, ResolvedVariantData, display_type,
};

/// Render a [`TypeExpr`] as the user wrote it, for outline details
/// and other places that have the syntax but no registry.
pub fn type_expr_label(type_expr: &TypeExpr) -> String {
    match type_expr {
        TypeExpr::Named { path, .. } => path_text(path),
        TypeExpr::Generic { path, args, .. } => {
            let args: Vec<String> = args.iter().map(type_expr_label).collect();
            format!("{}<{}>", path_text(path), args.join(", "))
        }
        TypeExpr::Unit { .. } => "()".to_string(),
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let params: Vec<String> = params.iter().map(type_expr_label).collect();
            format!(
                "fn ({}) -> {}",
                params.join(", "),
                type_expr_label(return_type)
            )
        }
        TypeExpr::Self_ { .. } => "Self".to_string(),
        TypeExpr::Tuple { elements, .. } => {
            let elements: Vec<String> = elements.iter().map(type_expr_label).collect();
            format!("({})", elements.join(", "))
        }
        TypeExpr::Union { types, .. } => {
            let types: Vec<String> = types.iter().map(type_expr_label).collect();
            types.join(" | ")
        }
    }
}

/// Render a [`FunctionSignature`] under `display_name`, including
/// type-parameter names from the owning [`koja_typecheck::RegistryEntry`].
pub fn format_function_signature(
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
        display_type(&sig.return_type, registry, DisplayStyle::Short)
    )
}

fn format_param(p: &ResolvedParam, registry: &GlobalRegistry) -> String {
    let ty = display_type(&p.ty, registry, DisplayStyle::Short);
    format!("{}: {}", p.name, ty)
}

/// Render a struct's hover signature: `struct Name<Tp,...>` followed
/// by each field on its own indented line.
pub fn format_struct_def(
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
        .map(|f| {
            format!(
                "  {}: {}",
                f.name,
                display_type(&f.ty, registry, DisplayStyle::Short)
            )
        })
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
pub fn format_enum_def(
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
                    .map(|t| display_type(t, registry, DisplayStyle::Short))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("  {}({ts})", v.name)
            }
            ResolvedVariantData::Struct(fields) => {
                let fs = fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{}: {}",
                            f.name,
                            display_type(&f.ty, registry, DisplayStyle::Short)
                        )
                    })
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
pub fn format_protocol_def(
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
                display_type(&m.return_type, registry, DisplayStyle::Short)
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
