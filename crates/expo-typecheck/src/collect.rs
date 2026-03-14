use std::collections::{HashMap, HashSet};

use expo_ast::ast::{EnumVariantData, ImplMember, ImportTarget, Item, Module, Param, TypeExpr};
use expo_ast::span::Span;

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

    ctx.functions.insert(
        "print".to_string(),
        FunctionSig {
            is_private: false,
            params: vec![ParamInfo {
                name: "value".to_string(),
                ty: Type::Unknown,
            }],
            return_type: Type::Unit,
            span: Span::zero(),
        },
    );

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
        is_private: f.is_private,
        params,
        return_type,
        span: f.span,
    })
}

pub fn resolve_imports(
    module: &Module,
    ctx: &mut TypeContext,
    module_contexts: &HashMap<String, TypeContext>,
) {
    let mut imported_names: HashSet<String> = HashSet::new();

    for item in &module.items {
        if let Item::Import(import) = item {
            let base_path = import.path.join(".");

            match &import.target {
                ImportTarget::Module => {
                    if let Some(source_ctx) = module_contexts.get(&base_path) {
                        merge_all_public(
                            ctx,
                            source_ctx,
                            &base_path,
                            import.span,
                            &mut imported_names,
                        );
                        let module_name = import.path.last().unwrap().clone();
                        insert_module_or_error(ctx, &module_name, source_ctx, import.span);
                    } else {
                        ctx.error(
                            format!("unresolved import: module `{base_path}` not found"),
                            import.span,
                        );
                    }
                }
                ImportTarget::Wildcard => {
                    if let Some(source_ctx) = module_contexts.get(&base_path) {
                        merge_all_public(
                            ctx,
                            source_ctx,
                            &base_path,
                            import.span,
                            &mut imported_names,
                        );
                        let module_name = import.path.last().unwrap().clone();
                        insert_module_or_error(ctx, &module_name, source_ctx, import.span);
                    } else {
                        ctx.error(
                            format!("unresolved import: module `{base_path}` not found"),
                            import.span,
                        );
                    }
                }
                ImportTarget::Item(name) => {
                    let full_path = format!("{base_path}.{name}");
                    if let Some(source_ctx) = module_contexts.get(&full_path) {
                        merge_all_public(
                            ctx,
                            source_ctx,
                            &full_path,
                            import.span,
                            &mut imported_names,
                        );
                        insert_module_or_error(ctx, name, source_ctx, import.span);
                    } else if let Some(source_ctx) = module_contexts.get(&base_path) {
                        merge_named(
                            ctx,
                            source_ctx,
                            name,
                            &base_path,
                            import.span,
                            &mut imported_names,
                        );
                    } else {
                        ctx.error(
                            format!("unresolved import: module `{base_path}` not found"),
                            import.span,
                        );
                    }
                }
                ImportTarget::Group(names) => {
                    if let Some(source_ctx) = module_contexts.get(&base_path) {
                        for name in names {
                            merge_named(
                                ctx,
                                source_ctx,
                                name,
                                &base_path,
                                import.span,
                                &mut imported_names,
                            );
                        }
                    } else {
                        ctx.error(
                            format!("unresolved import: module `{base_path}` not found"),
                            import.span,
                        );
                    }
                }
            }
        }
    }
}

fn insert_module_or_error(
    ctx: &mut TypeContext,
    module_name: &str,
    source_ctx: &TypeContext,
    span: Span,
) {
    if ctx.imported_modules.contains_key(module_name) {
        ctx.error(
            format!("module qualifier `{module_name}` is already in use from another import"),
            span,
        );
    } else {
        ctx.imported_modules
            .insert(module_name.to_string(), clone_public_context(source_ctx));
    }
}

