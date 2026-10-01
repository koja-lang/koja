//! Post-resolve deprecation warnings. Every use that resolves to a
//! `@deprecated` registry entry warns at the use span. Uses inside
//! the deprecated decl itself, and inside `impl` / `extend` blocks
//! whose target is deprecated, are suppressed so deprecating a type
//! does not flag its own methods.
//!
//! Expression uses read the [`Resolution::Global`] stamps resolve
//! left behind (idents, static receivers) or the [`ResolvedType`]
//! head on construction nodes. Type positions and patterns carry no
//! stamps, so their paths re-resolve through the same
//! [`lookup_type`] the resolver used.

use koja_ast::ast::{
    Annotation, AnnotationKind, Diagnostic, Expr, ExprKind, File, Function, Item, Name, Pattern,
    ProtocolMethod, TypeExpr, TypeParam, name_texts,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_ast::visit::{self, Visitor};

use crate::pipeline::aliases::collect_file_aliases;
use crate::pipeline::collect::nominal_target_path;
use crate::pipeline::lift_signatures::ResolutionScope;
use crate::pipeline::resolve::types::{lookup_type, peel_alias};
use crate::registry::{GlobalKind, GlobalRegistry};

pub(crate) fn check_file(
    file: &File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let aliases = collect_file_aliases(file);
    let scope = ResolutionScope {
        aliases: &aliases,
        package,
        registry,
    };
    let mut walker = Walker {
        diagnostics,
        scope,
        type_params: Vec::new(),
    };
    walker.visit_file(file);
}

/// Whether the decl carries a well-formed `@deprecated` annotation.
/// Its own body and signature never warn about other deprecated
/// items (or itself).
fn is_deprecated(annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| matches!(a.kind(), AnnotationKind::Deprecated { .. }))
}

struct Walker<'a, 'd> {
    diagnostics: &'d mut Vec<Diagnostic>,
    scope: ResolutionScope<'a>,
    /// Generic-param names in scope, so a single-segment type path
    /// naming one is never misread as a global type.
    type_params: Vec<String>,
}

impl Walker<'_, '_> {
    /// Run `walk` and drop the type params it pushed on the way.
    fn scoped(&mut self, walk: impl FnOnce(&mut Self)) {
        let depth = self.type_params.len();
        walk(self);
        self.type_params.truncate(depth);
    }

    /// Whether the item's own uses never warn. A deprecated decl
    /// suppresses itself, and an `impl` / `extend` block is
    /// suppressed when its target is deprecated. Functions decide
    /// for themselves in [`Visitor::visit_function`].
    fn suppressed(&self, item: &Item) -> bool {
        match item {
            Item::Alias(_) | Item::Function(_) => false,
            Item::Builtin(decl) => is_deprecated(&decl.annotations),
            Item::Constant(constant) => is_deprecated(&constant.annotations),
            Item::Enum(decl) => is_deprecated(&decl.annotations),
            Item::Extend(block) => self.target_is_deprecated(&block.target),
            Item::Impl(block) => self.target_is_deprecated(&block.target),
            Item::Protocol(decl) => is_deprecated(&decl.annotations),
            Item::Struct(decl) => is_deprecated(&decl.annotations),
            Item::Test(_) => {
                unreachable!("desugar turns test blocks into functions or drops them")
            }
            Item::TypeAlias(alias) => is_deprecated(&alias.annotations),
        }
    }

    /// Whether an `impl`/`extend` target resolves to a deprecated
    /// entry, using the same path lookup collect keyed the block by.
    fn target_is_deprecated(&self, target: &TypeExpr) -> bool {
        let Some(path) = nominal_target_path(target) else {
            return false;
        };
        matches!(
            lookup_type(&name_texts(path), self.scope),
            Some((_, entry)) if entry.deprecation.is_some()
        )
    }

    /// Warn when a source type path names a deprecated entry.
    /// In-scope generic params shadow globals, so those never warn.
    fn warn_type_path(&mut self, path: &[Name], span: Span) {
        if path.len() == 1 && self.type_params.contains(&path[0].text) {
            return;
        }
        let Some((id, _)) = lookup_type(&name_texts(path), self.scope) else {
            return;
        };
        self.warn_use(id, span);
    }

