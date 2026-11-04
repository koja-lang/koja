//! Field-default resolution, at the declaration and at each
//! construction site that omits the field.
//!
//! Lift stored each default as an unresolved, synthetic-spanned clone
//! on the registry field. This module resolves it in two places:
//!
//! - At the declaration, the walker trial-resolves every default
//!   against the lifted field type. Unknown names, type mismatches,
//!   and out-of-range literals diagnose here.
//! - At a construction site that omits the field, the site gets a
//!   [`FieldInit`] cloned from the stored default and resolved against
//!   the substituted field type. See [`synthesize_default_init`] for
//!   why the site reports nothing.
//!
//! Both go through [`resolve_in_declaring_scope`], which resolves in
//! the declaring package with the declaring file's aliases and no
//! locals. One scope shape at both points is what keeps them from
//! diverging.
//!
//! A default is part of its owner's header, so the declaration pass
//! also runs [`crate::pipeline::visibility::check_value_leaks`] on it.

use koja_ast::ast::{
    AliasDecl, Diagnostic, EnumDecl, EnumVariantData, Expr, FieldInit, Name, StructDecl,
    StructField, name_texts,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};
use koja_ast::span::Span;

use crate::pipeline::visibility::check_value_leaks;
use crate::registry::{
    GlobalKind, GlobalRegistry, RegistryEntry, ResolvedStructField, ResolvedVariantData,
};

use super::coercion::{check_compatible_stamping, mismatch_message};
use super::ctx::ResolverEnv;
use super::resolve_in_declaring_scope;

/// The struct or enum a default belongs to. `label` names it in
/// diagnostics, `Pkg.Config` or `Pkg.Shape.Rect` for a variant.
struct DefaultOwner<'a> {
    entry: &'a RegistryEntry,
    label: String,
}

/// Trial-resolve every field default on a struct decl. Called by the
/// walker while it visits the declaring file.
pub(super) fn resolve_struct_defaults(
    decl: &mut StructDecl,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(env.package, name_texts(&decl.path));
    let Some((_, entry)) = env.registry.lookup(&identifier) else {
        return;
    };
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        return;
    };
    let owner = DefaultOwner {
        entry,
        label: entry.identifier.to_string(),
    };
    resolve_field_defaults(
        &mut decl.fields,
        &definition.fields,
        &owner,
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
    let identifier = Identifier::new(env.package, name_texts(&decl.path));
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
        let Some((_, lifted)) = definition.lookup_variant(variant.name.as_str()) else {
            continue;
        };
        let ResolvedVariantData::Struct(declared) = &lifted.data else {
            continue;
        };
        let owner = DefaultOwner {
            entry,
            label: format!("{}.{}", entry.identifier, variant.name),
        };
        resolve_field_defaults(fields, declared, &owner, env, diagnostics);
    }
}

fn resolve_field_defaults(
    fields: &mut [StructField],
    declared: &[ResolvedStructField],
    owner: &DefaultOwner<'_>,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in fields.iter_mut() {
        let Some(default) = field.default.as_mut() else {
            continue;
        };
        let Some(lifted) = declared.iter().find(|f| f.name == field.name.text) else {
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
            field.name.as_str(),
            owner,
            env,
            diagnostics,
        );
    }
}

/// Trial-resolve one default against its lifted field type in the
/// declaring file's scope, then check it for leaks. Diagnostics land
/// on the default expression.
fn resolve_declared_default(
    default: &mut Expr,
    field_ty: &ResolvedType,
    field_name: &str,
    owner: &DefaultOwner<'_>,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut trial = Vec::new();
    resolve_in_declaring_scope(
        default,
        Some(field_ty),
        env.package,
        env.file_aliases,
        env.registry,
        &mut trial,
    );
    if !trial.is_empty() {
        diagnostics.append(&mut trial);
        return;
    }

    let subject = format!("default for field `{field_name}` of `{}`", owner.label);
    check_value_leaks(default, &subject, owner.entry, env.registry, diagnostics);

    let actual = default.resolution.clone();
    if !actual.is_resolved() || !field_ty.is_resolved() {
        return;
    }
    if let Some(mismatch) = check_compatible_stamping(default, &actual, field_ty, env.registry) {
        diagnostics.push(Diagnostic::error(
            mismatch_message(&subject, &mismatch, field_ty, &actual, env.registry),
            default.span,
        ));
    }
}

/// Synthesize the omitted field's init at a construction site. Clone
/// the stored default and resolve it against the substituted field
/// type in the declaring file's scope.
///
/// Diagnostics go to a scratch vec on purpose. A default that fails
/// here failed the same way at its declaration, which reports it, and
/// the walker may reach this site first. The init still returns with
/// whatever resolution the trial left, so the site adds no
/// missing-field error on top.
pub(super) fn synthesize_default_init(
    declared_field: &ResolvedStructField,
    owner_id: GlobalRegistryId,
    construction_span: Span,
    registry: &GlobalRegistry,
) -> Option<FieldInit> {
    let default = declared_field.default.as_ref()?;
    let mut value = (**default).clone();

    let (package, aliases) = declaring_scope(owner_id, registry);
    let mut scratch = Vec::new();
    resolve_in_declaring_scope(
        &mut value,
        Some(&declared_field.ty),
        package,
        aliases,
        registry,
        &mut scratch,
    );
    let actual = value.resolution.clone();
    if scratch.is_empty() && actual.is_resolved() && declared_field.ty.is_resolved() {
        let _ = check_compatible_stamping(&mut value, &actual, &declared_field.ty, registry);
    }

    let span = construction_span.as_synthetic();
    Some(FieldInit {
        name: Name::new(declared_field.name.clone(), span),
        value,
        span,
    })
}

/// The package and alias roster of the file that declared `owner_id`,
/// a struct or an enum whose struct variant is under construction.
pub(crate) fn declaring_scope<'a>(
    owner_id: GlobalRegistryId,
    registry: &'a GlobalRegistry,
) -> (&'a str, &'a [AliasDecl]) {
    let entry: &'a RegistryEntry = registry
        .get(owner_id)
        .expect("construction resolved through this id");
    let aliases: &'a [AliasDecl] = match &entry.kind {
        GlobalKind::Struct(Some(definition)) => &definition.aliases,
        GlobalKind::Enum(Some(definition)) => &definition.aliases,
        _ => &[],
    };
    (entry.identifier.package(), aliases)
}
