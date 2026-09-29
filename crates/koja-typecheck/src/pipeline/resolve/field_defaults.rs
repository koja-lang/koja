//! Field-default resolution: declaration-time validation and
//! construction-time fill.
//!
//! Lift stored each default as an *unresolved* AST on the registry
//! field. This module resolves it in two places:
//!
//! - **Declaration**: the walker trial-resolves every default in the
//!   declaring file's scope, its package and its alias roster with
//!   no locals, against the lifted field type. Unknown names, type
//!   mismatches, and out-of-range literals all diagnose on the
//!   declaring file.
//! - **Construction**: a site that omits a defaulted field gets a
//!   synthesized [`FieldInit`] cloned from the stored default, which
//!   lift already marked synthetic, resolved with the substituted
//!   field type as the expected hint. The declaring file's aliases
//!   come off the owner's registry definition. The declaration owns
//!   the diagnostics, so site resolution uses a scratch vec and
//!   stays quiet when the declaration already failed.
//!
//! Both resolutions go through [`resolve_in_declaring_scope`], the
//! same scope shape (declaring package, declaring file's aliases, no
//! locals), so they cannot diverge.

use koja_ast::ast::{
    AliasDecl, Diagnostic, EnumDecl, EnumVariantData, Expr, FieldInit, Name, StructDecl,
    StructField, name_texts,
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
    let identifier = Identifier::new(env.package, name_texts(&decl.path));
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
            owner_label,
            env,
            diagnostics,
        );
    }
}

/// Trial-resolve one default against its lifted field type in the
/// declaring file's scope. Diagnostics land on the default
/// expression.
fn resolve_declared_default(
    default: &mut Expr,
    field_ty: &ResolvedType,
    field_name: &str,
    owner_label: &str,
    env: &ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut trial = Vec::new();
    resolve_in_declaring_scope(
        default,
        field_ty,
        env.package,
        env.file_aliases,
        env.registry,
        &mut trial,
    );
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
/// declaration trial and the construction-site fill from one
/// function is what keeps declaration-time and site-time resolution
/// identical.
fn resolve_in_declaring_scope(
    expr: &mut Expr,
    expected: &ResolvedType,
    package: &str,
    aliases: &[AliasDecl],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut env = ResolverEnv {
        bound_overlay: None,
        file_aliases: aliases,
        package,
        registry,
    };
    let mut scope = LocalScope::new();
    let mut resolver = env.make_resolver(None, None, &[], &mut scope);
    resolve_expr_with_expected(expr, Some(expected), &mut resolver, diagnostics);
}

/// Synthesize the omitted field's init at a construction site:
/// clone the stored default, whose spans lift already marked
/// synthetic, and re-resolve it against the substituted field type
/// in the declaring file's scope.
///
/// Diagnostics go to a scratch vec on purpose. A default that fails
/// here failed the same way at its declaration, which already
/// reported it, and the walker may reach this site before that
/// declaration. The init still returns, with whatever resolution the
/// trial left on it, so the site does not add a missing-field error
/// on top.
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
        &declared_field.ty,
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
fn declaring_scope<'a>(
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
