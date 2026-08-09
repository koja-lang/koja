//! Field-default resolution: declaration-time validation and
//! construction-time fill.
//!
//! Lift stored each default as an *unresolved* AST on the registry
//! field. This module resolves it in two places:
//!
//! - **Declaration**: the walker trial-resolves every default in the
//!   declaring package's scope with no file aliases and no locals,
//!   against the lifted field type. Unknown names, type mismatches,
//!   and out-of-range literals all diagnose on the declaring file.
//! - **Construction**: a site that omits a defaulted field gets a
//!   synthesized [`FieldInit`] cloned from the unresolved default,
//!   resolved with the substituted field type as the expected hint.
//!   Declaration validation already proved the expression clean, so
//!   site resolution uses a scratch diagnostics vec.
//!
//! Both resolutions go through [`resolve_in_declaring_scope`], the
//! same scope shape (declaring package, empty aliases, no locals),
//! so they cannot diverge. Aliased names in defaults are rejected
//! at the declaration with a "write the qualified name" hint.

use koja_ast::ast::{
    AliasDecl, Diagnostic, EnumConstructionData, EnumDecl, EnumVariantData, Expr, ExprKind,
    FieldInit, StructDecl, StructField,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};
use koja_ast::span::Span;

use crate::pipeline::local_scope::LocalScope;
use crate::registry::{
    GlobalKind, GlobalRegistry, RegistryEntry, ResolvedStructField, ResolvedVariantData,
};

use super::coercion::{check_compatible_stamping, mismatch_message};
use super::ctx::ResolverEnv;
use super::expr::resolve_expr_with_expected;

/// Trial-resolve every field default on a struct decl. Called by the
/// walker while it visits the declaring file.
pub(super) fn resolve_struct_defaults(
    decl: &mut StructDecl,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(env.package, decl.path.clone());
    let Some((_, entry)) = env.registry.lookup(&identifier) else {
        return;
    };
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        return;
    };
    let owner_label = entry.identifier.to_string();
    resolve_field_defaults(
        &mut decl.fields,
        &definition.fields,
        &owner_label,
        env,
        diagnostics,
    );
}

/// Trial-resolve every struct-variant field default on an enum decl.
/// Called by the walker while it visits the declaring file.
pub(super) fn resolve_enum_defaults(
    decl: &mut EnumDecl,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(env.package, decl.path.clone());
    let Some((_, entry)) = env.registry.lookup(&identifier) else {
        return;
    };
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        return;
    };
    for variant in &mut decl.variants {
        let EnumVariantData::Struct(fields) = &mut variant.data else {
            continue;
        };
        let Some((_, lifted)) = definition.lookup_variant(&variant.name) else {
            continue;
        };
        let ResolvedVariantData::Struct(declared) = &lifted.data else {
            continue;
        };
        let owner_label = format!("{}.{}", entry.identifier, variant.name);
        resolve_field_defaults(fields, declared, &owner_label, env, diagnostics);
    }
}

fn resolve_field_defaults(
    fields: &mut [StructField],
    declared: &[ResolvedStructField],
    owner_label: &str,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in fields.iter_mut() {
        let Some(default) = field.default.as_mut() else {
            continue;
        };
        let Some(lifted) = declared.iter().find(|f| f.name == field.name) else {
            continue;
        };
        // Lift's shape check rejected this default (`None` slot), so
        // trial-resolving it would only stack confusing follow-ups.
        if lifted.default.is_none() {
            continue;
        }
        resolve_declared_default(
            default,
            &lifted.ty,
            &field.name,
            owner_label,
            env,
            diagnostics,
        );
    }
}

