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

use koja_ast::ast::{
    Constant, EnumVariantData, File, Function, ImplMember, Item, Name, Param, ProtocolMethod,
    StructField, TypeExpr, TypeParam, name_texts, path_text,
};
use koja_ast::identifier::{AnonymousKind, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use expressions::seal_expr;
use statements::seal_statement;

use crate::pipeline::collect::nominal_target_path;
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
                assert!(
                    decl.nested.is_empty(),
                    "sealed struct `{}` still carries nested declarations",
                    name_texts(&decl.path).join(".")
                );
                seal_type_params(&decl.type_params);
                seal_type_exprs(&decl.conformances);
                seal_struct_fields(&decl.fields);
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    let generic = owner_generic || !function.type_params.is_empty();
                    seal_function(function, SealMode::for_template(generic));
                }
            }
            Item::Builtin(decl) => {
                seal_type_params(&decl.type_params);
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    let generic = owner_generic || !function.type_params.is_empty();
                    seal_function(function, SealMode::for_template(generic));
                }
            }
            Item::Enum(decl) => {
                assert!(
                    decl.nested.is_empty(),
                    "sealed enum `{}` still carries nested declarations",
                    name_texts(&decl.path).join(".")
                );
                seal_type_params(&decl.type_params);
                seal_type_exprs(&decl.conformances);
                for variant in &decl.variants {
                    match &variant.data {
                        EnumVariantData::Unit => {}
                        EnumVariantData::Tuple(types) => seal_type_exprs(types),
                        EnumVariantData::Struct(fields) => seal_struct_fields(fields),
                    }
                }
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    let generic = owner_generic || !function.type_params.is_empty();
                    seal_function(function, SealMode::for_template(generic));
                }
            }
            Item::Impl(impl_block) => {
                seal_type_expr(&impl_block.target);
                seal_type_params(&impl_block.target_bounds);
                seal_type_expr(&impl_block.trait_expr);
                let target_generic = impl_target_is_generic(&impl_block.target, package, registry);
                for member in &impl_block.members {
                    if let ImplMember::Function(function) = member {
                        let generic = target_generic || !function.type_params.is_empty();
                        seal_function(function, SealMode::for_template(generic));
                    }
                }
            }
            Item::Extend(extend_block) => {
                seal_type_expr(&extend_block.target);
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
            Item::Protocol(decl) => {
                seal_type_params(&decl.type_params);
                for method in &decl.methods {
                    seal_protocol_method_signature(method);
                }
            }
            Item::TypeAlias(alias) => seal_type_expr(&alias.type_expr),
            Item::Alias(_) | Item::Test(_) => {}
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
            GlobalKind::Builtin(_)
            | GlobalKind::Constant(Some(_))
            | GlobalKind::Enum(Some(_))
            | GlobalKind::Protocol(Some(_))
            | GlobalKind::Struct(Some(_))
            | GlobalKind::TypeAlias(Some(_)) => {}
            GlobalKind::Function(definition) if definition.signature.is_some() => {}
            GlobalKind::Constant(None)
            | GlobalKind::Enum(None)
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
            GlobalKind::Function(_) => seal_panic(
                &format!(
                    "registry entry `{}` reached seal as an unstamped function",
                    entry.identifier,
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
    registry
        .lookup_owner_path(path, package)
        .is_some_and(|(id, _, _)| {
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
    let identifier = Identifier::new(package, name_texts(&constant.path));
    let Some((_, entry)) = registry.lookup(&identifier) else {
        seal_panic(
            &format!(
                "constant `{identifier}` missing from registry. This is a collect/lift invariant violation",
            ),
            constant.span,
        );
    };
    match &entry.kind {
        GlobalKind::Constant(Some(_)) => {}
        GlobalKind::Constant(None) => seal_panic(
            &format!(
                "constant `{identifier}` reached seal without a stamped definition. \
                 This is a lift_signatures::constants invariant violation",
            ),
            constant.span,
        ),
        other => seal_panic(
            &format!(
                "registry entry for `{identifier}` is `{}`, expected `constant`. \
                 This is a collect/lift invariant violation",
                other.label(),
            ),
            constant.span,
        ),
    }
    if let Some(annotation) = &constant.type_annotation {
        seal_type_expr(annotation);
    }
    seal_expr(&constant.value, SealMode::Concrete);
}

fn seal_function(function: &Function, mode: SealMode) {
    seal_type_params(&function.type_params);
    seal_params(&function.params);
    seal_optional_type_expr(function.return_type.as_ref());
    seal_optional_type_expr(function.error_type.as_ref());
    let Some(body) = function.body.as_ref() else {
        return;
    };
    for stmt in body {
        seal_statement(stmt, mode);
    }
}

/// Protocol method bodies are default implementations. Lift clones
/// them into each conforming type and resolves the clones, so only
/// the signature is sealed here.
fn seal_protocol_method_signature(method: &ProtocolMethod) {
    seal_type_params(&method.type_params);
    seal_params(&method.params);
    seal_optional_type_expr(method.return_type.as_ref());
    seal_optional_type_expr(method.error_type.as_ref());
}

fn seal_params(params: &[Param]) {
    for param in params {
        if let Param::Regular { type_expr, .. } = param {
            seal_type_expr(type_expr);
        }
    }
}

fn seal_type_params(type_params: &[TypeParam]) {
    for param in type_params {
        seal_type_exprs(&param.bounds);
    }
}

fn seal_struct_fields(fields: &[StructField]) {
    for field in fields {
        seal_type_expr(&field.type_expr);
    }
}

fn seal_type_exprs(types: &[TypeExpr]) {
    for ty in types {
        seal_type_expr(ty);
    }
}

pub(super) fn seal_optional_type_expr(ty: Option<&TypeExpr>) {
    if let Some(ty) = ty {
        seal_type_expr(ty);
    }
}

/// Assert every named type path under `ty` carries a stamp. `Self`
/// and `()` name nothing and carry none.
pub(super) fn seal_type_expr(ty: &TypeExpr) {
    match ty {
        TypeExpr::Named {
            path,
            resolution,
            span,
        } => seal_type_path(path, *resolution, *span),
        TypeExpr::Generic {
            path,
            args,
            resolution,
            span,
        } => {
            seal_type_path(path, *resolution, *span);
            seal_type_exprs(args);
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            seal_type_exprs(params);
            seal_type_expr(return_type);
        }
        TypeExpr::Tuple { elements, .. } => seal_type_exprs(elements),
        TypeExpr::Union { types, .. } => seal_type_exprs(types),
        TypeExpr::Self_ { .. } | TypeExpr::Unit { .. } => {}
    }
}

pub(super) fn seal_type_path(path: &[Name], resolution: Resolution, span: Span) {
    if !resolution.is_resolved() {
        seal_panic(
            &format!("type path `{}` missing resolution", path_text(path)),
            span,
        );
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
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            for element in elements {
                seal_no_type_param(element, span);
            }
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
        "typecheck seal violation. {message} at line {}, column {}",
        span.start.line, span.start.column
    );
}

#[cfg(test)]
mod tests {
    use koja_ast::ast::{Expr, ExprKind, Function, Literal, Name, TypeExpr, Visibility};
    use koja_ast::identifier::{Identifier, Resolution};
    use koja_ast::span::Span;

    use crate::registry::{FunctionOrigin, GlobalRegistry, VisibilityScope};

    use super::expressions::seal_expr;
    use super::{SealMode, seal_registry, seal_type_expr};

    #[test]
    #[should_panic(expected = "reached seal as an unstamped function")]
    fn registry_rejects_unstamped_entry() {
        let pending = Function {
            annotations: Vec::new(),
            origin: FunctionOrigin::Explicit,
            visibility: Visibility::Public,
            name: Name::new("pending", Span::default()),
            type_params: Vec::new(),
            params: Vec::new(),
            return_type: None,
            error_type: None,
            body: None,
            span: Span::default(),
        };
        let mut registry = GlobalRegistry::new();
        registry.insert_function(
            Identifier::single("Test", "pending"),
            &pending,
            VisibilityScope::Public,
        );

        seal_registry(&registry);
    }

    #[test]
    #[should_panic(expected = "type path `Int` missing resolution")]
    fn rejects_unstamped_type_path_argument() {
        let span = Span::default();
        let registry = GlobalRegistry::with_stdlib_stubs();
        let (list, _) = registry
            .lookup(&Identifier::single("Global", "Int"))
            .expect("stub registry has Int");
        // The head is stamped, so the walk has to descend into the
        // argument to find the miss.
        let ty = TypeExpr::Generic {
            path: vec![Name::new("List", span)],
            args: vec![TypeExpr::named(vec![Name::new("Int", span)], span)],
            resolution: Resolution::Global(list),
            span,
        };
        seal_type_expr(&ty);
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
