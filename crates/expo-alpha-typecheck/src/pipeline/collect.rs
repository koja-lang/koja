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
//! static methods on structs (no `self`). Default field values and
//! impl-block type aliases surface as feature-gap diagnostics here so
//! later passes never see those shapes. `alias Pkg.Type` is accepted
//! as a no-op at collect; [`super::aliases::validate_aliases`] runs
//! immediately after to enforce path-len / target-exists / no-shadow
//! rules.

use expo_ast::ast::{
    Annotation, AnnotationKind, Constant, Diagnostic, EnumDecl, EnumVariant, EnumVariantData, File,
    Function, ImplBlock, ImplMember, Item, Param, ProtocolDecl, ProtocolMethod, StructDecl,
    StructField, TypeExpr, TypeParam,
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
            Item::Constant(constant) => {
                register_constant(constant, package, registry, diagnostics);
            }
            // `alias Pkg.Type [as Local]` doesn't introduce a new
            // global identifier — it binds a file-private local name
            // to an existing one. Validation runs in
            // [`super::aliases::validate_aliases`]; collect just
            // skips it here.
            Item::Alias(_) => {}
            // Other Item variants land as alpha grows. Reject them
            // explicitly so unsupported shapes diagnose instead of
            // round-tripping silently.
            Item::TypeAlias(_) => {
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
    let type_params = type_param_names(&function.type_params);
    match registry.insert_function(identifier, function.span, type_params) {
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
    let type_params = type_param_names(&decl.type_params);
    match registry.insert_struct(identifier, decl.span, type_params) {
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
    let type_params = type_param_names(&decl.type_params);
    match registry.insert_enum(identifier, decl.span, type_params) {
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
/// under `(package, [type_name, fn_name])`. Inherent and trait
/// impls share this path — neither gets its own registry entry.
/// Trait-impl conformance facts (`target : protocol`) are recorded
/// at lift time onto the target's struct/enum definition; duplicate
/// `impl P for T` blocks surface there. Multiple inherent `impl T`
/// blocks accumulate methods on `T` (collisions surface per-method,
/// not per-block).
fn register_impl(
    impl_block: &ImplBlock,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_impl_member_feature_gaps(impl_block, diagnostics);
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

/// Register a protocol decl. Stamps `type_params` as
/// `["Self", ...user_declared]` so `Self` lives at index 0 and
/// resolves through the same machinery as user-declared params.
/// Reserves the literal `"Self"` — a user-declared param named
/// `Self` would shadow the implicit slot, so we diagnose and
/// register without it.
fn register_protocol(
    decl: &ProtocolDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_protocol_feature_gaps(decl, diagnostics);
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let mut type_params = vec!["Self".to_string()];
    for param in &decl.type_params {
        if param.name == "Self" {
            diagnostics.push(Diagnostic::error(
                format!(
                    "type parameter name `Self` is reserved (on protocol `{}`)",
                    decl.name,
                ),
                param.span,
            ));
            continue;
        }
        type_params.push(param.name.clone());
    }
    if let InsertOutcome::Collision { existing } =
        registry.insert_protocol(identifier, decl.span, type_params)
    {
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

/// Register a package-level `const NAME = expr` declaration. Stamps
/// the constant in the `Constant(None)` state — `lift_signatures`
/// resolves the optional type annotation + RHS expression and stamps
/// the [`crate::registry::ConstantDefinition`] later. Constants
/// occupy the same identifier namespace as functions / structs /
/// enums / protocols, so collision diagnostics flow through the
/// shared insert path.
fn register_constant(
    constant: &Constant,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_constant_annotations(&constant.name, &constant.annotations, diagnostics);
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    if let InsertOutcome::Collision { existing } =
        registry.insert_constant(identifier, constant.span)
    {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("`{}` is already defined", existing.identifier),
            format!(
                "previous {} definition is at line {}",
                existing.kind.label(),
                existing.span.start.line
            ),
            constant.span,
        ));
    }
}

fn diagnose_constant_annotations(
    constant_name: &str,
    annotations: &[Annotation],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for annotation in annotations {
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on constant items \
                 (`@{}` on `{constant_name}`)",
                annotation.name,
            ),
            annotation.span,
        ));
    }
}

/// Pull the bare type name out of `impl Foo` / `impl Foo<...>` /
/// `impl Foo.Bar` shapes, returning `None` for anything we don't
/// support (dotted paths, function types, unions). Both
/// `Named { path: [Foo] }` and `Generic { path: [Foo], args: [...] }`
/// resolve to `"Foo"` — the type-args contribute to free-name
/// extraction (impl `<T>`s) and to method registration is keyed only
/// at `[Foo, method]` regardless of args.
fn simple_named_target(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } if path.len() == 1 => {
            Some(path[0].as_str())
        }
        _ => None,
    }
}

/// Project the AST `[TypeParam]` list down to the param-name `Vec`
/// the registry stores. Bounds are not stamped here — `lift_signatures`
/// resolves bound names against registered protocols once every
/// protocol id exists, then writes them through
/// [`crate::registry::GlobalRegistry::set_type_param_bounds`].
fn type_param_names(type_params: &[TypeParam]) -> Vec<String> {
    type_params.iter().map(|p| p.name.clone()).collect()
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
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            continue;
        }
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
    for annotation in &decl.annotations {
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            continue;
        }
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

/// Diagnose protocol-decl feature gaps still present after slice 2.5
/// (annotations, generic protocol methods). Generic protocol decls
/// and `Self` in non-receiver positions are now supported via lift's
/// `["Self", ...user_declared]` type-param stamping.
fn diagnose_protocol_feature_gaps(decl: &ProtocolDecl, diagnostics: &mut Vec<Diagnostic>) {
    for annotation in &decl.annotations {
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            continue;
        }
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
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on protocol methods \
                 (`@{}` on `{protocol_name}.{}`)",
                annotation.name, method.name,
            ),
            annotation.span,
        ));
    }
}
