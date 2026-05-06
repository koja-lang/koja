//! Collect sub-pass: register a canonical [`Identifier`] for every
//! globally-named decl. Pure registration — signature resolution lives
//! in [`super::lift_signatures`].
//!
//! Path encoding follows the [`Identifier`] convention: top-level
//! functions register at `path = ["name"]`; static methods on `Point`
//! (declared inline in the struct body or in an `impl` block) register
//! at `path = ["Point", "name"]`. Both forms produce the same registry
//! entry so call resolution can't tell them apart.
//!
//! The walk is split into two passes: pass 1 registers `Item::Function`
//! and `Item::Struct` (including each `decl.functions[i]`); pass 2
//! registers `Item::Impl`. The split makes `impl Foo` order-independent
//! relative to `struct Foo` — pass 2 always sees a fully-populated
//! registry of declared types.
//!
//! Today the supported surface is: top-level functions, structs, and
//! static methods on structs (no `self`). Instance methods, generics,
//! `priv`, default field values, trait impls, and impl-block type
//! aliases all surface as feature-gap diagnostics here so later passes
//! never see those shapes.

use expo_ast::ast::{
    Annotation, Diagnostic, File, Function, ImplBlock, ImplMember, Item, Param, StructDecl,
    StructField, TypeExpr,
};
use expo_ast::identifier::Identifier;
use expo_ast::span::Span;

use crate::registry::{GlobalKind, GlobalRegistry, InsertOutcome};
use expo_ast::labels::{item_label, item_span};

pub(crate) fn collect_file(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Pass 1: top-level functions, structs, and inline static methods.
    // Every type name a later impl block could target is registered
    // before pass 2 starts looking up impl targets.
    for item in &file.items {
        match item {
            Item::Function(function) => {
                let identifier = Identifier::new(package, vec![function.name.clone()]);
                register_function_with_identifier(
                    function,
                    identifier,
                    SelfContext::RejectSelf,
                    registry,
                    diagnostics,
                );
            }
            Item::Struct(decl) => {
                register_struct(decl, package, registry, diagnostics);
            }
            Item::Impl(_) => {}
            // Other Item variants land as alpha grows. Reject them
            // explicitly so unsupported shapes diagnose instead of
            // round-tripping silently.
            Item::Alias(_)
            | Item::Constant(_)
            | Item::Enum(_)
            | Item::Protocol(_)
            | Item::TypeAlias(_) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck does not yet support `{}` items",
                        item_label(item)
                    ),
                    item_span(item),
                ));
            }
        }
    }

    // Pass 2: impl blocks, now that every struct declared anywhere in
    // the file has a registry entry.
    for item in &file.items {
        if let Item::Impl(impl_block) = item {
            register_impl(impl_block, package, registry, diagnostics);
        }
    }
}

/// Whether the registration site (top-level vs inside a `struct` /
/// `impl` body) accepts a `self` receiver. Lift_signatures carries a
/// richer struct-aware variant; collect only needs to know "is `self`
/// allowed here?" so a flat enum suffices.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelfContext {
    AllowSelf,
    RejectSelf,
}

/// Register a function under `identifier`. Shared by all three callers
/// (top-level fns, inline static or instance methods, impl-block
/// static or instance methods) so the duplicate-detection /
/// collision-message / `self`-context paths stay in one place.
fn register_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    self_context: SelfContext,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if reject_self_param(function, &identifier, self_context, diagnostics) {
        return;
    }
    match registry.insert_function(identifier, function.span) {
        InsertOutcome::Fresh(_) => {}
        InsertOutcome::Collision { existing } => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`{}` is already defined", existing.identifier),
                format!(
                    "previous {} definition is at line {}",
                    existing.kind.label(),
                    existing.span.start.line
                ),
                function.span,
            ));
        }
    }
}

/// Reject a `self` receiver only when registration is happening
/// outside a struct/impl context (top-level functions); inside a
/// struct or `impl Type` block, `self` is the receiver for an
/// instance method and lift_signatures will lift it to a real
/// parameter typed by the enclosing struct.
fn reject_self_param(
    function: &Function,
    identifier: &Identifier,
    self_context: SelfContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if self_context == SelfContext::AllowSelf {
        return false;
    }
    let Some(self_span) = function.params.iter().find_map(|param| match param {
        Param::Regular { .. } => None,
        Param::Self_ { span, .. } => Some(*span),
    }) else {
        return false;
    };
    diagnostics.push(Diagnostic::error(
        format!(
            "`self` receiver is only valid inside `struct` or `impl` blocks (on `{identifier}`)"
        ),
        self_span,
    ));
    true
}

