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
    Annotation, Diagnostic, EnumDecl, EnumVariant, EnumVariantData, File, Function, ImplBlock,
    ImplMember, Item, Param, ProtocolDecl, ProtocolMethod, StructDecl, StructField, TypeExpr,
    TypeParam,
};
use expo_ast::identifier::Identifier;
use expo_ast::labels::{item_label, item_span};
use expo_ast::span::Span;

use crate::registry::{GlobalKind, GlobalRegistry, InsertOutcome};

pub(crate) fn collect_file(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Pass 1: top-level functions, structs, enums, protocols, and
    // inline static/instance methods. Every type / protocol name a
    // later impl block could target is registered before pass 2
    // starts looking up impl targets.
    for item in &file.items {
        match item {
            Item::Enum(decl) => {
                register_enum(decl, package, registry, diagnostics);
            }
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
            Item::Protocol(decl) => {
                register_protocol(decl, package, registry, diagnostics);
            }
            Item::Struct(decl) => {
                register_struct(decl, package, registry, diagnostics);
            }
            Item::Impl(_) => {}
            // Other Item variants land as alpha grows. Reject them
            // explicitly so unsupported shapes diagnose instead of
            // round-tripping silently.
            Item::Alias(_) | Item::Constant(_) | Item::TypeAlias(_) => {
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

/// Register an enum decl + every inline method on it, and surface
/// every feature-gap diagnostic up front. Mirrors [`register_struct`]:
/// the decl always registers (even on collision or in the presence
/// of feature gaps) so downstream resolve sees a populated registry
/// for diagnostic-friendly error messages.
fn register_enum(
    decl: &EnumDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_enum_feature_gaps(decl, diagnostics);
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    match registry.insert_enum(identifier, decl.span) {
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

/// Register every method declared in an `impl Type ... end` block
/// (inherent or `impl Protocol for Type`) under
/// `(package, [type_name, fn_name])`. Diagnoses out-of-scope shapes
/// (generic targets, generic trait_expr, `TypeAlias` members,
/// unknown / non-struct targets) and skips registration when any of
/// them apply — methods inside an unsupported impl never reach
/// lift / resolve / lower.
///
/// The `trait_expr` itself is only validated for *shape* here (no
/// generics, simple named path); it isn't dereferenced. Protocol
/// conformance checking lives in lift_signatures, after both the
/// protocol and the impl methods have lifted signatures to compare.
fn register_impl(
    impl_block: &ImplBlock,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_impl_member_feature_gaps(impl_block, diagnostics);
    if let Some(trait_expr) = &impl_block.trait_expr
        && simple_named_target(trait_expr).is_none()
    {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support generic `impl Trait for Type`".to_string(),
            type_expr_span(trait_expr),
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
    if !matches!(entry.kind, GlobalKind::Enum(_) | GlobalKind::Struct(_)) {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only supports `impl` on structs and enums \
                 (`{target_name}` is a {})",
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

/// Register a protocol decl. Diagnoses every out-of-scope shape
/// (generics on the decl or its methods, annotations, `Self` in
/// non-self position) up front so later passes never see them.
fn register_protocol(
    decl: &ProtocolDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_protocol_feature_gaps(decl, diagnostics);
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    if let InsertOutcome::Collision { existing } = registry.insert_protocol(identifier, decl.span) {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("`{}` is already defined", existing.identifier),
            format!(
                "previous {} definition is at line {}",
                existing.kind.label(),
                existing.span.start.line
            ),
            decl.span,
        ));
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
    diagnose_type_param_bounds(&decl.name, &decl.type_params, diagnostics);
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

/// Diagnose every feature gap on an enum decl up front so collect is
/// the single seam covering them. Mirrors [`diagnose_struct_feature_gaps`]
/// — the decl still registers in the presence of any gap so resolve
/// sees a populated registry.
fn diagnose_enum_feature_gaps(decl: &EnumDecl, diagnostics: &mut Vec<Diagnostic>) {
    diagnose_type_param_bounds(&decl.name, &decl.type_params, diagnostics);
    for annotation in &decl.annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on enum items \
                 (`@{}` on `{}`)",
                annotation.name, decl.name,
            ),
            annotation.span,
        ));
    }
    for variant in &decl.variants {
        diagnose_enum_variant_gaps(&decl.name, variant, diagnostics);
    }
}

/// Diagnose every type-param bound on a generic decl. Bounds parse
/// (`<T: Show>`) but resolve has no infrastructure to enforce them
/// yet, so we surface a single error per bound and lift / IR proceed
/// as if the param were unbounded.
fn diagnose_type_param_bounds(
    owner_name: &str,
    type_params: &[TypeParam],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for param in type_params {
        for bound in &param.bounds {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support type-parameter bounds \
                     (`{owner_name}<{}: {bound}>`)",
                    param.name,
                ),
                param.span,
            ));
        }
    }
}

/// Diagnose feature gaps on a single enum variant. Reuses
/// [`diagnose_struct_field_gaps`] for the per-field walk on struct
/// variants so the diagnostic wording stays identical between
/// `struct Foo { ... }` and `enum E { Foo { ... } }`.
fn diagnose_enum_variant_gaps(
    enum_name: &str,
    variant: &EnumVariant,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match &variant.data {
        EnumVariantData::Struct(fields) => {
            let owner = format!("{enum_name}.{}", variant.name);
            for field in fields {
                diagnose_struct_field_gaps(&owner, field, diagnostics);
            }
        }
        EnumVariantData::Tuple(_) | EnumVariantData::Unit => {}
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

/// Diagnose every feature gap on a protocol decl up front (generics,
/// annotations, generic methods, and `Self` in non-self position).
/// The protocol still registers so impl blocks targeting it produce
/// useful conformance diagnostics rather than "unknown protocol".
fn diagnose_protocol_feature_gaps(decl: &ProtocolDecl, diagnostics: &mut Vec<Diagnostic>) {
    if !decl.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support generic protocols \
                 (`{}` has type parameters)",
                decl.name,
            ),
            decl.span,
        ));
    }
    for annotation in &decl.annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on protocols \
                 (`@{}` on `{}`)",
                annotation.name, decl.name,
            ),
            annotation.span,
        ));
    }
    for method in &decl.methods {
        diagnose_protocol_method_feature_gaps(&decl.name, method, diagnostics);
    }
}

