use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use expo_ast::ast::{
    Diagnostic, EnumConstructionData, EnumVariantData, Expr, ExprKind, File, Function, ImplMember,
    Item, Literal, Param, Pattern, ProtocolMethod, Severity, Statement, StringPart, TypeExpr,
};
use expo_ast::identifier::Identifier;
use expo_ast::span::Span;

use crate::registry::GlobalRegistry;

use crate::context::{
    FunctionKind, FunctionSig, ParamInfo, ProtocolInfo, TypeContext, TypeInfo, TypeKind,
    VariantData, VariantInfo, Visibility,
};
use crate::types::package_from_str;
use crate::types::{
    Package, Primitive, Type, TypeIdentifier, named, resolve_type_expr_with_params,
};

/// Resolves a bare type name to a [`TypeIdentifier`] by trying the current
/// package first, then falling back to `Package::Global`.
fn resolve_type_key(ctx: &TypeContext, name: &str, package: &str) -> Option<TypeIdentifier> {
    let id = if package == "Global" {
        TypeIdentifier::global(name)
    } else {
        TypeIdentifier::new(package, name)
    };
    if ctx.types.contains_key(&id) {
        return Some(id);
    }
    let global_id = TypeIdentifier::global(name);
    if ctx.types.contains_key(&global_id) {
        return Some(global_id);
    }
    None
}

/// Pre-collected struct and enum names across all files in the program,
/// plus the set of known package labels. Passed into [`collect`] so that type
/// resolution sees every type name from the start and can validate qualified
/// `pkg.Type` paths against the known package set on the first pass.
pub struct GlobalNames {
    pub enum_names: HashSet<String>,
    pub packages: BTreeSet<Package>,
    pub struct_names: HashSet<String>,
}

