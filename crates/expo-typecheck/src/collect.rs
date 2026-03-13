use std::collections::HashMap;

use expo_ast::ast::{EnumVariantData, ImplMember, Item, Module, Param, TypeExpr};

use crate::context::{
    EnumInfo, FunctionSig, ParamInfo, StructInfo, TypeContext, VariantData, VariantInfo,
};
use crate::types::{Type, resolve_type_expr};

pub fn collect(module: &Module) -> TypeContext {
    let mut ctx = TypeContext::new();

    let struct_names: Vec<&str> = module
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(s) = item {
                Some(s.name.as_str())
            } else {
                None
            }
        })
        .collect();

    let enum_names: Vec<&str> = module
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Enum(e) = item {
                Some(e.name.as_str())
            } else {
                None
            }
        })
        .collect();

    for item in &module.items {
        match item {
            Item::Enum(e) => {
                if !e.type_params.is_empty() {
                    continue;
                }
                let variants: Vec<VariantInfo> = e
                    .variants
                    .iter()
                    .map(|v| {
                        let data = match &v.data {
                            EnumVariantData::Struct(fields) => {
                                let resolved: Vec<(String, Type)> = fields
                                    .iter()
                                    .map(|f| {
                                        let ty = resolve_type_expr(
                                            &f.type_expr,
                                            &struct_names,
                                            &enum_names,
                                        );
                                        (f.name.clone(), ty)
                                    })
                                    .collect();
                                VariantData::Struct(resolved)
                            }
                            EnumVariantData::Tuple(types) => {
                                let resolved: Vec<Type> = types
                                    .iter()
                                    .map(|t| resolve_type_expr(t, &struct_names, &enum_names))
                                    .collect();
                                VariantData::Tuple(resolved)
                            }
                            EnumVariantData::Unit => VariantData::Unit,
                        };
                        VariantInfo {
                            data,
                            name: v.name.clone(),
                        }
                    })
                    .collect();
                ctx.enums.insert(
                    e.name.clone(),
                    EnumInfo {
                        methods: HashMap::new(),
                        span: e.span,
                        variants,
                    },
                );
            }
            Item::Function(f) => {
                if let Some(sig) = build_function_sig(f, &struct_names, &enum_names) {
                    ctx.functions.insert(f.name.clone(), sig);
                }
            }
            Item::Impl(impl_block) => {
                if impl_block.trait_expr.is_some() {
                    continue;
                }
                let target_name = match &impl_block.target {
                    TypeExpr::Named { path, .. } if path.len() == 1 => path[0].clone(),
                    _ => continue,
                };
                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member
                        && let Some(sig) = build_function_sig(f, &struct_names, &enum_names)
                    {
                        let methods = if let Some(si) = ctx.structs.get_mut(&target_name) {
                            Some(&mut si.methods)
                        } else if let Some(ei) = ctx.enums.get_mut(&target_name) {
                            Some(&mut ei.methods)
                        } else {
                            None
                        };
                        if let Some(methods) = methods {
                            if methods.contains_key(&f.name) {
                                ctx.diagnostics.push(expo_ast::ast::Diagnostic {
                                    severity: expo_ast::ast::Severity::Error,
                                    message: format!(
                                        "duplicate method `{}` in impl for `{}`",
                                        f.name, target_name
                                    ),
                                    hint: None,
                                    span: f.span,
                                });
                            } else {
                                methods.insert(f.name.clone(), sig);
                            }
                        }
                    }
                }
            }
            Item::Struct(s) => {
                if !s.type_params.is_empty() {
                    continue;
                }
                let fields: Vec<(String, Type)> = s
                    .fields
                    .iter()
                    .map(|f| {
                        let ty = resolve_type_expr(&f.type_expr, &struct_names, &enum_names);
                        (f.name.clone(), ty)
                    })
                    .collect();
                ctx.structs.insert(
                    s.name.clone(),
                    StructInfo {
                        fields,
                        methods: HashMap::new(),
                        span: s.span,
                    },
                );
            }
            _ => {}
        }
    }

    ctx
}

fn build_function_sig(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
) -> Option<FunctionSig> {
    if !f.type_params.is_empty() {
        return None;
    }

    let params: Vec<ParamInfo> = f
        .params
        .iter()
        .filter_map(|p| match p {
            Param::Regular {
                name, type_expr, ..
            } => Some(ParamInfo {
                name: name.clone(),
                ty: resolve_type_expr(type_expr, known_structs, known_enums),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();

    let return_type = f
        .return_type
        .as_ref()
        .map(|t| resolve_type_expr(t, known_structs, known_enums))
        .unwrap_or(Type::Unit);

    Some(FunctionSig {
        params,
        return_type,
        span: f.span,
    })
}