/// Trial-resolve one default against its lifted field type in the
/// declaring package's scope. Diagnostics land on the default
/// expression. When resolution only succeeds through the file's
/// aliases, the raw errors are replaced with one qualified-name hint.
fn resolve_declared_default(
    default: &mut Expr,
    field_ty: &ResolvedType,
    field_name: &str,
    owner_label: &str,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let pristine = default.clone();
    let mut trial = Vec::new();
    resolve_in_declaring_scope(
        default,
        field_ty,
        env.package,
        &[],
        env.registry,
        &mut trial,
    );

    if !trial.is_empty() && !env.file_aliases.is_empty() {
        let mut aliased = pristine;
        let mut scratch = Vec::new();
        resolve_in_declaring_scope(
            &mut aliased,
            field_ty,
            env.package,
            env.file_aliases,
            env.registry,
            &mut scratch,
        );
        if scratch.is_empty() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "default for field `{field_name}` of `{owner_label}` cannot use an \
                     `alias` shorthand. Write the qualified name",
                ),
                default.span,
            ));
            return;
        }
    }
    if !trial.is_empty() {
        diagnostics.append(&mut trial);
        return;
    }

    let actual = default.resolution.clone();
    if !actual.is_resolved() || !field_ty.is_resolved() {
        return;
    }
    if let Some(mismatch) = check_compatible_stamping(default, &actual, field_ty, env.registry) {
        let subject = format!("default for field `{field_name}` of `{owner_label}`");
        diagnostics.push(Diagnostic::error(
            mismatch_message(&subject, &mismatch, field_ty, &actual, env.registry),
            default.span,
        ));
    }
}

/// Resolve `expr` with `expected` as the hint in a fresh scope:
/// `package`, the given alias roster, and no locals. Serving the
/// declaration trial, the alias probe, and the construction-site
/// fill from one function is what keeps declaration-time and
/// site-time resolution identical.
fn resolve_in_declaring_scope(
    expr: &mut Expr,
    expected: &ResolvedType,
    package: &str,
    aliases: &[AliasDecl],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut env = ResolverEnv {
        file_aliases: aliases,
        package,
        registry,
    };
    let mut scope = LocalScope::new();
    let mut resolver = env.make_resolver(None, None, &[], &mut scope);
    resolve_expr_with_expected(expr, Some(expected), &mut resolver, diagnostics);
}

/// Synthesize the omitted field's init at a construction site:
/// clone the stored default, mark its spans synthetic (so LSP
/// position lookups in the declaring file skip it), and re-resolve
/// it against the substituted field type in the declaring package's
/// scope.
pub(super) fn synthesize_default_init(
    declared_field: &ResolvedStructField,
    owner_id: GlobalRegistryId,
    construction_span: Span,
    registry: &GlobalRegistry,
) -> Option<FieldInit> {
    let default = declared_field.default.as_ref()?;
    let mut value = (**default).clone();
    mark_synthetic(&mut value);

    let mut scratch = Vec::new();
    resolve_in_declaring_scope(
        &mut value,
        &declared_field.ty,
        declaring_package(owner_id, registry),
        &[],
        registry,
        &mut scratch,
    );
    debug_assert!(
        scratch.is_empty(),
        "field default for `{}` diverged from declaration validation: {scratch:?}",
        declared_field.name,
    );
    let actual = value.resolution.clone();
    if actual.is_resolved() && declared_field.ty.is_resolved() {
        let mismatch = check_compatible_stamping(&mut value, &actual, &declared_field.ty, registry);
        debug_assert!(
            mismatch.is_none(),
            "field default for `{}` diverged from declaration validation: {mismatch:?}",
            declared_field.name,
        );
    }

    Some(FieldInit {
        name: declared_field.name.clone(),
        value,
        span: construction_span.as_synthetic(),
    })
}

fn declaring_package<'a>(owner_id: GlobalRegistryId, registry: &'a GlobalRegistry) -> &'a str {
    let entry: &'a RegistryEntry = registry
        .get(owner_id)
        .expect("construction resolved through this id");
    entry.identifier.package()
}

/// Mark every span in a default-value clone synthetic. Only the
/// shapes the lift-time check allows can appear here; anything else
/// was rejected at the declaration.
fn mark_synthetic(expr: &mut Expr) {
    expr.span = expr.span.as_synthetic();
    match &mut expr.kind {
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                segment.span = segment.span.as_synthetic();
                mark_synthetic(&mut segment.value);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => mark_field_inits_synthetic(fields),
            EnumConstructionData::Tuple(elements) => {
                elements.iter_mut().for_each(mark_synthetic);
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::Group { expr: inner } => mark_synthetic(inner),
        ExprKind::List { elements } => elements.iter_mut().for_each(mark_synthetic),
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                mark_synthetic(key);
                mark_synthetic(value);
            }
        }
        ExprKind::StructConstruction { fields, .. } => mark_field_inits_synthetic(fields),
        ExprKind::Unary { operand, .. } => mark_synthetic(operand),
        _ => {}
    }
}

fn mark_field_inits_synthetic(fields: &mut [FieldInit]) {
    for field in fields {
        field.span = field.span.as_synthetic();
        mark_synthetic(&mut field.value);
    }
}
