//! Seal sub-pass: assert every relevant [`Resolution`] /
//! [`koja_ast::identifier::ResolvedType`] annotation is populated.
//! Panics on violation per [`COMPILER-NORTHSTAR.md`]: seal failures
//! are upstream compiler bugs, not user errors.
//!
//! # Module layout
//!
//! - [`statements`]: assignment / compound-assign target checks
//!   plus per-statement recursion into [`expressions::seal_expr`].
//! - [`expressions`]: every [`ExprKind`] arm's resolution invariants
//!   plus the `Call`-callee carve-out.
//! - [`patterns`]: match-pattern shape checks ([`Wildcard`] /
//!   [`Literal`] / [`Binding`] / [`EnumUnit`] / [`EnumTuple`] /
//!   [`EnumStruct`] / [`Or`] / [`Struct`]).
//!
//! Top-level orchestration (`seal_ast` -> `seal_file` ->
//! `seal_function` / `seal_constant`) plus the cross-module
//! helpers ([`seal_no_type_param`], [`seal_panic`]) live here so
//! submodules need only `pub(super)` visibility.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../../design/COMPILER-NORTHSTAR.md
//! [`ExprKind`]: koja_ast::ast::ExprKind
//! [`Wildcard`]: koja_ast::ast::Pattern::Wildcard
//! [`Literal`]: koja_ast::ast::Pattern::Literal
//! [`Binding`]: koja_ast::ast::Pattern::Binding
//! [`EnumUnit`]: koja_ast::ast::Pattern::EnumUnit
//! [`EnumTuple`]: koja_ast::ast::Pattern::EnumTuple
//! [`EnumStruct`]: koja_ast::ast::Pattern::EnumStruct
//! [`Or`]: koja_ast::ast::Pattern::Or
//! [`Struct`]: koja_ast::ast::Pattern::Struct

mod expressions;
mod patterns;
mod statements;

use koja_ast::ast::{Constant, File, Function, ImplMember, Item, TypeExpr};
use koja_ast::identifier::{AnonymousKind, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use expressions::seal_expr;
use statements::seal_statement;

use crate::pipeline::collect::{lookup_owner_path, nominal_target_path};
use crate::program::CheckedProgram;
use crate::registry::{GlobalKind, GlobalRegistry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SealMode {
    Concrete,
    GenericTemplate,
}

impl SealMode {
    fn for_template(generic: bool) -> Self {
        if generic {
            Self::GenericTemplate
        } else {
            Self::Concrete
        }
    }
}

/// Asserts the sealed-AST invariants on `program`. Panics on violation.
///
/// Generic templates permit resolved [`Resolution::TypeParam`] leaves.
/// Concrete bodies reject them. Neither mode permits unresolved
/// annotations.
pub(crate) fn seal_ast(program: &CheckedProgram) {
    seal_registry(&program.registry);
    for pkg in &program.packages {
        for file in &pkg.files {
            seal_file(file, &pkg.package, &program.registry);
        }
    }
}

fn seal_file(file: &File, package: &str, registry: &GlobalRegistry) {
    for item in &file.items {
        match item {
            Item::Function(function) => {
                let mode = SealMode::for_template(!function.type_params.is_empty());
                seal_function(function, mode);
            }
            Item::Struct(decl) => {
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    let generic = owner_generic || !function.type_params.is_empty();
                    seal_function(function, SealMode::for_template(generic));
                }
            }
            Item::Enum(decl) => {
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    let generic = owner_generic || !function.type_params.is_empty();
                    seal_function(function, SealMode::for_template(generic));
                }
            }
            Item::Impl(impl_block) => {
                let target_generic = impl_target_is_generic(&impl_block.target, package, registry);
                for member in &impl_block.members {
                    if let ImplMember::Function(function) = member {
                        let generic = target_generic || !function.type_params.is_empty();
                        seal_function(function, SealMode::for_template(generic));
                    }
                }
            }
            Item::Extend(extend_block) => {
                let target_generic =
                    impl_target_is_generic(&extend_block.target, package, registry);
                for member in &extend_block.members {
                    if let ImplMember::Function(function) = member {
                        let generic = target_generic || !function.type_params.is_empty();
                        seal_function(function, SealMode::for_template(generic));
                    }
                }
            }
            Item::Constant(constant) => {
                seal_constant(constant, package, registry);
            }
            _ => {}
        }
    }
    if let Some(body) = file.body.as_ref() {
        // Script-mode files keep their top-level statements on
        // `file.body`. Downstream passes consume them directly. Seal
        // the same statement-tree invariants function bodies satisfy.
        for stmt in body {
            seal_statement(stmt, SealMode::Concrete);
        }
    }
}