fn diagnose_protocol_method_feature_gaps(
    protocol_name: &str,
    method: &ProtocolMethod,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !method.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support generic protocol methods \
                 (`{protocol_name}.{}` has type parameters)",
                method.name,
            ),
            method.span,
        ));
    }
    for annotation in &method.annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on protocol methods \
                 (`@{}` on `{protocol_name}.{}`)",
                annotation.name, method.name,
            ),
            annotation.span,
        ));
    }
    if let Some(return_type) = &method.return_type
        && contains_self(return_type)
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support `Self` in protocol method return types \
                 (on `{protocol_name}.{}`)",
                method.name,
            ),
            type_expr_span(return_type),
        ));
    }
    for param in &method.params {
        if let Param::Regular { type_expr, .. } = param
            && contains_self(type_expr)
        {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support `Self` in protocol method parameter \
                     types (on `{protocol_name}.{}`)",
                    method.name,
                ),
                type_expr_span(type_expr),
            ));
        }
    }
}

/// True when `ty` (or a transitive component of it) is `TypeExpr::Self_`.
/// `Self` as a *receiver type* is captured separately by `Param::Self_`,
/// so this only fires on type annotations the protocol can't yet
/// represent in alpha.
fn contains_self(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Self_ { .. } => true,
        TypeExpr::Named { .. } | TypeExpr::Unit { .. } => false,
        TypeExpr::Generic { args, .. } => args.iter().any(contains_self),
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => params.iter().any(contains_self) || contains_self(return_type),
        TypeExpr::Union { types, .. } => types.iter().any(contains_self),
    }
}