    /// Warn when a method call lands on a deprecated function entry.
    /// The receiver's own deprecation is warned separately, by the
    /// `Ident` hook for statics or wherever the value was produced
    /// for instances.
    fn warn_deprecated_method(
        &mut self,
        receiver: &Expr,
        method: &Name,
        explicit_arity: usize,
        span: Span,
    ) {
        let static_type_id = match &receiver.kind {
            ExprKind::Ident {
                resolution: Resolution::Global(id),
                ..
            } if self.scope.registry.get(*id).is_some_and(|entry| {
                matches!(
                    entry.kind,
                    GlobalKind::Builtin(_)
                        | GlobalKind::Enum(_)
                        | GlobalKind::Protocol(_)
                        | GlobalKind::Struct(_)
                )
            }) =>
            {
                Some(*id)
            }
            _ => None,
        };
        let type_id = static_type_id.or_else(|| {
            match peel_alias(&receiver.resolution, self.scope.registry) {
                ResolvedType::Named {
                    resolution: Resolution::Global(id),
                    ..
                } => Some(id),
                _ => None,
            }
        });
        let Some(type_id) = type_id else {
            return;
        };
        let Some(type_entry) = self.scope.registry.get(type_id) else {
            return;
        };
        let method_identifier = Identifier::member(
            type_entry.identifier.package(),
            type_entry.identifier.path(),
            method.as_str(),
        );
        let arity = explicit_arity + usize::from(static_type_id.is_none());
        let Some((method_id, _)) = self
            .scope
            .registry
            .lookup_function(&method_identifier, arity)
        else {
            return;
        };
        self.warn_use(method_id, span);
    }

    /// Warn when a construction expression's resolved head names a
    /// deprecated type.
    fn warn_resolution_head(&mut self, resolution: &ResolvedType, span: Span) {
        if let ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } = resolution
        {
            self.warn_use(*id, span);
        }
    }

    /// The `@test` annotation is the pre-0.19 test form. The harness
    /// still runs it, so the warning is the only nudge toward `test`
    /// blocks. Desugared blocks carry no annotation, so they never
    /// reach here with one.
    fn warn_legacy_test(&mut self, function: &Function) {
        let Some(annotation) = function.annotations.iter().find(|a| a.name == "test") else {
            return;
        };
        self.diagnostics.push(Diagnostic::warning_with_hint(
            "`@test` is deprecated. Koja 0.20 removes it.",
            "move the body into a `test \"description\"` block",
            annotation.span,
        ));
    }

    fn warn_use(&mut self, id: GlobalRegistryId, span: Span) {
        let Some(entry) = self.scope.registry.get(id) else {
            return;
        };
        let Some(message) = entry.deprecation.as_ref() else {
            return;
        };
        // The LSP keys its deprecated-tag detection off this message
        // shape (koja-lsp's `is_deprecation_warning`). Keep the two
        // in sync when changing the wording.
        self.diagnostics.push(Diagnostic::warning(
            format!(
                "`{}` is deprecated. {message}",
                entry.identifier.path().join("."),
            ),
            span,
        ));
    }
}

impl<'ast> Visitor<'ast> for Walker<'_, '_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if self.suppressed(item) {
            return;
        }
        self.scoped(|walker| visit::walk_item(walker, item));
    }

    fn visit_function(&mut self, function: &'ast Function) {
        self.warn_legacy_test(function);
        if is_deprecated(&function.annotations) {
            return;
        }
        self.scoped(|walker| visit::walk_function(walker, function));
    }

    fn visit_protocol_method(&mut self, method: &'ast ProtocolMethod) {
        self.scoped(|walker| visit::walk_protocol_method(walker, method));
    }

    /// Bounds are protocol references, so they warn. The name then
    /// shadows any global of the same text until the scope ends.
    fn visit_type_param(&mut self, type_param: &'ast TypeParam) {
        visit::walk_type_param(self, type_param);
        self.type_params.push(type_param.name.text.clone());
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        // Compiler-synthesized subtrees (field-default fills, derived
        // impls) reuse user expressions that already warned at their
        // declaration. Warning again per site would be noise.
        if expr.span.synthetic {
            return;
        }
        match &expr.kind {
            ExprKind::EnumConstruction { .. } | ExprKind::StructConstruction { .. } => {
                self.warn_resolution_head(&expr.resolution, expr.span);
            }
            ExprKind::Ident {
                resolution: Resolution::Global(id),
                ..
            }
            | ExprKind::NamedFunctionReference {
                target: Resolution::Global(id),
                ..
            } => self.warn_use(*id, expr.span),
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => self.warn_deprecated_method(receiver, method, args.len(), expr.span),
            _ => {}
        }
        visit::walk_expr(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &'ast Pattern) {
        match pattern {
            Pattern::EnumStruct {
                type_path, span, ..
            }
            | Pattern::EnumTuple {
                type_path, span, ..
            }
            | Pattern::EnumUnit {
                type_path, span, ..
            }
            | Pattern::Struct {
                type_path, span, ..
            } => self.warn_type_path(type_path, *span),
            _ => {}
        }
        visit::walk_pattern(self, pattern);
    }

    fn visit_type_expr(&mut self, type_expr: &'ast TypeExpr) {
        match type_expr {
            TypeExpr::Generic { path, span, .. } | TypeExpr::Named { path, span, .. } => {
                self.warn_type_path(path, *span);
            }
            _ => {}
        }
        visit::walk_type_expr(self, type_expr);
    }
}