fn register_struct(
    decl: &StructDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_struct_feature_gaps(decl, diagnostics);
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    match registry.insert_struct(identifier, decl.span) {
        InsertOutcome::Fresh(_) => {}
        InsertOutcome::Collision { existing } => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`{}` is already defined", existing.identifier),
                format!(
                    "previous {} definition is at line {}",
                    existing.kind.label(),
                    existing.span.start.line
                ),
                decl.span,
            ));
            // Still register inline methods even on collision: the
            // duplicate decl is itself diagnosed; methods declared
            // under the duplicate would otherwise dangle.
        }
    }
    for function in &decl.functions {
        let method_identifier =
            Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
        register_function_with_identifier(
            function,
            method_identifier,
            SelfContext::AllowSelf,
            registry,
            diagnostics,
        );
    }
}

/// Register every static method declared in an `impl Type ... end`
/// block under `(package, [type_name, fn_name])`. Diagnoses every
/// out-of-scope shape (trait impls, generic targets, `TypeAlias`
/// members, unknown / non-struct targets) and skips registration when
/// any of them apply — the methods inside an unsupported impl never
/// reach lift / resolve / lower.
fn register_impl(
    impl_block: &ImplBlock,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_impl_member_feature_gaps(impl_block, diagnostics);
    if let Some(span) = impl_block.trait_expr.as_ref().map(type_expr_span) {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support `impl Trait for Type`".to_string(),
            span,
        ));
        return;
    }
    let Some(target_name) = simple_named_target(&impl_block.target) else {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support generic impl targets".to_string(),
            type_expr_span(&impl_block.target),
        ));
        return;
    };
    let target_identifier = Identifier::new(package, vec![target_name.to_string()]);
    let Some((_, entry)) = registry.lookup(&target_identifier) else {
        diagnostics.push(Diagnostic::error(
            format!("alpha typecheck cannot extend unknown type `{target_name}`"),
            type_expr_span(&impl_block.target),
        ));
        return;
    };
    if !matches!(entry.kind, GlobalKind::Struct(_)) {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only supports `impl` on structs (`{target_name}` is a {})",
                entry.kind.label(),
            ),
            type_expr_span(&impl_block.target),
        ));
        return;
    }
    for member in &impl_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let method_identifier = Identifier::new(
            package,
            vec![target_name.to_string(), function.name.clone()],
        );
        register_function_with_identifier(
            function,
            method_identifier,
            SelfContext::AllowSelf,
            registry,
            diagnostics,
        );
    }
}

/// Pull the bare type name out of `impl Foo` / `impl Foo<...>` /
/// `impl Foo.Bar` shapes, returning `None` for anything we don't yet
/// support (generics, dotted paths, function types, unions, etc.).
fn simple_named_target(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].as_str()),
        _ => None,
    }
}

fn type_expr_span(type_expr: &TypeExpr) -> Span {
    match type_expr {
        TypeExpr::Function { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Named { span, .. }
        | TypeExpr::Self_ { span }
        | TypeExpr::Union { span, .. }
        | TypeExpr::Unit { span } => *span,
    }
}

/// Diagnose every feature gap on a struct decl up front so collect
/// is the single seam covering them. The struct still registers (so
/// downstream `resolve` finds it for diagnostic-friendly error
/// messages); lift_signatures stamps a permissive "best effort"
/// definition in the presence of these gaps so the surrounding
/// program shape stays accurate.
fn diagnose_struct_feature_gaps(decl: &StructDecl, diagnostics: &mut Vec<Diagnostic>) {
    if !decl.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support generic structs (`{}` has type parameters)",
                decl.name,
            ),
            decl.span,
        ));
    }
    diagnose_struct_annotations(&decl.name, &decl.annotations, diagnostics);
    for field in &decl.fields {
        diagnose_struct_field_gaps(&decl.name, field, diagnostics);
    }
}

fn diagnose_struct_annotations(
    struct_name: &str,
    annotations: &[Annotation],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for annotation in annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on struct items \
                 (`@{}` on `{struct_name}`)",
                annotation.name,
            ),
            annotation.span,
        ));
    }
}

fn diagnose_struct_field_gaps(
    struct_name: &str,
    field: &StructField,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if field.default.is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support default field values \
                 (on `{struct_name}.{}`)",
                field.name,
            ),
            field.span,
        ));
    }
}

/// Diagnose the only impl-block member shape we don't yet support:
/// `type Alias = ...`. `Function` members flow through normal
/// registration in [`register_impl`]; this pass surfaces a diagnostic
/// for every other shape so the user sees one error per offending
/// member rather than a single block-level message.
fn diagnose_impl_member_feature_gaps(impl_block: &ImplBlock, diagnostics: &mut Vec<Diagnostic>) {
    for member in &impl_block.members {
        if let ImplMember::TypeAlias(alias) = member {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support `type` aliases inside `impl` blocks"
                    .to_string(),
                alias.span,
            ));
        }
    }
}
