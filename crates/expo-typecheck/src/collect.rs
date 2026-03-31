use std::collections::{BTreeMap, HashMap, HashSet};

use expo_ast::ast::{
    EnumConstructionData, EnumVariantData, Expr, Function, ImplMember, ImportTarget, Item, Literal,
    Module, Param, Pattern, ProtocolMethod, Statement, StringPart, TypeExpr,
};
use expo_ast::span::Span;

use crate::context::{
    FunctionKind, FunctionSig, ParamInfo, PassMode, ProtocolInfo, TypeContext, TypeInfo, TypeKind,
    VariantData, VariantInfo, Visibility,
};
use crate::types::{Primitive, Type, resolve_type_expr_with_params};

/// Pre-collected struct and enum names across all modules in the program.
/// Passed into [`collect`] so that type resolution sees every type name
/// from the start, eliminating the need for re-resolution patches.
pub struct GlobalNames {
    pub struct_names: HashSet<String>,
    pub enum_names: HashSet<String>,
}

/// Scans all modules for struct and enum names without resolving any types.
/// This is the first phase of a two-phase collection: names are gathered
/// globally, then passed into [`collect`] so cross-module type references
/// resolve correctly on the first pass.
pub fn collect_all_names(modules: &[&Module]) -> GlobalNames {
    let mut names = GlobalNames {
        struct_names: HashSet::new(),
        enum_names: HashSet::new(),
    };
    for module in modules {
        for item in &module.items {
            match item {
                Item::Struct(s) => {
                    names.struct_names.insert(s.name.clone());
                }
                Item::Enum(e) => {
                    names.enum_names.insert(e.name.clone());
                }
                _ => {}
            }
        }
    }
    names
}

