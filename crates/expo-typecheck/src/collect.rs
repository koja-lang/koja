use std::collections::{HashMap, HashSet};

use expo_ast::ast::{
    EnumVariantData, Expr, ImplMember, ImportTarget, Item, Literal, Module, Param, ProtocolMethod,
    TypeExpr,
};
use expo_ast::span::Span;

use crate::context::{
    EnumInfo, FunctionSig, ParamInfo, PassMode, ProtocolInfo, StructInfo, TypeContext, VariantData,
    VariantInfo, Visibility,
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

                let protocol_name = impl_block.trait_expr.as_ref().and_then(|te| match te {
                    TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].clone()),
                    TypeExpr::Generic { path, .. } if path.len() == 1 => Some(path[0].clone()),
                    _ => None,
                });

                if let Some(ref proto) = protocol_name
                    && !ctx.protocols.contains_key(proto)
                {
                    ctx.error(format!("unknown protocol `{proto}`"), impl_block.span);
                }

                let tp_refs: Vec<&str> = impl_type_params.iter().map(|s| s.as_str()).collect();
                let mut impl_method_names: HashSet<String> = HashSet::new();

                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member {
                        let sig =
                            build_function_sig_with_params(f, &struct_names, &enum_names, &tp_refs);
                        let Some(sig) = sig else { continue };

                        if let Some(ref proto) = protocol_name
                            && f.visibility == Visibility::Public
                        {
                            impl_method_names.insert(f.name.clone());
                            if let Some(pi) = ctx.protocols.get(proto)
                                && !pi.methods.contains_key(&f.name)
                            {
                                ctx.error(
                                    format!(
                                        "method `{}` is not defined in protocol `{proto}`",
                                        f.name
                                    ),
                                    f.span,
                                );
                            }
                        }

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

                if let Some(ref proto) = protocol_name {
                    let missing: Vec<String> = ctx
                        .protocols
                        .get(proto)
                        .map(|pi| {
                            pi.methods
                                .keys()
                                .filter(|name| !impl_method_names.contains(*name))
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default();
                    for name in &missing {
                        ctx.error(
                            format!("missing method `{name}` required by protocol `{proto}`"),
                            impl_block.span,
                        );
                    }
                    ctx.protocol_impls
                        .entry(target_name.clone())
                        .or_default()
                        .push(proto.clone());
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
            Item::Protocol(p) => {
                let tp_refs: Vec<&str> = p.type_params.iter().map(|s| s.as_str()).collect();
                let mut methods: HashMap<String, FunctionSig> = HashMap::new();
                for m in &p.methods {
                    if let Some(sig) =
                        build_protocol_method_sig(m, &struct_names, &enum_names, &tp_refs)
                    {
                        methods.insert(m.name.clone(), sig);
                    }
                }
                if !p.type_params.is_empty() {
                    ctx.generic_protocol_asts.insert(p.name.clone(), p.clone());
                }
                ctx.protocols.insert(
                    p.name.clone(),
                    ProtocolInfo {
                        methods,
                        span: p.span,
                        type_params: p.type_params.clone(),
                    },
                );
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
            visibility: Visibility::Public,
            params: vec![ParamInfo {
                mode: PassMode::Borrow,
                name: "value".to_string(),
                ty: Type::Unknown,
            }],
            return_type: Type::Unit,
            self_mode: PassMode::Borrow,
            span: Span::zero(),
            type_params: Vec::new(),
        },
    );

    ctx.functions.insert(
        "panic".to_string(),
        FunctionSig {
            visibility: Visibility::Public,
            params: vec![ParamInfo {
                mode: PassMode::Borrow,
                name: "message".to_string(),
                ty: Type::Primitive(crate::types::Primitive::String),
            }],
            return_type: Type::Unit,
            self_mode: PassMode::Borrow,
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
                mode,
                name,
                type_expr,
                ..
            } => Some(ParamInfo {
                mode: *mode,
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

    let self_mode = f
        .params
        .iter()
        .find_map(|p| match p {
            Param::Self_ { mode, .. } => Some(*mode),
            _ => None,
        })
        .unwrap_or(PassMode::Borrow);

    Some(FunctionSig {
        visibility: f.visibility,
        params,
        return_type,
        self_mode,
        span: f.span,
        type_params: f.type_params.clone(),
    })
}

fn build_protocol_method_sig(
    m: &ProtocolMethod,
    known_structs: &[&str],
    known_enums: &[&str],
    extra_type_params: &[&str],
) -> Option<FunctionSig> {
    let mut all_tp: Vec<&str> = m.type_params.iter().map(|s| s.as_str()).collect();
    all_tp.extend_from_slice(extra_type_params);

    let params: Vec<ParamInfo> = m
        .params
        .iter()
        .filter_map(|p| match p {
            Param::Regular {
                mode,
                name,
                type_expr,
                ..
            } => Some(ParamInfo {
                mode: *mode,
                name: name.clone(),
                ty: resolve_type_expr_with_params(type_expr, known_structs, known_enums, &all_tp),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();

    let return_type = m
        .return_type
        .as_ref()
        .map(|t| resolve_type_expr_with_params(t, known_structs, known_enums, &all_tp))
        .unwrap_or(Type::Unit);

    let self_mode = m
        .params
        .iter()
        .find_map(|p| match p {
            Param::Self_ { mode, .. } => Some(*mode),
            _ => None,
        })
        .unwrap_or(PassMode::Borrow);

    Some(FunctionSig {
        visibility: Visibility::Public,
        params,
        return_type,
        self_mode,
        span: m.span,
        type_params: m.type_params.clone(),
    })
}

/// Builds a new context containing only the public symbols from `source`.
fn clone_public_context(source: &TypeContext) -> TypeContext {
    let mut ctx = TypeContext::new();
    for (name, sig) in &source.functions {
        if sig.visibility == Visibility::Public {
            let mut cloned = sig.clone();
            cloned.visibility = Visibility::Public;
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
        if sig.visibility == Visibility::Private {
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
            cloned.visibility = Visibility::Public;
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
        if sig.visibility == Visibility::Private {
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
            cloned.visibility = Visibility::Public;
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