/// Scans all files for struct and enum names without resolving any types.
/// `packages` is the set of package labels visible to the program (typically
/// derived from the file graph: `Package::Global` for `Global.*` files and
/// `Package::Named(...)` for everything else). Together they form the
/// first phase of a two-phase collection: names and packages are gathered
/// globally, then passed into [`collect`] so cross-file type references
/// resolve correctly on the first pass.
pub fn collect_all_names(files: &[&File], packages: BTreeSet<Package>) -> GlobalNames {
    let mut names = GlobalNames {
        enum_names: HashSet::new(),
        packages,
        struct_names: HashSet::new(),
    };
    for file in files {
        for item in &file.items {
            match item {
                Item::Alias(_) => {}
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

/// Walks all top-level items in a file and builds a [`TypeContext`] containing
/// function signatures, struct definitions, and enum definitions.
/// Requires [`GlobalNames`] from [`collect_all_names`] so that cross-file
/// type references (e.g. imported struct names) resolve correctly.
/// The `package` parameter identifies which package this file belongs to
/// (e.g. `"Global"`, `"JSON"`, or the project name). It's stored on each
/// [`TypeInfo`]'s [`TypeIdentifier`] for package-aware collision detection.
pub fn collect(file: &File, global_names: &GlobalNames, package: &str) -> TypeContext {
    let mut ctx = TypeContext::new();
    ctx.current_package = Some(package_from_str(package));

    let struct_names: Vec<&str> = global_names
        .struct_names
        .iter()
        .map(|s| s.as_str())
        .collect();
    let enum_names: Vec<&str> = global_names.enum_names.iter().map(|s| s.as_str()).collect();
    let known_packages = &global_names.packages;

    // Pre-pass: collect type aliases so they're available for resolving
    // function signatures and struct/enum fields in the main pass.
    for item in &file.items {
        if let Item::TypeAlias(ta) = item {
            let resolved = resolve_type_expr_with_params(
                &ta.type_expr,
                &struct_names,
                &enum_names,
                &[],
                &std::collections::BTreeMap::new(),
                known_packages,
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

    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Enum(e) => {
                let tp_refs: Vec<&str> = e.type_params.iter().map(|s| s.name.as_str()).collect();
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
                                            known_packages,
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
                                            known_packages,
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
                let type_id = if package == "Global" {
                    TypeIdentifier::global(&e.name)
                } else {
                    TypeIdentifier::new(package, &e.name)
                };
                ctx.generic_enum_asts.insert(e.name.clone(), e.clone());
                ctx.insert_type(
                    type_id,
                    TypeInfo {
                        identifier: TypeIdentifier::unresolved(&e.name),
                        functions: BTreeMap::new(),
                        kind: TypeKind::Enum { variants },
                        span: e.span,
                        type_params: e.type_params.clone(),
                    },
                );
                register_global(&mut ctx, package, &e.name, e.span, GlobalKind::Enum);
                let self_type = named(&e.name);
                let resolve = ResolveCtx {
                    enum_names: &enum_names,
                    packages: known_packages,
                    struct_names: &struct_names,
                    tp_refs: &tp_refs,
                    type_aliases: &type_aliases,
                };
                for f in &e.functions {
                    register_method_on_type(&mut ctx, f, &e.name, &self_type, &resolve, package);
                }
            }
            Item::Function(f) => {
                if let Some(sig) =
                    build_function_sig(f, &struct_names, &enum_names, &type_aliases, known_packages)
                {
                    if !f.type_params.is_empty() {
                        ctx.generic_function_asts.insert(f.name.clone(), f.clone());
                    }
                    ctx.functions.insert(f.name.clone(), sig);
                    register_global(&mut ctx, package, &f.name, f.span, GlobalKind::Function);
                }
            }
            Item::Impl(impl_block) => {
                let (target_name, impl_type_params, is_specialized) = match &impl_block.target {
                    TypeExpr::Named { path, .. } if path.len() == 1 => {
                        (path[0].clone(), Vec::new(), false)
                    }
                    TypeExpr::Generic { path, args, .. } if path.len() == 1 => {
                        let mut params = Vec::new();
                        let mut concrete_count = 0;
                        for a in args {
                            if let TypeExpr::Named { path: p, .. } = a
                                && p.len() == 1
                            {
                                let name = &p[0];
                                if Primitive::from_name(name).is_some()
                                    || struct_names.contains(&name.as_str())
                                    || enum_names.contains(&name.as_str())
                                {
                                    concrete_count += 1;
                                } else {
                                    params.push(name.clone());
                                }
                            }
                        }
                        let specialized = concrete_count > 0 && params.is_empty();
                        if concrete_count > 0 && !params.is_empty() {
                            ctx.error(
                                format!(
                                    "impl `{}` mixes concrete types and type parameters",
                                    path[0]
                                ),
                                impl_block.span,
                            );
                            continue;
                        }
                        (path[0].clone(), params, specialized)
                    }
                    _ => continue,
                };
                let is_generic_impl = !impl_type_params.is_empty();

                if is_specialized {
                    let concrete_types: Vec<Type> =
                        if let TypeExpr::Generic { args, .. } = &impl_block.target {
                            args.iter()
                                .map(|a| {
                                    resolve_type_expr_with_params(
                                        a,
                                        &struct_names,
                                        &enum_names,
                                        &[],
                                        &type_aliases,
                                        known_packages,
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };

                    let type_id = resolve_type_key(&ctx, &target_name, package)
                        .unwrap_or_else(|| TypeIdentifier::global(&target_name));

                    ctx.specialized_impl_asts
                        .entry(type_id.clone())
                        .or_default()
                        .push((concrete_types.clone(), impl_block.clone()));

                    let self_type = resolve_type_expr_with_params(
                        &impl_block.target,
                        &struct_names,
                        &enum_names,
                        &[],
                        &type_aliases,
                        known_packages,
                    );

                    let mut method_sigs: BTreeMap<String, FunctionSig> = BTreeMap::new();
                    for member in &impl_block.members {
                        if let ImplMember::Function(f) = member {
                            let sig = build_function_sig_with_params(
                                f,
                                &struct_names,
                                &enum_names,
                                &[],
                                &type_aliases,
                                known_packages,
                            );
                            let Some(sig) = sig else { continue };
                            let sig = substitute_self_type(sig, &self_type);
                            method_sigs.insert(f.name.clone(), sig);
                        }
                    }

                    ctx.specialized_methods
                        .entry(type_id)
                        .or_default()
                        .push((concrete_types, method_sigs));

                    continue;
                }

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
                    known_packages,
                );

                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member {
                        let sig = build_function_sig_with_params(
                            f,
                            &struct_names,
                            &enum_names,
                            &tp_refs,
                            &type_aliases,
                            known_packages,
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

                        let ti = resolve_type_key(&ctx, &target_name, package)
                            .and_then(|id| ctx.get_type_mut(&id));
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
                            let prim_id = TypeIdentifier::global(&target_name);
                            let ti = ctx
                                .types
                                .entry(prim_id.clone())
                                .or_insert_with(|| TypeInfo {
                                    identifier: prim_id,
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
                        .map(|pi| pi.type_params.iter().map(|tp| tp.name.clone()).collect())
                        .unwrap_or_default();
                    let proto_type_arg_exprs: Vec<&TypeExpr> =
                        if let Some(TypeExpr::Generic { args, .. }) = &impl_block.trait_expr {
                            args.iter().collect()
                        } else {
                            Vec::new()
                        };
                    let type_param_map: Vec<(&str, &TypeExpr)> = proto_type_param_names
                        .iter()
                        .zip(proto_type_arg_exprs.iter())
                        .map(|(k, v)| (k.as_str(), *v))
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
                                known_packages,
                            );
                            if let Some(sig) = sig {
                                let sig = substitute_self_type(sig, &self_type);
                                if let Some(id) = resolve_type_key(&ctx, &target_name, package)
                                    && let Some(ti) = ctx.get_type_mut(&id)
                                {
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
                                        known_packages,
                                    )
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                    let impl_key =
                        resolve_type_key(&ctx, &target_name, package).unwrap_or_else(|| {
                            if package == "Global" {
                                TypeIdentifier::global(&target_name)
                            } else {
                                TypeIdentifier::new(package, &target_name)
                            }
                        });
                    ctx.protocol_impls
                        .entry(impl_key)
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
                        known_packages,
                    )
                } else {
                    match &c.value.kind {
                        ExprKind::Literal {
                            value: Literal::Bool(_),
                        } => Type::Primitive(Primitive::Bool),
                        ExprKind::Literal {
                            value: Literal::Int(_),
                        } => Type::Primitive(Primitive::I64),
                        ExprKind::Literal {
                            value: Literal::Float(_),
                        } => Type::Primitive(Primitive::F64),
                        ExprKind::String { .. } => Type::Primitive(Primitive::String),
                        ExprKind::EnumConstruction {
                            type_path,
                            data: EnumConstructionData::Unit,
                            ..
                        } => {
                            let enum_name = type_path.join(".");
                            if enum_names.contains(&enum_name.as_str()) {
                                named(&enum_name)
                            } else {
                                ctx.error(
                                    format!("constant `{}`: unknown enum `{}`", c.name, enum_name),
                                    c.span,
                                );
                                Type::Error
                            }
                        }
                        ExprKind::StructConstruction {
                            type_path, fields, ..
                        } if fields.iter().all(|f| is_const_expr(&f.value)) => {
                            let name = type_path.join(".");
                            if struct_names.contains(&name.as_str()) {
                                named(&name)
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
                let const_id = TypeIdentifier {
                    package: package_from_str(package),
                    name: c.name.clone(),
                };
                if ctx.constants.contains_key(&const_id) {
                    ctx.error(format!("duplicate constant `{}`", c.name), c.span);
                } else if ctx.functions.contains_key(&c.name)
                    || resolve_type_key(&ctx, &c.name, package).is_some()
                {
                    ctx.error(
                        format!(
                            "constant `{}` conflicts with an existing declaration",
                            c.name
                        ),
                        c.span,
                    );
                } else {
                    ctx.constants.insert(const_id, ty);
                }
            }
            Item::Protocol(p) => {
                let tp_refs: Vec<&str> = p.type_params.iter().map(|s| s.name.as_str()).collect();
                let mut methods: BTreeMap<String, FunctionSig> = BTreeMap::new();
                let mut default_bodies: BTreeMap<String, ProtocolMethod> = BTreeMap::new();
                for m in &p.methods {
                    if let Some(sig) = build_protocol_method_sig(
                        m,
                        &struct_names,
                        &enum_names,
                        &tp_refs,
                        &type_aliases,
                        known_packages,
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
                register_global(&mut ctx, package, &p.name, p.span, GlobalKind::Protocol);
            }
            Item::Struct(s) => {
                let tp_refs: Vec<&str> = s.type_params.iter().map(|s| s.name.as_str()).collect();
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
                            known_packages,
                        );
                        (f.name.clone(), ty)
                    })
                    .collect();
                let type_id = if package == "Global" {
                    TypeIdentifier::global(&s.name)
                } else {
                    TypeIdentifier::new(package, &s.name)
                };
                ctx.generic_struct_asts.insert(s.name.clone(), s.clone());
                ctx.insert_type(
                    type_id,
                    TypeInfo {
                        identifier: TypeIdentifier::unresolved(&s.name),
                        functions: BTreeMap::new(),
                        kind: TypeKind::Struct { fields },
                        span: s.span,
                        type_params: s.type_params.clone(),
                    },
                );
                register_global(&mut ctx, package, &s.name, s.span, GlobalKind::Struct);
                let self_type = named(&s.name);
                let resolve = ResolveCtx {
                    enum_names: &enum_names,
                    packages: known_packages,
                    struct_names: &struct_names,
                    tp_refs: &tp_refs,
                    type_aliases: &type_aliases,
                };
                for f in &s.functions {
                    register_method_on_type(&mut ctx, f, &s.name, &self_type, &resolve, package);
                }
            }
            Item::TypeAlias(_) => {} // handled in pre-pass above
        }
    }

    resolve_same_package_refs(&mut ctx, package);

    ctx
}

/// Which kind of top-level decl is being registered into the
/// [`crate::registry::GlobalRegistry`].
#[derive(Clone, Copy)]
enum GlobalKind {
    Enum,
    Function,
    Protocol,
    Struct,
}

/// Registers a top-level decl into [`TypeContext::registry`] and emits a
/// "`X` is already defined" diagnostic on collision (pointing at the
/// previously-registered span as a hint). The legacy `types`/`functions`
/// maps are populated separately by the caller -- this is purely the
/// new-identifier shadow registration.
fn register_global(ctx: &mut TypeContext, package: &str, name: &str, span: Span, kind: GlobalKind) {
    let id = Identifier::new(package, vec![name.to_string()]);
    let existing = match kind {
        GlobalKind::Enum => ctx.registry.insert_enum(id, span),
        GlobalKind::Function => ctx.registry.insert_function(id, span),
        GlobalKind::Protocol => ctx.registry.insert_protocol(id, span),
        GlobalKind::Struct => ctx.registry.insert_struct(id, span),
    };
    if let Some(prev) = existing {
        let prev_kind = prev.kind_label();
        let prev_line = prev.span().start.line;
        ctx.error_with_hint(
            format!("`{name}` is already defined"),
            format!("previous {prev_kind} definition is at line {prev_line}"),
            span,
        );
    }
}

/// Walks one file's top-level items and registers each surviving
/// declaration (struct, enum, function, protocol) into `registry` as a
/// path-based [`Identifier`]. Returns one [`Diagnostic`] per duplicate
/// definition; the caller chooses what to do with them.
///
/// Pure registration: no field types resolved, no function signatures
/// built, no internal references touched. The output of this pass is
/// the authoritative "what globally-named decls exist in the program"
/// view that subsequent passes (resolve, check, seal) consume.
///
/// To populate a registry across the whole program, call this once per
/// file with the file's own package, then thread the populated registry
/// into the heavier resolution passes.
pub fn scan_globals(file: &File, package: &str, registry: &mut GlobalRegistry) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for item in &file.items {
        let (name, span, kind) = match item {
            Item::Enum(e) => (e.name.as_str(), e.span, GlobalKind::Enum),
            Item::Function(f) => (f.name.as_str(), f.span, GlobalKind::Function),
            Item::Protocol(p) => (p.name.as_str(), p.span, GlobalKind::Protocol),
            Item::Struct(s) => (s.name.as_str(), s.span, GlobalKind::Struct),
            _ => continue,
        };
        let id = Identifier::new(package, vec![name.to_string()]);
        let existing = match kind {
            GlobalKind::Enum => registry.insert_enum(id, span),
            GlobalKind::Function => registry.insert_function(id, span),
            GlobalKind::Protocol => registry.insert_protocol(id, span),
            GlobalKind::Struct => registry.insert_struct(id, span),
        };
        if let Some(prev) = existing {
            let prev_kind = prev.kind_label();
            let prev_line = prev.span().start.line;
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                message: format!("`{name}` is already defined"),
                hint: Some(format!(
                    "previous {prev_kind} definition is at line {prev_line}"
                )),
                span,
            });
        }
    }
    diagnostics
}

/// Final pass of [`collect`]: rewrites `Package::Unresolved` identifiers that
/// match a type already registered in the current package to that package's
/// qualified form. This ensures that free function signatures, constants, and
/// type aliases defined in the file carry the right package before any
/// merge (which would otherwise require a global-scope resolver to guess).
///
/// Stdlib and cross-package references remain unresolved here; they are
/// handled later by [`crate::resolve::resolve_packages`] via the bare
/// `name_index` entries (stdlib only) or reported as errors.
fn resolve_same_package_refs(ctx: &mut TypeContext, package: &str) {
    let scope = package_from_str(package);
    let local_names: HashSet<String> = ctx
        .types
        .keys()
        .filter(|id| id.package == scope)
        .map(|id| id.name.clone())
        .collect();
    if local_names.is_empty() {
        return;
    }

    for sig in ctx.functions.values_mut() {
        resolve_sig_locally(sig, &local_names, &scope);
    }
    for ty in ctx.constants.values_mut() {
        resolve_type_locally(ty, &local_names, &scope);
    }
    for ty in ctx.type_aliases.values_mut() {
        resolve_type_locally(ty, &local_names, &scope);
    }
    let keys: Vec<TypeIdentifier> = ctx.types.keys().cloned().collect();
    for key in keys {
        if let Some(ti) = ctx.types.get_mut(&key) {
            for sig in ti.functions.values_mut() {
                resolve_sig_locally(sig, &local_names, &scope);
            }
        }
    }
}

fn resolve_sig_locally(sig: &mut FunctionSig, local_names: &HashSet<String>, scope: &Package) {
    for p in &mut sig.params {
        resolve_type_locally(&mut p.ty, local_names, scope);
    }
    resolve_type_locally(&mut sig.return_type, local_names, scope);
}

fn resolve_type_locally(ty: &mut Type, local_names: &HashSet<String>, scope: &Package) {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            if identifier.package == Package::Unresolved && local_names.contains(&identifier.name) {
                identifier.package = scope.clone();
            }
            for arg in type_args {
                resolve_type_locally(arg, local_names, scope);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for p in params {
                resolve_type_locally(&mut p.ty, local_names, scope);
            }
            resolve_type_locally(return_type, local_names, scope);
        }
        Type::Indirect(inner) | Type::Pointer(inner) => {
            resolve_type_locally(inner, local_names, scope);
        }
        Type::Union(members) => {
            for m in members {
                resolve_type_locally(m, local_names, scope);
            }
        }
        Type::Primitive(_) | Type::Parameter(_) | Type::Unit | Type::Unknown | Type::Error => {}
    }
}

/// Synthesizes default protocol method implementations for impl blocks whose
/// protocol info wasn't available during initial collection (e.g. stdlib
/// protocols like `Process`). Must be called after merging the stdlib context.
pub fn synthesize_protocol_defaults(file: &File, ctx: &mut TypeContext, package: &str) {
    let struct_names: Vec<String> = ctx
        .types
        .values()
        .filter(|ti| ti.is_struct())
        .map(|ti| ti.identifier.name.clone())
        .collect();
    let enum_names: Vec<String> = ctx
        .types
        .values()
        .filter(|ti| ti.is_enum())
        .map(|ti| ti.identifier.name.clone())
        .collect();
    let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();
    let type_aliases = ctx.type_aliases.clone();
    let known_packages: BTreeSet<Package> = ctx.package_types.keys().cloned().collect();
    let tp_refs: Vec<&str> = Vec::new();

    for item in &file.items {
        if let Item::Impl(impl_block) = item {
            let target_name = match &impl_block.target {
                TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. }
                    if path.len() == 1 =>
                {
                    path[0].clone()
                }
                _ => continue,
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

            let self_type = if ctx.is_struct(&target_name) || ctx.is_enum(&target_name) {
                named(&target_name)
            } else if let Some(p) = Primitive::from_name(&target_name) {
                Type::Primitive(p)
            } else {
                continue;
            };

            let proto_type_param_names: Vec<String> = ctx
                .protocols
                .get(&proto)
                .map(|pi| pi.type_params.iter().map(|tp| tp.name.clone()).collect())
                .unwrap_or_default();
            let proto_type_arg_exprs: Vec<&TypeExpr> =
                if let Some(TypeExpr::Generic { args, .. }) = &impl_block.trait_expr {
                    args.iter().collect()
                } else {
                    Vec::new()
                };
            let type_param_map: Vec<(&str, &TypeExpr)> = proto_type_param_names
                .iter()
                .zip(proto_type_arg_exprs.iter())
                .map(|(k, v)| (k.as_str(), *v))
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
                        &known_packages,
                    );
                    if let Some(sig) = sig {
                        let sig = substitute_self_type(sig, &self_type);
                        if let Some(id) = resolve_type_key(ctx, &target_name, package)
                            && let Some(ti) = ctx.get_type_mut(&id)
                        {
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

/// Name-resolution context passed to helpers that build function signatures.
struct ResolveCtx<'a> {
    enum_names: &'a [&'a str],
    packages: &'a BTreeSet<Package>,
    struct_names: &'a [&'a str],
    tp_refs: &'a [&'a str],
    type_aliases: &'a BTreeMap<String, Type>,
}

/// Builds a function signature and registers it on the given type's [`TypeInfo`].
/// Shared by both inline functions (defined inside struct/enum) and `impl` blocks.
fn register_method_on_type(
    ctx: &mut TypeContext,
    f: &Function,
    type_name: &str,
    self_type: &Type,
    resolve: &ResolveCtx<'_>,
    package: &str,
) {
    let Some(sig) = build_function_sig_with_params(
        f,
        resolve.struct_names,
        resolve.enum_names,
        resolve.tp_refs,
        resolve.type_aliases,
        resolve.packages,
    ) else {
        return;
    };
    let sig = substitute_self_type(sig, self_type);

    let Some(id) = resolve_type_key(ctx, type_name, package) else {
        return;
    };
    let Some(ti) = ctx.get_type_mut(&id) else {
        return;
    };
    if ti.functions.contains_key(&f.name) {
        ctx.error(
            format!("duplicate function `{}` on `{type_name}`", f.name),
            f.span,
        );
    } else {
        ti.functions.insert(f.name.clone(), sig);
    }
}

/// Builds a [`FunctionSig`] from a function AST, delegating to
/// [`build_function_sig_with_params`] with no extra type parameters.
fn build_function_sig(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
    known_packages: &BTreeSet<Package>,
) -> Option<FunctionSig> {
    build_function_sig_with_params(
        f,
        known_structs,
        known_enums,
        &[],
        known_type_aliases,
        known_packages,
    )
}

/// Resolves a function AST node into a [`FunctionSig`], handling `self` receivers,
/// parameter types, and merging `extra_type_params` from the enclosing type.
fn build_function_sig_with_params(
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
    known_enums: &[&str],
    extra_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
    known_packages: &BTreeSet<Package>,
) -> Option<FunctionSig> {
    let mut all_tp: Vec<&str> = f.type_params.iter().map(|s| s.name.as_str()).collect();
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
                    known_packages,
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
                known_packages,
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
    known_packages: &BTreeSet<Package>,
) -> Option<FunctionSig> {
    let mut all_tp: Vec<&str> = m.type_params.iter().map(|s| s.name.as_str()).collect();
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
                    known_packages,
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
                known_packages,
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

/// Converts a `ProtocolMethod` with a default body into a `Function` AST node
/// suitable for compilation as `{target_type}_{method_name}`.
/// `type_param_map` maps protocol type params to concrete type expressions
/// (e.g., `"T"` -> `TypeExpr::Named { path: ["String"], .. }`).
fn synthesize_default_fn(
    pm: &ProtocolMethod,
    target_type: &str,
    type_param_map: &[(&str, &TypeExpr)],
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
    let body = pm.body.clone().map(|mut stmts| {
        for stmt in &mut stmts {
            substitute_self_in_statement(stmt, target_type);
            for (from, to) in type_param_map {
                substitute_named_in_statement(stmt, from, to);
            }
        }
        stmts
    });
    Function {
        annotations: pm.annotations.clone(),
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
/// with a concrete type expression throughout a type expression tree.
fn substitute_named_in_type_expr(te: &mut TypeExpr, from: &str, to: &TypeExpr) {
    match te {
        TypeExpr::Named { path, .. } if path.len() == 1 && path[0] == from => {
            *te = to.clone();
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

/// Recursively replaces type references from `from` to `to` inside a statement,
/// used when synthesizing protocol defaults for a concrete type parameter.
fn substitute_named_in_statement(stmt: &mut Statement, from: &str, to: &TypeExpr) {
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

/// Replaces type references from `from` to `to` inside match/receive arms.
fn substitute_named_in_arms(arms: &mut [expo_ast::ast::MatchArm], from: &str, to: &TypeExpr) {
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

/// Recursively replaces type references from `from` to `to` inside an expression tree.
fn substitute_named_in_expr(expr: &mut Expr, from: &str, to: &TypeExpr) {
    match &mut expr.kind {
        ExprKind::Match { subject, arms, .. } => {
            substitute_named_in_expr(subject, from, to);
            substitute_named_in_arms(arms, from, to);
        }
        ExprKind::Receive { arms, .. } => {
            substitute_named_in_arms(arms, from, to);
        }
        ExprKind::Closure {
            return_type, body, ..
        } => {
            if let Some(rt) = return_type {
                substitute_named_in_type_expr(rt, from, to);
            }
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            substitute_named_in_expr(callee, from, to);
            for a in args {
                substitute_named_in_expr(&mut a.value, from, to);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            substitute_named_in_expr(receiver, from, to);
            for a in args {
                substitute_named_in_expr(&mut a.value, from, to);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            substitute_named_in_expr(left, from, to);
            substitute_named_in_expr(right, from, to);
        }
        ExprKind::Unary { operand, .. } => substitute_named_in_expr(operand, from, to),
        ExprKind::If {
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
        ExprKind::For {
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
        ExprKind::While {
            condition, body, ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        ExprKind::Loop { body, .. } => {
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        ExprKind::FieldAccess { receiver, .. } => substitute_named_in_expr(receiver, from, to),
        ExprKind::Group { expr, .. } | ExprKind::Spawn { expr, .. } => {
            substitute_named_in_expr(expr, from, to)
        }
        ExprKind::Cond {
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
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    substitute_named_in_expr(expr, from, to);
                }
            }
        }
        ExprKind::List { elements, .. } => {
            for e in elements {
                substitute_named_in_expr(e, from, to);
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for f in fields {
                substitute_named_in_expr(&mut f.value, from, to);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            substitute_named_in_expr(condition, from, to);
            substitute_named_in_expr(then_expr, from, to);
            substitute_named_in_expr(else_expr, from, to);
        }
        ExprKind::Unless {
            condition, body, ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for s in body {
                substitute_named_in_statement(s, from, to);
            }
        }
        ExprKind::ShortClosure { body, .. } => substitute_named_in_expr(body, from, to),
        ExprKind::Map { entries, .. } => {
            for (k, v) in entries {
                substitute_named_in_expr(k, from, to);
                substitute_named_in_expr(v, from, to);
            }
        }
        ExprKind::BinaryLiteral { segments, .. } => {
            for seg in segments {
                substitute_named_in_expr(&mut seg.value, from, to);
                if let Some(sz) = &mut seg.size {
                    substitute_named_in_expr(sz, from, to);
                }
            }
        }
        ExprKind::Ident { .. }
        | ExprKind::Literal { .. }
        | ExprKind::Self_ { .. }
        | ExprKind::EnumConstruction { .. } => {}
    }
}

/// Replaces type references from `from` to `to` inside a pattern tree.
fn substitute_named_in_pattern(pat: &mut Pattern, from: &str, to: &TypeExpr) {
    match pat {
        Pattern::TypedBinding { type_expr, .. } => {
            substitute_named_in_type_expr(type_expr, from, to);
        }
        Pattern::EnumTuple { elements, .. } => {
            for e in elements {
                substitute_named_in_pattern(e, from, to);
            }
        }
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for f in fields {
                substitute_named_in_pattern(&mut f.pattern, from, to);
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
    match &mut expr.kind {
        ExprKind::Match { subject, arms, .. } => {
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
        ExprKind::Receive { arms, .. } => {
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
        ExprKind::Closure {
            return_type, body, ..
        } => {
            if let Some(rt) = return_type {
                substitute_self_in_type_expr(rt, target);
            }
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            substitute_self_in_expr(callee, target);
            for a in args {
                substitute_self_in_expr(&mut a.value, target);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            substitute_self_in_expr(receiver, target);
            for a in args {
                substitute_self_in_expr(&mut a.value, target);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            substitute_self_in_expr(left, target);
            substitute_self_in_expr(right, target);
        }
        ExprKind::Unary { operand, .. } => substitute_self_in_expr(operand, target),
        ExprKind::If {
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
        ExprKind::For {
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
        ExprKind::While {
            condition, body, ..
        } => {
            substitute_self_in_expr(condition, target);
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        ExprKind::Loop { body, .. } => {
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        ExprKind::FieldAccess { receiver, .. } => substitute_self_in_expr(receiver, target),
        ExprKind::Group { expr, .. } | ExprKind::Spawn { expr, .. } => {
            substitute_self_in_expr(expr, target)
        }
        ExprKind::Cond {
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
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    substitute_self_in_expr(expr, target);
                }
            }
        }
        ExprKind::List { elements, .. } => {
            for e in elements {
                substitute_self_in_expr(e, target);
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for f in fields {
                substitute_self_in_expr(&mut f.value, target);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            substitute_self_in_expr(condition, target);
            substitute_self_in_expr(then_expr, target);
            substitute_self_in_expr(else_expr, target);
        }
        ExprKind::Unless {
            condition, body, ..
        } => {
            substitute_self_in_expr(condition, target);
            for s in body {
                substitute_self_in_statement(s, target);
            }
        }
        ExprKind::ShortClosure { body, .. } => substitute_self_in_expr(body, target),
        ExprKind::Map { entries, .. } => {
            for (k, v) in entries {
                substitute_self_in_expr(k, target);
                substitute_self_in_expr(v, target);
            }
        }
        ExprKind::BinaryLiteral { segments, .. } => {
            for seg in segments {
                substitute_self_in_expr(&mut seg.value, target);
                if let Some(sz) = &mut seg.size {
                    substitute_self_in_expr(sz, target);
                }
            }
        }
        ExprKind::Ident { .. }
        | ExprKind::Literal { .. }
        | ExprKind::Self_ { .. }
        | ExprKind::EnumConstruction { .. } => {}
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
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for f in fields {
                substitute_self_in_pattern(&mut f.pattern, target);
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
    match &expr.kind {
        ExprKind::Literal { .. } => true,
        ExprKind::String { parts, .. } => parts
            .iter()
            .all(|p| matches!(p, StringPart::Literal { .. })),
        _ => false,
    }
}