/// Walks all top-level items in a module and builds a [`TypeContext`] containing
/// function signatures, struct definitions, and enum definitions.
/// Requires [`GlobalNames`] from [`collect_all_names`] so that cross-module
/// type references (e.g. imported struct names) resolve correctly.
pub fn collect(module: &Module, global_names: &GlobalNames) -> TypeContext {
    let mut ctx = TypeContext::new();

    let struct_names: Vec<&str> = global_names
        .struct_names
        .iter()
        .map(|s| s.as_str())
        .collect();
    let enum_names: Vec<&str> = global_names.enum_names.iter().map(|s| s.as_str()).collect();

    // Pre-pass: collect type aliases so they're available for resolving
    // function signatures and struct/enum fields in the main pass.
    for item in &module.items {
        if let Item::TypeAlias(ta) = item {
            let resolved = resolve_type_expr_with_params(
                &ta.type_expr,
                &struct_names,
                &enum_names,
                &[],
                &std::collections::BTreeMap::new(),
            );
            if let Some(existing) = ctx.type_aliases.get(&ta.name) {
                if *existing != resolved {
                    ctx.error(
                        format!(
                            "type alias `{}` is already defined with a different type",
                            ta.name
                        ),
                        ta.span,
                    );
                }
            } else {
                ctx.type_aliases.insert(ta.name.clone(), resolved);
            }
        }
    }

    let type_aliases = ctx.type_aliases.clone();

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
                                            &type_aliases,
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
                                            &type_aliases,
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
                ctx.generic_enum_asts.insert(e.name.clone(), e.clone());
                ctx.types.insert(
                    e.name.clone(),
                    TypeInfo {
                        functions: BTreeMap::new(),
                        kind: TypeKind::Enum { variants },
                        span: e.span,
                        type_params: e.type_params.clone(),
                    },
                );
            }
            Item::Function(f) => {
                if let Some(sig) = build_function_sig(f, &struct_names, &enum_names, &type_aliases)
                {
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

                let tp_refs: Vec<&str> = impl_type_params.iter().map(|s| s.as_str()).collect();
                let mut impl_method_names: HashSet<String> = HashSet::new();

                let self_type = resolve_type_expr_with_params(
                    &impl_block.target,
                    &struct_names,
                    &enum_names,
                    &tp_refs,
                    &type_aliases,
                );

                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member {
                        let sig = build_function_sig_with_params(
                            f,
                            &struct_names,
                            &enum_names,
                            &tp_refs,
                            &type_aliases,
                        );
                        let Some(sig) = sig else { continue };
                        let sig = substitute_self_type(sig, &self_type);

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

                        let ti = ctx.types.get_mut(&target_name);
                        if let Some(ti) = ti {
                            if ti.functions.contains_key(&f.name) {
                                ctx.error(
                                    format!(
                                        "duplicate function `{}` in impl for `{}`",
                                        f.name, target_name
                                    ),
                                    f.span,
                                );
                            } else {
                                ti.functions.insert(f.name.clone(), sig);
                            }
                        } else if Primitive::from_name(&target_name).is_some() {
                            let ti =
                                ctx.types
                                    .entry(target_name.clone())
                                    .or_insert_with(|| TypeInfo {
                                        functions: BTreeMap::new(),
                                        kind: TypeKind::Primitive,
                                        span: f.span,
                                        type_params: Vec::new(),
                                    });
                            if ti.functions.contains_key(&f.name) {
                                ctx.error(
                                    format!(
                                        "duplicate function `{}` in impl for `{}`",
                                        f.name, target_name
                                    ),
                                    f.span,
                                );
                            } else {
                                ti.functions.insert(f.name.clone(), sig);
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
                    let proto_type_param_names: Vec<String> = ctx
                        .protocols
                        .get(proto)
                        .map(|pi| pi.type_params.clone())
                        .unwrap_or_default();
                    let proto_type_args: Vec<String> = if let Some(TypeExpr::Generic {
                        args, ..
                    }) = &impl_block.trait_expr
                    {
                        args.iter()
                            .map(|a| match a {
                                TypeExpr::Named { path, .. } if path.len() == 1 => path[0].clone(),
                                _ => String::new(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let type_param_map: Vec<(&str, &str)> = proto_type_param_names
                        .iter()
                        .zip(proto_type_args.iter())
                        .map(|(k, v)| (k.as_str(), v.as_str()))
                        .collect();

                    for name in &missing {
                        let has_default = ctx
                            .protocols
                            .get(proto)
                            .is_some_and(|pi| pi.default_bodies.contains_key(name));
                        if has_default {
                            let pm = ctx.protocols[proto].default_bodies[name].clone();
                            let synth = synthesize_default_fn(&pm, &target_name, &type_param_map);
                            let sig = build_function_sig_with_params(
                                &synth,
                                &struct_names,
                                &enum_names,
                                &tp_refs,
                                &type_aliases,
                            );
                            if let Some(sig) = sig {
                                let sig = substitute_self_type(sig, &self_type);
                                if let Some(ti) = ctx.types.get_mut(&target_name) {
                                    ti.functions.insert(name.clone(), sig);
                                }
                            }
                            ctx.synthesized_default_fns
                                .entry(target_name.clone())
                                .or_default()
                                .push(synth);
                        } else {
                            ctx.error(
                                format!("missing method `{name}` required by protocol `{proto}`"),
                                impl_block.span,
                            );
                        }
                    }
                    let proto_type_args: Vec<Type> =
                        if let Some(TypeExpr::Generic { args, .. }) = &impl_block.trait_expr {
                            args.iter()
                                .map(|a| {
                                    resolve_type_expr_with_params(
                                        a,
                                        &struct_names,
                                        &enum_names,
                                        &tp_refs,
                                        &type_aliases,
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                    ctx.protocol_impls
                        .entry(target_name.clone())
                        .or_default()
                        .push((proto.clone(), proto_type_args));
                }
            }
            Item::Constant(c) => {
                let ty = if let Some(type_ann) = &c.type_annotation {
                    resolve_type_expr_with_params(
                        type_ann,
                        &struct_names,
                        &enum_names,
                        &[],
                        &ctx.type_aliases,
                    )
                } else {
                    match &c.value {
                        Expr::Literal {
                            value: Literal::Bool(_),
                            ..
                        } => Type::Primitive(Primitive::Bool),
                        Expr::Literal {
                            value: Literal::Int(_),
                            ..
                        } => Type::Primitive(Primitive::I64),
                        Expr::Literal {
                            value: Literal::Float(_),
                            ..
                        } => Type::Primitive(Primitive::F64),
                        Expr::String { .. } => Type::Primitive(Primitive::String),
                        Expr::EnumConstruction {
                            type_path,
                            data: EnumConstructionData::Unit,
                            ..
                        } => {
                            let enum_name = type_path.join(".");
                            if enum_names.contains(&enum_name.as_str()) {
                                Type::Enum(enum_name)
                            } else {
                                ctx.error(
                                    format!("constant `{}`: unknown enum `{}`", c.name, enum_name),
                                    c.span,
                                );
                                Type::Error
                            }
                        }
                        Expr::StructConstruction {
                            type_path, fields, ..
                        } if fields.iter().all(|f| is_const_expr(&f.value)) => {
                            let name = type_path.join(".");
                            if struct_names.contains(&name.as_str()) {
                                Type::Struct(name)
                            } else {
                                ctx.error(
                                    format!("constant `{}`: unknown struct `{}`", c.name, name),
                                    c.span,
                                );
                                Type::Error
                            }
                        }
                        _ => {
                            ctx.error(
                                format!(
                                    "constant `{}` must be a literal value (Int, Float, String, or Bool)",
                                    c.name
                                ),
                                c.span,
                            );
                            Type::Error
                        }
                    }
                };
                if ctx.constants.contains_key(&c.name) {
                    ctx.error(format!("duplicate constant `{}`", c.name), c.span);
                } else if ctx.functions.contains_key(&c.name) || ctx.types.contains_key(&c.name) {
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
                let mut methods: BTreeMap<String, FunctionSig> = BTreeMap::new();
                let mut default_bodies: BTreeMap<String, ProtocolMethod> = BTreeMap::new();
                for m in &p.methods {
                    if let Some(sig) = build_protocol_method_sig(
                        m,
                        &struct_names,
                        &enum_names,
                        &tp_refs,
                        &type_aliases,
                    ) {
                        methods.insert(m.name.clone(), sig);
                    }
                    if m.body.is_some() {
                        default_bodies.insert(m.name.clone(), m.clone());
                    }
                }
                if !p.type_params.is_empty() {
                    ctx.generic_protocol_asts.insert(p.name.clone(), p.clone());
                }
                ctx.protocols.insert(
                    p.name.clone(),
                    ProtocolInfo {
                        default_bodies,
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
                            &type_aliases,
                        );
                        (f.name.clone(), ty)
                    })
                    .collect();
                ctx.generic_struct_asts.insert(s.name.clone(), s.clone());
                ctx.types.insert(
                    s.name.clone(),
                    TypeInfo {
                        functions: BTreeMap::new(),
                        kind: TypeKind::Struct { fields },
                        span: s.span,
                        type_params: s.type_params.clone(),
                    },
                );
            }
            Item::TypeAlias(_) => {} // handled in pre-pass above
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
            kind: FunctionKind::Static,
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
            kind: FunctionKind::Static,
            span: Span::zero(),
            type_params: Vec::new(),
        },
    );

    ctx
}

/// Synthesizes default protocol method implementations for impl blocks whose
/// protocol info wasn't available during initial collection (e.g. stdlib
/// protocols like `Process`). Must be called after merging the stdlib context.
pub fn synthesize_protocol_defaults(module: &Module, ctx: &mut TypeContext) {
    let struct_names: Vec<String> = ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_struct())
        .map(|(n, _)| n.clone())
        .collect();
    let enum_names: Vec<String> = ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_enum())
        .map(|(n, _)| n.clone())
        .collect();
    let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();
    let type_aliases = ctx.type_aliases.clone();
    let tp_refs: Vec<&str> = Vec::new();

    for item in &module.items {
        if let Item::Impl(impl_block) = item {
            let target_name = if let TypeExpr::Named { path, .. } = &impl_block.target
                && path.len() == 1
            {
                path[0].clone()
            } else {
                continue;
            };

            let protocol_name = impl_block.trait_expr.as_ref().and_then(|te| match te {
                TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].clone()),
                TypeExpr::Generic { path, .. } if path.len() == 1 => Some(path[0].clone()),
                _ => None,
            });

            let Some(proto) = protocol_name else {
                continue;
            };

            if ctx.synthesized_default_fns.contains_key(&target_name)
                && ctx.synthesized_default_fns[&target_name].iter().any(|f| {
                    ctx.protocols
                        .get(&proto)
                        .is_some_and(|pi| pi.default_bodies.contains_key(&f.name))
                })
            {
                continue;
            }

            let impl_method_names: std::collections::HashSet<String> = impl_block
                .members
                .iter()
                .filter_map(|m| {
                    if let ImplMember::Function(f) = m {
                        Some(f.name.clone())
                    } else {
                        None
                    }
                })
                .collect();

            let missing: Vec<String> = ctx
                .protocols
                .get(&proto)
                .map(|pi| {
                    pi.methods
                        .keys()
                        .filter(|name| !impl_method_names.contains(*name))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if missing.is_empty() {
                continue;
            }

            let self_type = if ctx.is_struct(&target_name) {
                Type::Struct(target_name.clone())
            } else if ctx.is_enum(&target_name) {
                Type::Enum(target_name.clone())
            } else if let Some(p) = Primitive::from_name(&target_name) {
                Type::Primitive(p)
            } else {
                continue;
            };

            let proto_type_param_names: Vec<String> = ctx
                .protocols
                .get(&proto)
                .map(|pi| pi.type_params.clone())
                .unwrap_or_default();
            let proto_type_args: Vec<String> =
                if let Some(TypeExpr::Generic { args, .. }) = &impl_block.trait_expr {
                    args.iter()
                        .map(|a| match a {
                            TypeExpr::Named { path, .. } if path.len() == 1 => path[0].clone(),
                            _ => String::new(),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            let type_param_map: Vec<(&str, &str)> = proto_type_param_names
                .iter()
                .zip(proto_type_args.iter())
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            for name in &missing {
                let has_default = ctx
                    .protocols
                    .get(&proto)
                    .is_some_and(|pi| pi.default_bodies.contains_key(name));
                if has_default {
                    let pm = ctx.protocols[&proto].default_bodies[name].clone();
                    let synth = synthesize_default_fn(&pm, &target_name, &type_param_map);
                    let sig = build_function_sig_with_params(
                        &synth,
                        &struct_refs,
                        &enum_refs,
                        &tp_refs,
                        &type_aliases,
                    );
                    if let Some(sig) = sig {
                        let sig = substitute_self_type(sig, &self_type);
                        if let Some(ti) = ctx.types.get_mut(&target_name) {
                            ti.functions.insert(name.clone(), sig);
                        }
                    }
                    ctx.synthesized_default_fns
                        .entry(target_name.clone())
                        .or_default()
                        .push(synth);
                } else {
                    ctx.error(
                        format!("missing method `{name}` required by protocol `{proto}`"),
                        impl_block.span,
                    );
                }
            }
        }
    }
}

/// Processes import statements and merges symbols from other module contexts
/// into the current one, detecting name conflicts and missing modules.
pub fn resolve_imports(
    module: &Module,
    ctx: &mut TypeContext,
    module_contexts: &BTreeMap<String, TypeContext>,
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

/// Builds a [`FunctionSig`] from a function AST, delegating to
/// [`build_function_sig_with_params`] with no extra type parameters.
fn build_function_sig(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
) -> Option<FunctionSig> {
    build_function_sig_with_params(f, known_structs, known_enums, &[], known_type_aliases)
}

/// Resolves a function AST node into a [`FunctionSig`], handling `self` receivers,
/// parameter types, and merging `extra_type_params` from the enclosing type.
fn build_function_sig_with_params(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
    extra_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
) -> Option<FunctionSig> {
    let mut all_tp: Vec<&str> = f.type_params.iter().map(|s| s.as_str()).collect();
    all_tp.extend_from_slice(extra_type_params);
    if !all_tp.contains(&"Self") {
        all_tp.push("Self");
    }

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
                ty: resolve_type_expr_with_params(
                    type_expr,
                    known_structs,
                    known_enums,
                    &all_tp,
                    known_type_aliases,
                ),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();

    let return_type = f
        .return_type
        .as_ref()
        .map(|t| {
            resolve_type_expr_with_params(
                t,
                known_structs,
                known_enums,
                &all_tp,
                known_type_aliases,
            )
        })
        .unwrap_or(Type::Unit);

    let kind = f
        .params
        .iter()
        .find_map(|p| match p {
            Param::Self_ { mode, .. } => Some(FunctionKind::Instance(*mode)),
            _ => None,
        })
        .unwrap_or(FunctionKind::Static);

    Some(FunctionSig {
        visibility: f.visibility,
        params,
        return_type,
        kind,
        span: f.span,
        type_params: f.type_params.clone(),
    })
}

/// Builds a [`FunctionSig`] from a protocol method declaration, treating all
/// protocol methods as instance functions with borrowed `self`.
fn build_protocol_method_sig(
    m: &ProtocolMethod,
    known_structs: &[&str],
    known_enums: &[&str],
    extra_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
) -> Option<FunctionSig> {
    let mut all_tp: Vec<&str> = m.type_params.iter().map(|s| s.as_str()).collect();
    all_tp.extend_from_slice(extra_type_params);
    if !all_tp.contains(&"Self") {
        all_tp.push("Self");
    }

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
                ty: resolve_type_expr_with_params(
                    type_expr,
                    known_structs,
                    known_enums,
                    &all_tp,
                    known_type_aliases,
                ),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();

    let return_type = m
        .return_type
        .as_ref()
        .map(|t| {
            resolve_type_expr_with_params(
                t,
                known_structs,
                known_enums,
                &all_tp,
                known_type_aliases,
            )
        })
        .unwrap_or(Type::Unit);

    let kind = m
        .params
        .iter()
        .find_map(|p| match p {
            Param::Self_ { mode, .. } => Some(FunctionKind::Instance(*mode)),
            _ => None,
        })
        .unwrap_or(FunctionKind::Static);

    Some(FunctionSig {
        visibility: Visibility::Public,
        params,
        return_type,
        kind,
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
    for (name, info) in &source.types {
        ctx.types.insert(name.clone(), info.clone());
    }
    ctx
}

/// Registers an imported module's context under its qualifier name, or
/// emits an error if the qualifier already exists (duplicate import).
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

/// Returns `true` if two function signatures are structurally identical
/// (ignoring source spans, which differ between direct and transitive imports).
fn same_function(a: &FunctionSig, b: &FunctionSig) -> bool {
    a.params == b.params && a.return_type == b.return_type && a.type_params == b.type_params
}

/// Returns `true` if two type infos are structurally identical
/// (ignoring source spans).
fn same_type(a: &TypeInfo, b: &TypeInfo) -> bool {
    a.kind == b.kind && a.type_params == b.type_params
}

/// Copies all public functions, structs, and enums from `source` into `ctx`,
/// detecting duplicate imports across modules. Diamond imports (the same type
/// arriving through two paths) are silently deduplicated.
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
            if let Some(existing) = ctx.functions.get(name)
                && !same_function(existing, sig)
            {
                ctx.error(
                    format!("`{name}` is already imported from another module"),
                    span,
                );
            }
        } else if !ctx.functions.contains_key(name) {
            imported_names.insert(name.clone());
            let mut cloned = sig.clone();
            cloned.visibility = Visibility::Public;
            ctx.functions.insert(name.clone(), cloned);
        }
    }
    for (name, info) in &source.types {
        if imported_names.contains(name) {
            if let Some(existing) = ctx.types.get(name)
                && !same_type(existing, info)
            {
                ctx.error(
                    format!(
                        "{} `{name}` is already imported from another module",
                        info.kind_label()
                    ),
                    span,
                );
            }
        } else if !ctx.types.contains_key(name) {
            imported_names.insert(name.clone());
            ctx.types.insert(name.clone(), info.clone());
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
            if let Some(existing) = ctx.functions.get(name)
                && !same_function(existing, sig)
            {
                ctx.error(
                    format!("`{name}` is already imported from another module"),
                    span,
                );
            }
        } else if !ctx.functions.contains_key(name) {
            imported_names.insert(name.to_string());
            let mut cloned = sig.clone();
            cloned.visibility = Visibility::Public;
            ctx.functions.insert(name.to_string(), cloned);
        }
        return;
    }
    if let Some(info) = source.types.get(name) {
        if imported_names.contains(name) {
            if let Some(existing) = ctx.types.get(name)
                && !same_type(existing, info)
            {
                ctx.error(
                    format!(
                        "{} `{name}` is already imported from another module",
                        info.kind_label()
                    ),
                    span,
                );
            }
        } else if !ctx.types.contains_key(name) {
            imported_names.insert(name.to_string());
            ctx.types.insert(name.to_string(), info.clone());
        }
        return;
    }
    ctx.error(
        format!("`{name}` not found in module `{module_path}`"),
        span,
    );
}

/// Auto-derives `Debug` protocol methods (`format`, `inspect`) on all struct
/// and enum types that don't already have them. Must be called after merging
/// the stdlib context so the `Debug` protocol definition is available.
pub fn auto_derive_debug(ctx: &mut TypeContext) {
    let type_names: Vec<String> = ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_struct() || ti.is_enum())
        .map(|(n, _)| n.clone())
        .collect();

    let format_sig = FunctionSig {
        visibility: Visibility::Public,
        params: vec![],
        return_type: Type::Primitive(Primitive::String),
        kind: FunctionKind::Instance(PassMode::Borrow),
        span: Span::zero(),
        type_params: Vec::new(),
    };

    let inspect_sig = FunctionSig {
        visibility: Visibility::Public,
        params: vec![],
        return_type: Type::Unknown,
        kind: FunctionKind::Instance(PassMode::Move),
        span: Span::zero(),
        type_params: Vec::new(),
    };

    for name in &type_names {
        if let Some(ti) = ctx.types.get_mut(name) {
            if !ti.functions.contains_key("format") {
                ti.functions
                    .insert("format".to_string(), format_sig.clone());
            }
            if !ti.functions.contains_key("inspect") {
                ti.functions
                    .insert("inspect".to_string(), inspect_sig.clone());
            }
        }
    }
}

/// Converts a `ProtocolMethod` with a default body into a `Function` AST node
/// suitable for compilation as `{target_type}_{method_name}`.
/// `type_param_map` maps protocol type params to concrete type names (e.g., `T` -> `String`).
fn synthesize_default_fn(
    pm: &ProtocolMethod,
    target_type: &str,
    type_param_map: &[(&str, &str)],
) -> Function {
    let mut params = pm.params.clone();
    for p in &mut params {
        if let Param::Regular { type_expr, .. } = p {
            substitute_self_in_type_expr(type_expr, target_type);
            for (from, to) in type_param_map {
                substitute_named_in_type_expr(type_expr, from, to);
            }
        }
    }
    let mut return_type = pm.return_type.clone();
    if let Some(rt) = &mut return_type {
        substitute_self_in_type_expr(rt, target_type);
        for (from, to) in type_param_map {
            substitute_named_in_type_expr(rt, from, to);
        }
    }
    let mut body = pm.body.clone().unwrap_or_default();
    for stmt in &mut body {
        substitute_self_in_statement(stmt, target_type);
        for (from, to) in type_param_map {
            substitute_named_in_statement(stmt, from, to);
        }
    }
    Function {
        annotation: pm.annotation.clone(),
        visibility: Visibility::Public,
        name: pm.name.clone(),
        type_params: pm.type_params.clone(),
        params,
        return_type,
        body,
        span: pm.span,
    }
}

/// Replaces a named type reference (e.g., a protocol type parameter like `T`)
/// with a concrete type name throughout a type expression tree.
fn substitute_named_in_type_expr(te: &mut TypeExpr, from: &str, to: &str) {
    match te {
        TypeExpr::Named { path, .. } if path.len() == 1 && path[0] == from => {
            path[0] = to.to_string();
        }
        TypeExpr::Generic { args, .. } => {
            for arg in args {
                substitute_named_in_type_expr(arg, from, to);
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for p in params {
                substitute_named_in_type_expr(p, from, to);
            }
            substitute_named_in_type_expr(return_type, from, to);
        }
        TypeExpr::Union { types, .. } => {
            for t in types {
                substitute_named_in_type_expr(t, from, to);
            }
        }
        _ => {}
    }
}

/// Recursively renames type references from `from` to `to` inside a statement,
/// used when synthesizing protocol defaults for a concrete type parameter.
fn substitute_named_in_statement(stmt: &mut Statement, from: &str, to: &str) {
    match stmt {
        Statement::Expr(expr) => substitute_named_in_expr(expr, from, to),
        Statement::Assignment {
            type_annotation,
            value,
            ..
        } => {
            if let Some(ta) = type_annotation {
                substitute_named_in_type_expr(ta, from, to);
            }
            substitute_named_in_expr(value, from, to);
        }
        Statement::CompoundAssign { value, .. } => {
            substitute_named_in_expr(value, from, to);
        }
        Statement::Return { value, .. } => {
            if let Some(v) = value {
                substitute_named_in_expr(v, from, to);
            }
        }
        Statement::Break { .. } => {}
    }
}

/// Renames type references from `from` to `to` inside match/receive arms.
fn substitute_named_in_arms(arms: &mut [expo_ast::ast::MatchArm], from: &str, to: &str) {
    for arm in arms {
        substitute_named_in_pattern(&mut arm.pattern, from, to);
        if let Some(g) = &mut arm.guard {
            substitute_named_in_expr(g, from, to);
        }
        for s in &mut arm.body {
            substitute_named_in_statement(s, from, to);
        }
    }
}

/// Recursively renames type references from `from` to `to` inside an expression tree.
fn substitute_named_in_expr(expr: &mut Expr, from: &str, to: &str) {
    match expr {
        Expr::Match { subject, arms, .. } => {
            substitute_named_in_expr(subject, from, to);
            substitute_named_in_arms(arms, from, to);
        }
        Expr::Receive { arms, .. } => {
            substitute_named_in_arms(arms, from, to);
        }
        Expr::Closure {
            return_type, body, ..
        } => {
            if let Some(rt) = return_type {
                substitute_named_in_type_expr(rt, from, to);
            }
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        Expr::Call { callee, args, .. } => {
            substitute_named_in_expr(callee, from, to);
            for a in args {
                substitute_named_in_expr(&mut a.value, from, to);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            substitute_named_in_expr(receiver, from, to);
            for a in args {
                substitute_named_in_expr(&mut a.value, from, to);
            }
        }
        Expr::Binary { left, right, .. } => {
            substitute_named_in_expr(left, from, to);
            substitute_named_in_expr(right, from, to);
        }
        Expr::Unary { operand, .. } => substitute_named_in_expr(operand, from, to),
        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for s in then_body {
                substitute_named_in_statement(s, from, to);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    substitute_named_in_statement(s, from, to);
                }
            }
        }
        Expr::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            substitute_named_in_pattern(pattern, from, to);
            substitute_named_in_expr(iterable, from, to);
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        Expr::While {
            condition, body, ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        Expr::Loop { body, .. } | Expr::Arena { body, .. } => {
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        Expr::FieldAccess { receiver, .. } => substitute_named_in_expr(receiver, from, to),
        Expr::Group { expr, .. } | Expr::Spawn { expr, .. } => {
            substitute_named_in_expr(expr, from, to)
        }
        Expr::Cond {
            arms, else_body, ..
        } => {
            for arm in arms {
                substitute_named_in_expr(&mut arm.condition, from, to);
                for s in &mut arm.body {
                    substitute_named_in_statement(s, from, to);
                }
            }
            if let Some(eb) = else_body {
                for s in eb {
                    substitute_named_in_statement(s, from, to);
                }
            }
        }
        Expr::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    substitute_named_in_expr(expr, from, to);
                }
            }
        }
        Expr::List { elements, .. } => {
            for e in elements {
                substitute_named_in_expr(e, from, to);
            }
        }
        Expr::StructConstruction { fields, .. } => {
            for f in fields {
                substitute_named_in_expr(&mut f.value, from, to);
            }
        }
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            substitute_named_in_expr(condition, from, to);
            substitute_named_in_expr(then_expr, from, to);
            substitute_named_in_expr(else_expr, from, to);
        }
        Expr::Unless {
            condition, body, ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        Expr::ShortClosure { body, .. } => substitute_named_in_expr(body, from, to),
        Expr::Map { entries, .. } => {
            for (k, v) in entries {
                substitute_named_in_expr(k, from, to);
                substitute_named_in_expr(v, from, to);
            }
        }
        Expr::BinaryLiteral { segments, .. } => {
            for seg in segments {
                substitute_named_in_expr(&mut seg.value, from, to);
                if let Some(sz) = &mut seg.size {
                    substitute_named_in_expr(sz, from, to);
                }
            }
        }
        Expr::Ident { .. }
        | Expr::Literal { .. }
        | Expr::Self_ { .. }
        | Expr::EnumConstruction { .. } => {}
    }
}

/// Renames type references from `from` to `to` inside a pattern tree.
fn substitute_named_in_pattern(pat: &mut Pattern, from: &str, to: &str) {
    match pat {
        Pattern::TypedBinding { type_expr, .. } => {
            substitute_named_in_type_expr(type_expr, from, to);
        }
        Pattern::EnumTuple { elements, .. } => {
            for e in elements {
                substitute_named_in_pattern(e, from, to);
            }
        }
        Pattern::EnumStruct { fields, .. } => {
            for f in fields {
                if let Some(p) = &mut f.pattern {
                    substitute_named_in_pattern(p, from, to);
                }
            }
        }
        Pattern::Constructor { elements, .. } => {
            for e in elements {
                substitute_named_in_pattern(e, from, to);
            }
        }
        Pattern::List { elements, .. } => {
            for e in elements {
                substitute_named_in_pattern(e, from, to);
            }
        }
        Pattern::Binary { segments, .. } => {
            for seg in segments {
                substitute_named_in_expr(&mut seg.value, from, to);
                if let Some(sz) = &mut seg.size {
                    substitute_named_in_expr(sz, from, to);
                }
            }
        }
        Pattern::Or { patterns, .. } => {
            for p in patterns {
                substitute_named_in_pattern(p, from, to);
            }
        }
        Pattern::Wildcard { .. }
        | Pattern::Literal { .. }
        | Pattern::Binding { .. }
        | Pattern::EnumUnit { .. } => {}
    }
}

/// Replaces `Self` type references with the concrete `target` type name in a statement,
/// used when inlining synthesized protocol default function bodies.
fn substitute_self_in_statement(stmt: &mut Statement, target: &str) {
    match stmt {
        Statement::Expr(expr) => substitute_self_in_expr(expr, target),
        Statement::Assignment {
            type_annotation,
            value,
            ..
        } => {
            if let Some(ta) = type_annotation {
                substitute_self_in_type_expr(ta, target);
            }
            substitute_self_in_expr(value, target);
        }
        Statement::CompoundAssign { value, .. } => substitute_self_in_expr(value, target),
        Statement::Return { value, .. } => {
            if let Some(v) = value {
                substitute_self_in_expr(v, target);
            }
        }
        Statement::Break { .. } => {}
    }
}

/// Replaces `Self` type references with the concrete `target` name in an expression tree.
fn substitute_self_in_expr(expr: &mut Expr, target: &str) {
    match expr {
        Expr::Match { subject, arms, .. } => {
            substitute_self_in_expr(subject, target);
            for arm in arms {
                substitute_self_in_pattern(&mut arm.pattern, target);
                if let Some(g) = &mut arm.guard {
                    substitute_self_in_expr(g, target);
                }
                for s in &mut arm.body {
                    substitute_self_in_statement(s, target);
                }
            }
        }
        Expr::Receive { arms, .. } => {
            for arm in arms {
                substitute_self_in_pattern(&mut arm.pattern, target);
                if let Some(g) = &mut arm.guard {
                    substitute_self_in_expr(g, target);
                }
                for s in &mut arm.body {
                    substitute_self_in_statement(s, target);
                }
            }
        }
        Expr::Closure {
            return_type, body, ..
        } => {
            if let Some(rt) = return_type {
                substitute_self_in_type_expr(rt, target);
            }
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        Expr::Call { callee, args, .. } => {
            substitute_self_in_expr(callee, target);
            for a in args {
                substitute_self_in_expr(&mut a.value, target);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            substitute_self_in_expr(receiver, target);
            for a in args {
                substitute_self_in_expr(&mut a.value, target);
            }
        }
        Expr::Binary { left, right, .. } => {
            substitute_self_in_expr(left, target);
            substitute_self_in_expr(right, target);
        }
        Expr::Unary { operand, .. } => substitute_self_in_expr(operand, target),
        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            substitute_self_in_expr(condition, target);
            for s in then_body {
                substitute_self_in_statement(s, target);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    substitute_self_in_statement(s, target);
                }
            }
        }
        Expr::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            substitute_self_in_pattern(pattern, target);
            substitute_self_in_expr(iterable, target);
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        Expr::While {
            condition, body, ..
        } => {
            substitute_self_in_expr(condition, target);
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        Expr::Loop { body, .. } | Expr::Arena { body, .. } => {
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        Expr::FieldAccess { receiver, .. } => substitute_self_in_expr(receiver, target),
        Expr::Group { expr, .. } | Expr::Spawn { expr, .. } => {
            substitute_self_in_expr(expr, target)
        }
        Expr::Cond {
            arms, else_body, ..
        } => {
            for arm in arms {
                substitute_self_in_expr(&mut arm.condition, target);
                for s in &mut arm.body {
                    substitute_self_in_statement(s, target);
                }
            }
            if let Some(eb) = else_body {
                for s in eb {
                    substitute_self_in_statement(s, target);
                }
            }
        }
        Expr::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    substitute_self_in_expr(expr, target);
                }
            }
        }
        Expr::List { elements, .. } => {
            for e in elements {
                substitute_self_in_expr(e, target);
            }
        }
        Expr::StructConstruction { fields, .. } => {
            for f in fields {
                substitute_self_in_expr(&mut f.value, target);
            }
        }
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            substitute_self_in_expr(condition, target);
            substitute_self_in_expr(then_expr, target);
            substitute_self_in_expr(else_expr, target);
        }
        Expr::Unless {
            condition, body, ..
        } => {
            substitute_self_in_expr(condition, target);
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        Expr::ShortClosure { body, .. } => substitute_self_in_expr(body, target),
        Expr::Map { entries, .. } => {
            for (k, v) in entries {
                substitute_self_in_expr(k, target);
                substitute_self_in_expr(v, target);
            }
        }
        Expr::BinaryLiteral { segments, .. } => {
            for seg in segments {
                substitute_self_in_expr(&mut seg.value, target);
                if let Some(sz) = &mut seg.size {
                    substitute_self_in_expr(sz, target);
                }
            }
        }
        Expr::Ident { .. }
        | Expr::Literal { .. }
        | Expr::Self_ { .. }
        | Expr::EnumConstruction { .. } => {}
    }
}

/// Replaces `Self` type references with the concrete `target` name in a pattern tree.
fn substitute_self_in_pattern(pat: &mut Pattern, target: &str) {
    match pat {
        Pattern::TypedBinding { type_expr, .. } => {
            substitute_self_in_type_expr(type_expr, target);
        }
        Pattern::EnumTuple { elements, .. } => {
            for e in elements {
                substitute_self_in_pattern(e, target);
            }
        }
        Pattern::EnumStruct { fields, .. } => {
            for f in fields {
                if let Some(p) = &mut f.pattern {
                    substitute_self_in_pattern(p, target);
                }
            }
        }
        Pattern::Constructor { elements, .. } => {
            for e in elements {
                substitute_self_in_pattern(e, target);
            }
        }
        Pattern::List { elements, .. } => {
            for e in elements {
                substitute_self_in_pattern(e, target);
            }
        }
        Pattern::Binary { segments, .. } => {
            for seg in segments {
                substitute_self_in_expr(&mut seg.value, target);
                if let Some(sz) = &mut seg.size {
                    substitute_self_in_expr(sz, target);
                }
            }
        }
        Pattern::Or { patterns, .. } => {
            for p in patterns {
                substitute_self_in_pattern(p, target);
            }
        }
        Pattern::Wildcard { .. }
        | Pattern::Literal { .. }
        | Pattern::Binding { .. }
        | Pattern::EnumUnit { .. } => {}
    }
}

/// Replaces `TypeExpr::Self_` with a `TypeExpr::Named` pointing to the
/// concrete target type throughout a type expression tree.
fn substitute_self_in_type_expr(te: &mut TypeExpr, target: &str) {
    match te {
        TypeExpr::Self_ { span } => {
            *te = TypeExpr::Named {
                path: vec![target.to_string()],
                span: *span,
            };
        }
        TypeExpr::Generic { args, .. } => {
            for arg in args {
                substitute_self_in_type_expr(arg, target);
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for p in params {
                substitute_self_in_type_expr(p, target);
            }
            substitute_self_in_type_expr(return_type, target);
        }
        TypeExpr::Union { types, .. } => {
            for t in types {
                substitute_self_in_type_expr(t, target);
            }
        }
        TypeExpr::Named { .. } | TypeExpr::Unit { .. } => {}
    }
}

/// Replaces `Type::TypeVar("Self")` with the concrete impl target type in a
/// method signature's params and return type.
fn substitute_self_type(mut sig: FunctionSig, self_type: &Type) -> FunctionSig {
    if matches!(self_type, Type::Unknown) {
        return sig;
    }
    let subst = HashMap::from([("Self".to_string(), self_type.clone())]);
    sig.return_type = crate::types::substitute_preserving(&sig.return_type, &subst);
    for p in &mut sig.params {
        p.ty = crate::types::substitute_preserving(&p.ty, &subst);
    }
    sig
}

/// Whether an expression is valid as a compile-time constant initializer.
fn is_const_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Literal { .. } => true,
        Expr::String { parts, .. } => parts
            .iter()
            .all(|p| matches!(p, StringPart::Literal { .. })),
        _ => false,
    }
}
