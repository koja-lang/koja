//! Type-expression resolution + small label/span helpers shared by
//! every other submodule under `lift_signatures/`.

use expo_ast::ast::{Diagnostic, TypeExpr};
use expo_ast::identifier::{
    GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use expo_ast::span::Span;

use crate::registry::{Dispatch, GlobalRegistry};

/// Stack of generic-decl owners visible at this resolution site.
/// Innermost first (e.g. `[fn_id, struct_id]` for an inline method
/// on a generic struct). [`Self::lookup`] walks the stack and yields
/// the first `(owner, index)` whose entry registers a matching param
/// name. Names live on the [`GlobalRegistry`] entry — the scope is
/// just the chain.
///
/// Empty scope (`TypeParamScope::default()`) is the right value
/// outside any generic-decl body; lookups against it always return
/// `None` so resolve falls through to global lookup.
#[derive(Clone, Copy, Default)]
pub(crate) struct TypeParamScope<'a> {
    owners: &'a [GlobalRegistryId],
}

impl<'a> TypeParamScope<'a> {
    pub(crate) fn new(owners: &'a [GlobalRegistryId]) -> Self {
        Self { owners }
    }

    pub(crate) fn lookup(
        &self,
        name: &str,
        registry: &GlobalRegistry,
    ) -> Option<(GlobalRegistryId, TypeParamIndex)> {
        for &owner in self.owners {
            let names = registry.type_params(owner)?;
            if let Some(pos) = names.iter().position(|n| n == name) {
                return Some((owner, TypeParamIndex::new(pos as u32)));
            }
        }
        None
    }
}

/// Resolve a [`TypeExpr`] against the registry. Single-segment
/// `TypeExpr::Named` matching the surrounding scope resolves to
/// [`Resolution::TypeParam`]; otherwise it resolves to a preloaded
/// `Global.<name>` stub or a user struct/enum. `TypeExpr::Generic`
/// recurses into its args. `scope` is empty outside generic-decl
/// bodies (see [`TypeParamScope::default`]).
pub(crate) fn resolve_type_expr(
    type_expr: &TypeExpr,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match type_expr {
        TypeExpr::Function { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support function-typed annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Generic { path, args, span } => {
            resolve_generic(path, args, *span, scope, package, registry, diagnostics)
        }
        TypeExpr::Named { path, span } => {
            resolve_named(path, *span, scope, package, registry, diagnostics)
        }
        TypeExpr::Self_ { span } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support `Self` type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Union { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support union type annotations".to_string(),
                *span,
            ));
            ResolvedType::unresolved()
        }
        TypeExpr::Unit { .. } => registry.primitive("Unit"),
    }
}

/// Resolve `Path<args...>`. Path resolution mirrors [`resolve_named`]
/// for the head; type args lower recursively through the same scope.
/// A type param shadows a global of the same name; `T<args>` is an
/// error because type params are arity-0.
fn resolve_generic(
    path: &[String],
    args: &[TypeExpr],
    span: Span,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() == 1 && scope.lookup(&path[0], registry).is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck: type parameter `{}` cannot take type arguments",
                path[0],
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    if path.len() != 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support dotted type names (`{}`)",
                path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let name = &path[0];
    let local = Identifier::new(package, vec![name.clone()]);
    let head = if let Some((id, _)) = registry.lookup(&local) {
        Resolution::Global(id)
    } else {
        let candidate = Identifier::new("Global", vec![name.clone()]);
        let Some((id, entry)) = registry.lookup(&candidate) else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not recognize the type name `{name}` (no \
                     same-package struct/enum or `Global.*` type registered)",
                ),
                span,
            ));
            return ResolvedType::unresolved();
        };
        if !entry.identifier.is_in_global() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck only recognizes `Global.*` primitive type names; \
                     got `{name}`",
                ),
                span,
            ));
            return ResolvedType::unresolved();
        }
        Resolution::Global(id)
    };
    let resolved_args = args
        .iter()
        .map(|arg| resolve_type_expr(arg, scope, package, registry, diagnostics))
        .collect();
    ResolvedType {
        resolution: head,
        type_args: resolved_args,
    }
}

fn resolve_named(
    path: &[String],
    span: Span,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if path.len() != 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support dotted type names (`{}`)",
                path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let name = &path[0];
    if let Some((owner, index)) = scope.lookup(name, registry) {
        return ResolvedType::leaf(Resolution::TypeParam { owner, index });
    }
    // User-defined structs in the current package shadow stdlib
    // primitives by binding lookup order. The collect sub-pass has
    // already registered every user struct, so a same-package struct
    // entry takes precedence over a `Global.<name>` primitive.
    let local = Identifier::new(package, vec![name.clone()]);
    if let Some((id, _)) = registry.lookup(&local) {
        return ResolvedType::leaf(Resolution::Global(id));
    }
    let candidate = Identifier::new("Global", vec![name.clone()]);
    let Some((id, entry)) = registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the type name `{name}` (no \
                 same-package struct or `Global.*` primitive registered)",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    if !entry.identifier.is_in_global() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only recognizes `Global.*` primitive type names; \
                 got `{name}`",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    ResolvedType::leaf(Resolution::Global(id))
}

pub(super) fn type_expr_span(type_expr: &TypeExpr) -> Span {
    match type_expr {
        TypeExpr::Function { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Named { span, .. }
        | TypeExpr::Self_ { span }
        | TypeExpr::Union { span, .. }
        | TypeExpr::Unit { span } => *span,
    }
}

pub(super) fn impl_target_name(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].as_str()),
        _ => None,
    }
}

pub(super) fn dispatch_label(dispatch: Dispatch) -> &'static str {
    match dispatch {
        Dispatch::Instance => "instance method (with `self`)",
        Dispatch::Static => "static method (no `self`)",
    }
}

pub(super) fn render_resolved(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty.resolution {
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => "<unknown>".to_string(),
        },
        Resolution::Local(_) => "<local>".to_string(),
        Resolution::TypeParam { owner, index } => registry
            .type_param_name(owner, index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        Resolution::Unresolved => "<unresolved>".to_string(),
    }
}