fn merge_all_public(
    ctx: &mut TypeContext,
    source: &TypeContext,
    _module_path: &str,
    span: Span,
    imported_names: &mut HashSet<String>,
) {
    for (name, sig) in &source.functions {
        if sig.is_private {
            continue;
        }
        if imported_names.contains(name) {
            ctx.error(
                format!("`{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.functions.contains_key(name) {
            imported_names.insert(name.clone());
            ctx.functions.insert(
                name.clone(),
                FunctionSig {
                    is_private: false,
                    params: sig
                        .params
                        .iter()
                        .map(|p| ParamInfo {
                            name: p.name.clone(),
                            ty: p.ty.clone(),
                        })
                        .collect(),
                    return_type: sig.return_type.clone(),
                    span: sig.span,
                },
            );
        }
        // name in ctx.functions but not in imported_names -> local definition, skip silently
    }
    for (name, info) in &source.structs {
        if imported_names.contains(name) {
            ctx.error(
                format!("struct `{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.structs.contains_key(name) {
            imported_names.insert(name.clone());
            ctx.structs.insert(name.clone(), clone_struct_info(info));
        }
    }
    for (name, info) in &source.enums {
        if imported_names.contains(name) {
            ctx.error(
                format!("enum `{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.enums.contains_key(name) {
            imported_names.insert(name.clone());
            ctx.enums.insert(name.clone(), clone_enum_info(info));
        }
    }
}

fn merge_named(
    ctx: &mut TypeContext,
    source: &TypeContext,
    name: &str,
    module_path: &str,
    span: Span,
    imported_names: &mut HashSet<String>,
) {
    if let Some(sig) = source.functions.get(name) {
        if sig.is_private {
            ctx.error(
                format!("function `{name}` is private to module `{module_path}`"),
                span,
            );
        } else if imported_names.contains(name) {
            ctx.error(
                format!("`{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.functions.contains_key(name) {
            imported_names.insert(name.to_string());
            ctx.functions.insert(
                name.to_string(),
                FunctionSig {
                    is_private: false,
                    params: sig
                        .params
                        .iter()
                        .map(|p| ParamInfo {
                            name: p.name.clone(),
                            ty: p.ty.clone(),
                        })
                        .collect(),
                    return_type: sig.return_type.clone(),
                    span: sig.span,
                },
            );
        }
        return;
    }
    if let Some(info) = source.structs.get(name) {
        if imported_names.contains(name) {
            ctx.error(
                format!("struct `{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.structs.contains_key(name) {
            imported_names.insert(name.to_string());
            ctx.structs
                .insert(name.to_string(), clone_struct_info(info));
        }
        return;
    }
    if let Some(info) = source.enums.get(name) {
        if imported_names.contains(name) {
            ctx.error(
                format!("enum `{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.enums.contains_key(name) {
            imported_names.insert(name.to_string());
            ctx.enums.insert(name.to_string(), clone_enum_info(info));
        }
        return;
    }
    ctx.error(
        format!("`{name}` not found in module `{module_path}`"),
        span,
    );
}

fn clone_public_context(source: &TypeContext) -> TypeContext {
    let mut ctx = TypeContext::new();
    for (name, sig) in &source.functions {
        if !sig.is_private {
            ctx.functions.insert(
                name.clone(),
                FunctionSig {
                    is_private: false,
                    params: sig
                        .params
                        .iter()
                        .map(|p| ParamInfo {
                            name: p.name.clone(),
                            ty: p.ty.clone(),
                        })
                        .collect(),
                    return_type: sig.return_type.clone(),
                    span: sig.span,
                },
            );
        }
    }
    for (name, info) in &source.structs {
        ctx.structs.insert(name.clone(), clone_struct_info(info));
    }
    for (name, info) in &source.enums {
        ctx.enums.insert(name.clone(), clone_enum_info(info));
    }
    ctx
}

fn clone_struct_info(info: &StructInfo) -> StructInfo {
    StructInfo {
        fields: info.fields.clone(),
        methods: info
            .methods
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    FunctionSig {
                        is_private: v.is_private,
                        params: v
                            .params
                            .iter()
                            .map(|p| ParamInfo {
                                name: p.name.clone(),
                                ty: p.ty.clone(),
                            })
                            .collect(),
                        return_type: v.return_type.clone(),
                        span: v.span,
                    },
                )
            })
            .collect(),
        span: info.span,
    }
}

fn clone_enum_info(info: &EnumInfo) -> EnumInfo {
    EnumInfo {
        methods: info
            .methods
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    FunctionSig {
                        is_private: v.is_private,
                        params: v
                            .params
                            .iter()
                            .map(|p| ParamInfo {
                                name: p.name.clone(),
                                ty: p.ty.clone(),
                            })
                            .collect(),
                        return_type: v.return_type.clone(),
                        span: v.span,
                    },
                )
            })
            .collect(),
        span: info.span,
        variants: info
            .variants
            .iter()
            .map(|v| VariantInfo {
                name: v.name.clone(),
                data: v.data.clone(),
            })
            .collect(),
    }
}