fn seal_registry(registry: &GlobalRegistry) {
    for (_, entry) in registry.iter() {
        match &entry.kind {
            GlobalKind::Constant(Some(_))
            | GlobalKind::Enum(Some(_))
            | GlobalKind::Function(Some(_))
            | GlobalKind::Protocol(Some(_))
            | GlobalKind::Struct(Some(_))
            | GlobalKind::TypeAlias(Some(_)) => {}
            GlobalKind::Constant(None)
            | GlobalKind::Enum(None)
            | GlobalKind::Function(None)
            | GlobalKind::Protocol(None)
            | GlobalKind::Struct(None)
            | GlobalKind::TypeAlias(None) => seal_panic(
                &format!(
                    "registry entry `{}` reached seal as an unstamped {}",
                    entry.identifier,
                    entry.kind.label(),
                ),
                entry.span,
            ),
        }
    }
}

/// True when an `impl` target names a generic struct/enum (e.g.
/// `impl Pair` or `impl Show for List<T>`). Methods on a generic
/// target inherit the type-param scope (struct's slot anchors for
/// inherent impls, the impl entry's free-name anchors for
/// `impl Trait for Type<T>`), so their bodies carry `TypeParam`
/// resolutions and seal must skip them.
fn impl_target_is_generic(target: &TypeExpr, package: &str, registry: &GlobalRegistry) -> bool {
    let Some(path) = nominal_target_path(target) else {
        return false;
    };
    lookup_owner_path(path, package, registry).is_some_and(|(id, _, _)| {
        registry
            .get(id)
            .is_some_and(|entry| !entry.type_params.is_empty())
    })
}

/// Assert lift's constants pass produced a stamped
/// [`crate::registry::ConstantDefinition`], then seal the value
/// expression like any other resolved expression. The body shape is
/// already constrained to literals + struct/enum-of-literals, so the
/// reused [`seal_expr`] walk is sufficient.
fn seal_constant(constant: &Constant, package: &str, registry: &GlobalRegistry) {
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    let Some((_, entry)) = registry.lookup(&identifier) else {
        seal_panic(
            &format!(
                "constant `{identifier}` missing from registry: collect/lift invariant violation",
            ),
            constant.span,
        );
    };
    match &entry.kind {
        GlobalKind::Constant(Some(_)) => {}
        GlobalKind::Constant(None) => seal_panic(
            &format!(
                "constant `{identifier}` reached seal without a stamped definition: \
                 lift_signatures::constants invariant violation",
            ),
            constant.span,
        ),
        other => seal_panic(
            &format!(
                "registry entry for `{identifier}` is `{}`, expected `constant`: \
                 collect/lift invariant violation",
                other.label(),
            ),
            constant.span,
        ),
    }
    seal_expr(&constant.value, SealMode::Concrete);
}

fn seal_function(function: &Function, mode: SealMode) {
    let Some(body) = function.body.as_ref() else {
        return;
    };
    for stmt in body {
        seal_statement(stmt, mode);
    }
}

/// Walk `ty` and assert no `Resolution::TypeParam` leaf escapes into
/// runtime-value position. Concrete `type_args` are fine (and
/// expected for monomorphizable construction sites), so this only
/// rejects the `TypeParam` head.
pub(super) fn seal_no_type_param(ty: &ResolvedType, span: Span) {
    match ty {
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => seal_panic(
            &format!(
                "ResolvedType leaf carries TypeParam {{ owner: {owner}, index: {index} }} \
                 outside a generic-decl body",
            ),
            span,
        ),
        ResolvedType::Named { type_args, .. } => {
            for arg in type_args {
                seal_no_type_param(arg, span);
            }
        }
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            for param in params {
                seal_no_type_param(param, span);
            }
            seal_no_type_param(ret, span);
        }
        ResolvedType::Union(members) => {
            for member in members {
                seal_no_type_param(member, span);
            }
        }
        ResolvedType::Unresolved => {}
    }
}

pub(super) fn seal_resolved_type(ty: &ResolvedType, mode: SealMode, span: Span) {
    if !ty.is_resolved() {
        seal_panic("type annotation missing resolution", span);
    }
    if mode == SealMode::Concrete {
        seal_no_type_param(ty, span);
    }
}

pub(super) fn seal_panic(message: &str, span: Span) -> ! {
    panic!(
        "typecheck seal violation: {message} at line {}, column {}",
        span.start.line, span.start.column
    );
}

#[cfg(test)]
mod tests {
    use koja_ast::ast::{Expr, ExprKind, Literal};
    use koja_ast::identifier::Identifier;
    use koja_ast::span::Span;

    use crate::registry::{GlobalRegistry, VisibilityScope};

    use super::expressions::seal_expr;
    use super::{SealMode, seal_registry};

    #[test]
    #[should_panic(expected = "reached seal as an unstamped function")]
    fn registry_rejects_unstamped_entry() {
        let mut registry = GlobalRegistry::new();
        registry.insert_function(
            Identifier::new("Test", vec!["pending".to_string()]),
            Span::default(),
            Vec::new(),
            VisibilityScope::Public,
        );

        seal_registry(&registry);
    }

    #[test]
    #[should_panic(expected = "type annotation missing resolution")]
    fn template_rejects_unresolved_expression() {
        let expression = Expr::new(
            ExprKind::Literal {
                value: Literal::Unit,
            },
            Span::default(),
        );

        seal_expr(&expression, SealMode::GenericTemplate);
    }
}
