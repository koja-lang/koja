use std::collections::{HashMap, HashSet};

use expo_ast::ast::{
    EnumVariantData, Expr, ImplMember, ImportTarget, Item, Literal, Module, Param, TypeExpr,
};
use expo_ast::span::Span;

use crate::context::{
    EnumInfo, FunctionSig, ParamInfo, StructInfo, TypeContext, VariantData, VariantInfo,
};
use crate::types::{Type, resolve_type_expr_with_params};

/// Walks all top-level items in a module and builds a [`TypeContext`] containing
/// function signatures, struct definitions, and enum definitions.
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
                let tp_refs: Vec<&str> = e.type_params.iter().map(|s| s.as_str()).collect();
                let variants: Vec<VariantInfo> = e
                    .variants
                    .iter()
                    .map(|v| {
                        let data = match &v.data {
                            EnumVariantData::Struct(fields) => {
                                let resolved: Vec<(String, Type)> = fields
                                    .iter()
                                    .map(|f| {
                                        let ty = resolve_type_expr_with_params(
                                            &f.type_expr,
                                            &struct_names,
                                            &enum_names,
                                            &tp_refs,
                                        );
                                        (f.name.clone(), ty)
                                    })
                                    .collect();
                                VariantData::Struct(resolved)
                            }
                            EnumVariantData::Tuple(types) => {
                                let resolved: Vec<Type> = types
                                    .iter()
                                    .map(|t| {
                                        resolve_type_expr_with_params(
                                            t,
                                            &struct_names,
                                            &enum_names,
                                            &tp_refs,
                                        )
                                    })
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
                if !e.type_params.is_empty() {
                    ctx.generic_enum_asts.insert(e.name.clone(), e.clone());
                }
                ctx.enums.insert(
                    e.name.clone(),
                    EnumInfo {
                        methods: HashMap::new(),
                        span: e.span,
                        type_params: e.type_params.clone(),
                        variants,
                    },
                );
            }
            Item::Function(f) => {
                if let Some(sig) = build_function_sig(f, &struct_names, &enum_names) {
                    if !f.type_params.is_empty() {
                        ctx.generic_function_asts.insert(f.name.clone(), f.clone());
                    }
                    ctx.functions.insert(f.name.clone(), sig);
                }
            }
            Item::Impl(impl_block) => {
                if impl_block.trait_expr.is_some() {
                    continue;
                }
                let (target_name, impl_type_params) = match &impl_block.target {
                    TypeExpr::Named { path, .. } if path.len() == 1 => {
                        (path[0].clone(), Vec::new())
                    }
                    TypeExpr::Generic { path, args, .. } if path.len() == 1 => {
                        let tp_names: Vec<String> = args
                            .iter()
                            .filter_map(|a| {
                                if let TypeExpr::Named { path, .. } = a
                                    && path.len() == 1
                                {
                                    return Some(path[0].clone());
                                }
                                None
                            })
                            .collect();
                        (path[0].clone(), tp_names)
                    }
                    _ => continue,
                };
                let is_generic_impl = !impl_type_params.is_empty();
                if is_generic_impl {
                    ctx.generic_impl_asts
                        .entry(target_name.clone())
                        .or_default()
                        .push(impl_block.clone());
                }
                let tp_refs: Vec<&str> = impl_type_params.iter().map(|s| s.as_str()).collect();
                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member {
                        let sig =
                            build_function_sig_with_params(f, &struct_names, &enum_names, &tp_refs);
                        let Some(sig) = sig else { continue };
                        let methods = if let Some(si) = ctx.structs.get_mut(&target_name) {
                            Some(&mut si.methods)
                        } else if let Some(ei) = ctx.enums.get_mut(&target_name) {
                            Some(&mut ei.methods)
                        } else {
                            None
                        };
                        if let Some(methods) = methods {
                            if methods.contains_key(&f.name) {
                                ctx.error(
                                    format!(
                                        "duplicate method `{}` in impl for `{}`",
                                        f.name, target_name
                                    ),
                                    f.span,
                                );
                            } else {
                                methods.insert(f.name.clone(), sig);
                            }
                        }
                    }
                }
            }
            Item::Constant(c) => {
                let ty = match &c.value {
                    Expr::Literal {
                        value: Literal::Bool(_),
                        ..
                    } => Type::Primitive(crate::types::Primitive::Bool),
                    Expr::Literal {
                        value: Literal::Int(_),
                        ..
                    } => Type::Primitive(crate::types::Primitive::I32),
                    Expr::Literal {
                        value: Literal::Float(_),
                        ..
                    } => Type::Primitive(crate::types::Primitive::F64),
                    Expr::String { .. } => Type::Primitive(crate::types::Primitive::String),
                    _ => {
                        ctx.error(
                            format!(
                                "constant `{}` must be a literal value (int, float, string, or bool)",
                                c.name
                            ),
                            c.span,
                        );
                        Type::Error
                    }
                };
                if ctx.constants.contains_key(&c.name) {
                    ctx.error(format!("duplicate constant `{}`", c.name), c.span);
                } else if ctx.functions.contains_key(&c.name)
                    || ctx.structs.contains_key(&c.name)
                    || ctx.enums.contains_key(&c.name)
                {
                    ctx.error(
                        format!(
                            "constant `{}` conflicts with an existing declaration",
                            c.name
                        ),
                        c.span,
                    );
                } else {
                    ctx.constants.insert(c.name.clone(), ty);
                }
            }
            Item::Struct(s) => {
                let tp_refs: Vec<&str> = s.type_params.iter().map(|s| s.as_str()).collect();
                let fields: Vec<(String, Type)> = s
                    .fields
                    .iter()
                    .map(|f| {
                        let ty = resolve_type_expr_with_params(
                            &f.type_expr,
                            &struct_names,
                            &enum_names,
                            &tp_refs,
                        );
                        (f.name.clone(), ty)
                    })
                    .collect();
                if !s.type_params.is_empty() {
                    ctx.generic_struct_asts.insert(s.name.clone(), s.clone());
                }
                ctx.structs.insert(
                    s.name.clone(),
                    StructInfo {
                        fields,
                        methods: HashMap::new(),
                        span: s.span,
                        type_params: s.type_params.clone(),
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
            type_params: Vec::new(),
        },
    );

    ctx
}

/// Processes import statements and merges symbols from other module contexts
/// into the current one, detecting name conflicts and missing modules.
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

fn build_function_sig(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
) -> Option<FunctionSig> {
    build_function_sig_with_params(f, known_structs, known_enums, &[])
}

fn build_function_sig_with_params(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
    extra_type_params: &[&str],
) -> Option<FunctionSig> {
    let mut all_tp: Vec<&str> = f.type_params.iter().map(|s| s.as_str()).collect();
    all_tp.extend_from_slice(extra_type_params);

    let params: Vec<ParamInfo> = f
        .params
        .iter()
        .filter_map(|p| match p {
            Param::Regular {
                name, type_expr, ..
            } => Some(ParamInfo {
                name: name.clone(),
                ty: resolve_type_expr_with_params(type_expr, known_structs, known_enums, &all_tp),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();

    let return_type = f
        .return_type
        .as_ref()
        .map(|t| resolve_type_expr_with_params(t, known_structs, known_enums, &all_tp))
        .unwrap_or(Type::Unit);

    Some(FunctionSig {
        is_private: f.is_private,
        params,
        return_type,
        span: f.span,
        type_params: f.type_params.clone(),
    })
}

/// Builds a new context containing only the public symbols from `source`.
fn clone_public_context(source: &TypeContext) -> TypeContext {
    let mut ctx = TypeContext::new();
    for (name, sig) in &source.functions {
        if !sig.is_private {
            let mut cloned = sig.clone();
            cloned.is_private = false;
            ctx.functions.insert(name.clone(), cloned);
        }
    }
    for (name, info) in &source.structs {
        ctx.structs.insert(name.clone(), info.clone());
    }
    for (name, info) in &source.enums {
        ctx.enums.insert(name.clone(), info.clone());
    }
    ctx
}

fn insert_module_or_error(
    ctx: &mut TypeContext,
    module_name: &str,
    source_ctx: &TypeContext,
    span: Span,
) {
    if ctx.imported_modules.contains_key(module_name) {
        return ctx.error(
            format!("module qualifier `{module_name}` is already in use from another import"),
            span,
        );
    }
    ctx.imported_modules
        .insert(module_name.to_string(), clone_public_context(source_ctx));
}

/// Copies all public functions, structs, and enums from `source` into `ctx`,
/// detecting duplicate imports across modules.
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
            let mut cloned = sig.clone();
            cloned.is_private = false;
            ctx.functions.insert(name.clone(), cloned);
        }
    }
    for (name, info) in &source.structs {
        if imported_names.contains(name) {
            ctx.error(
                format!("struct `{name}` is already imported from another module"),
                span,
            );
        } else if !ctx.structs.contains_key(name) {
            imported_names.insert(name.clone());
            ctx.structs.insert(name.clone(), info.clone());
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
            ctx.enums.insert(name.clone(), info.clone());
        }
    }
}

/// Imports a single named symbol from `source` into `ctx`, checking for
/// privacy violations and duplicate imports.
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
            let mut cloned = sig.clone();
            cloned.is_private = false;
            ctx.functions.insert(name.to_string(), cloned);
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
            ctx.structs.insert(name.to_string(), info.clone());
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
            ctx.enums.insert(name.to_string(), info.clone());
        }
        return;
    }
    ctx.error(
        format!("`{name}` not found in module `{module_path}`"),
        span,
    );
}
