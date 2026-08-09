//! Collect sub-pass: register a canonical [`Identifier`] for every
//! globally-named decl. Pure registration: signature resolution
//! lives in [`super::lift_signatures`].
//!
//! Path encoding follows the [`Identifier`] convention: top-level
//! functions register at `path = ["name"]`. Static methods on
//! `Point` (declared inline in the struct body or in an `impl`
//! block) register at `path = ["Point", "name"]`. Both forms
//! produce the same registry entry so call resolution can't tell
//! them apart.
//!
//! The walk is split into two passes driven by
//! [`super::super::program::check_program`]: pass 1
//! ([`collect_file_decls`]) registers `Item::Function`, `Item::Struct`,
//! `Item::Enum`, `Item::Protocol`, `Item::Constant`, and
//! `Item::TypeAlias` (including each `decl.functions[i]`). Pass 2
//! ([`collect_file_impls`]) registers `Item::Impl`. Each pass runs
//! across every file in every package before the next starts.
//! That makes `impl Foo` order-independent relative to its target,
//! regardless of which file (or which package) declared `Foo`.
//!
//! `alias Pkg.Type` is accepted as a no-op at collect.
//! [`super::aliases::validate_aliases`] runs immediately after to
//! enforce path-len / target-exists / no-shadow rules.

use koja_ast::ast::{
    Annotation, AnnotationKind, BuiltinDecl, Constant, Diagnostic, EnumDecl, ExtendBlock, File,
    Function, ImplBlock, ImplMember, Item, Param, ProtocolDecl, ProtocolMethod, StructDecl,
    TypeAlias, TypeExpr, TypeParam, Visibility, is_intrinsic,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier};
use koja_ast::labels::type_expr_span;
use koja_ast::span::Span;

use crate::pipeline::visibility::check_reference_visibility;
use crate::program::CheckedPackage;
use crate::registry::{ClaimOutcome, GlobalKind, GlobalRegistry, InsertOutcome, VisibilityScope};

/// Pass 1 of collect: register every named decl (functions,
/// structs, enums, protocols, constants, type aliases) so that
/// downstream impl blocks have a fully-populated registry to look
/// up against. Skips impl blocks. Pass 2 handles those once every
/// file's types are in the registry.
pub(crate) fn collect_file_decls(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &file.items {
        match item {
            Item::Builtin(decl) => {
                register_builtin(decl, package, registry, diagnostics);
            }
            Item::Enum(decl) => {
                register_enum(decl, package, registry, diagnostics);
            }
            Item::Function(function) => {
                let identifier = Identifier::new(package, vec![function.name.clone()]);
                register_function_with_identifier(
                    function,
                    identifier,
                    SelfContext::RejectSelf,
                    None,
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
            Item::Extend(_) => {}
            Item::Constant(constant) => {
                register_constant(constant, package, registry, diagnostics);
            }
            // `alias Pkg.Type [as Local]` doesn't introduce a new
            // global identifier: it binds a file-private local name
            // to an existing one. Validation runs in
            // [`super::aliases::validate_aliases`]. Collect just
            // skips it here.
            Item::Alias(_) => {}
            // `type X = ...` is a package-wide global like a struct or
            // constant: it lives in the registry as a TypeAlias entry
            // so cross-file (same package) and cross-package (`Pkg.X`)
            // lookups go through the same machinery. The RHS
            // `ResolvedType` is stamped later by lift's
            // `lift_type_aliases` pass, after structs/enums/protocols
            // are registered so the RHS can reference them.
            Item::TypeAlias(alias) => {
                register_type_alias(alias, package, registry, diagnostics);
            }
        }
    }
}

/// Validate every nested type declaration (`struct A.B … end`) once
/// pass 1 has registered all types. A nested type's owner path must
/// name a struct / enum / protocol in the **same package**, and a
/// type nested under an enum must not shadow one of that enum's
/// variants (variants aren't registry entries, so the
/// duplicate-identifier check can't catch this. Every other
/// same-namespace collision falls out of the registry for free).
pub(crate) fn validate_nested_types(
    packages: &[CheckedPackage],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                match item {
                    Item::Struct(decl) if !decl.owner_path().is_empty() => {
                        validate_nested_owner(
                            &pkg.package,
                            decl.owner_path(),
                            decl.name(),
                            decl.span,
                            packages,
                            registry,
                            diagnostics,
                        );
                    }
                    Item::Enum(decl) if !decl.owner_path().is_empty() => {
                        validate_nested_owner(
                            &pkg.package,
                            decl.owner_path(),
                            decl.name(),
                            decl.span,
                            packages,
                            registry,
                            diagnostics,
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}

fn validate_nested_owner(
    package: &str,
    owner_path: &[String],
    leaf: &str,
    span: Span,
    packages: &[CheckedPackage],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let owner_name = owner_path.join(".");
    let owner_identifier = Identifier::new(package, owner_path.to_vec());
    let Some((_, entry)) = registry.lookup(&owner_identifier) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "nested type `{leaf}` must be declared under a type in the same \
                 package (`{owner_name}` is not a known type in `{package}`)"
            ),
            span,
        ));
        return;
    };
    match entry.kind {
        GlobalKind::Builtin(_) | GlobalKind::Protocol(_) | GlobalKind::Struct(_) => {}
        GlobalKind::Enum(_) => {
            if enum_has_variant(packages, package, owner_path, leaf) {
                diagnostics.push(Diagnostic::error(
                    format!("nested type `{leaf}` collides with a variant of `{owner_name}`"),
                    span,
                ));
            }
        }
        _ => diagnostics.push(Diagnostic::error(
            format!(
                "nested type `{leaf}` cannot be declared under `{owner_name}` (a {})",
                entry.kind.label(),
            ),
            span,
        )),
    }
}

/// Whether the enum declared at `(package, owner_path)` has a variant
/// named `name`. Scans the AST because variant data isn't stamped into
/// the registry until `lift_signatures`.
fn enum_has_variant(
    packages: &[CheckedPackage],
    package: &str,
    owner_path: &[String],
    name: &str,
) -> bool {
    packages
        .iter()
        .filter(|pkg| pkg.package == package)
        .flat_map(|pkg| &pkg.files)
        .flat_map(|file| &file.items)
        .filter_map(|item| match item {
            Item::Enum(decl) if decl.path == owner_path => Some(decl),
            _ => None,
        })
        .any(|decl| decl.variants.iter().any(|variant| variant.name == name))
}

/// Pass 2: register every `impl` and `extend` block. Runs after
/// [`collect_file_decls`] on all packages so cross-file/cross-package
/// targets resolve.
pub(crate) fn collect_file_impls(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &file.items {
        match item {
            Item::Impl(impl_block) => register_impl(impl_block, package, registry, diagnostics),
            Item::Extend(extend_block) => {
                register_extend(extend_block, package, registry, diagnostics);
            }
            _ => {}
        }
    }
}

/// Whether the registration site (top-level vs inside a `struct` /
/// `impl` body) accepts a `self` receiver. Lift_signatures carries a
/// richer struct-aware variant. Collect only needs to know "is `self`
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
///
/// `owner_type` is the registry id of the enclosing `struct` / `enum`
/// for method registrations (any `priv fn` declared inside the decl
/// or `impl` body scopes to that type), or `None` for top-level
/// functions (which scope to their package). Together with the
/// function's surface `Visibility` it picks one of the three
/// [`VisibilityScope`] variants. See [`function_visibility_scope`].
fn register_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    self_context: SelfContext,
    owner_type: Option<GlobalRegistryId>,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_doc_on_private(
        &function.name,
        "function",
        function.visibility,
        &function.annotations,
        diagnostics,
    );
    if reject_self_param(function, &identifier, self_context, diagnostics) {
        return;
    }
    let deprecation = deprecation_message(&function.annotations, diagnostics);
    let type_params = type_param_names(&function.type_params);
    let visibility = function_visibility_scope(function.visibility, owner_type);
    match registry.insert_function(identifier, function.span, type_params, visibility) {
        InsertOutcome::Fresh(id) => stamp_deprecation(registry, id, deprecation),
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

/// Map the surface `(Visibility, owner_type)` pair to the
/// typecheck-internal [`VisibilityScope`]. Public functions get the
/// `Public` variant regardless of owner. `priv fn` declared inside a
/// type body becomes [`VisibilityScope::TypePrivate`]. A top-level
/// `priv fn` becomes [`VisibilityScope::PackagePrivate`]. The owner
/// id is the type the method belongs to: even an inherent or
/// protocol-impl method on `Foo` carries `Foo`'s id, so cross-impl
/// calls within the same type all share one scope.
fn function_visibility_scope(
    visibility: Visibility,
    owner_type: Option<GlobalRegistryId>,
) -> VisibilityScope {
    match (visibility, owner_type) {
        (Visibility::Public, _) => VisibilityScope::Public,
        (Visibility::Private, Some(owner)) => VisibilityScope::TypePrivate(owner),
        (Visibility::Private, None) => VisibilityScope::PackagePrivate,
    }
}

/// Map the surface `Visibility` of a non-function decl (struct, enum,
/// constant, type alias, protocol) to its [`VisibilityScope`]. These
/// are always top-level, so `priv` means package-private.
fn package_visibility_scope(visibility: Visibility) -> VisibilityScope {
    function_visibility_scope(visibility, None)
}

/// `@doc` on a private declaration is a compile error. Private items
/// never surface in generated docs, so the docstring is dead metadata.
fn diagnose_doc_on_private(
    name: &str,
    kind_label: &str,
    visibility: Visibility,
    annotations: &[Annotation],
    diagnostics: &mut Vec<Diagnostic>,
) {
    if visibility == Visibility::Public {
        return;
    }
    for annotation in annotations {
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`@doc` is not valid on private {kind_label} `{name}`"),
                "private declarations never appear in generated docs. Document it with a `#` \
                 comment instead."
                    .to_string(),
                annotation.span,
            ));
        }
    }
}

/// Annotations skipped by the per-decl "unsupported annotation" gap
/// helpers because a dedicated check owns them. `@doc` is checked by
/// [`diagnose_doc_on_private`] and `@deprecated` by
/// [`deprecation_message`], matched by name so malformed shapes
/// aren't double-diagnosed.
fn has_dedicated_validation(annotation: &Annotation) -> bool {
    annotation.name == "deprecated" || matches!(annotation.kind(), AnnotationKind::Doc(_))
}

/// The validated `@deprecated` message on a decl. Bare `@deprecated`
/// and non-string or empty payloads are rejected. Every deprecation
/// warning must tell callers what to use instead.
fn deprecation_message(
    annotations: &[Annotation],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let mut message = None;
    for annotation in annotations {
        if annotation.name != "deprecated" {
            continue;
        }
        match annotation.kind() {
            // Trimmed so `"""` payloads don't drag their surrounding
            // newlines into every warning.
            AnnotationKind::Deprecated { message: text } if !text.trim().is_empty() => {
                message = Some(text.trim().to_string());
            }
            _ => diagnostics.push(Diagnostic::error_with_hint(
                "`@deprecated` requires a message".to_string(),
                "describe the replacement, e.g. `@deprecated \"\"\"Use ... instead.\"\"\"`"
                    .to_string(),
                annotation.span,
            )),
        }
    }
    message
}

/// Stamp a validated `@deprecated` message onto a freshly inserted
/// entry.
fn stamp_deprecation(
    registry: &mut GlobalRegistry,
    id: GlobalRegistryId,
    deprecation: Option<String>,
) {
    if let Some(message) = deprecation {
        registry.set_deprecation(id, message);
    }
}

/// Reject a `self` receiver only when registration is happening
/// outside a struct/impl context (top-level functions). Inside a
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
    diagnose_doc_on_private(
        decl.name(),
        "struct",
        decl.visibility,
        &decl.annotations,
        diagnostics,
    );
    diagnose_intrinsic_on_struct(decl, diagnostics);
    let deprecation = deprecation_message(&decl.annotations, diagnostics);
    let identifier = Identifier::new(package, decl.path.clone());
    let struct_id = register_ordinary_struct(decl, &identifier, deprecation, registry, diagnostics);
    for function in &decl.functions {
        let method_identifier = Identifier::member(package, &decl.path, &function.name);
        register_function_with_identifier(
            function,
            method_identifier,
            SelfContext::AllowSelf,
            struct_id,
            registry,
            diagnostics,
        );
    }
}

fn register_ordinary_struct(
    decl: &StructDecl,
    identifier: &Identifier,
    deprecation: Option<String>,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<GlobalRegistryId> {
    insert_struct_entry(
        identifier,
        decl.span,
        type_param_names(&decl.type_params),
        package_visibility_scope(decl.visibility),
        deprecation,
        registry,
        diagnostics,
    )
}

/// Shared struct-entry insert with the standard collision
/// diagnostic. On collision the existing entry's id is returned so
/// the caller can still register inline methods against whatever
/// type already owns the name: the duplicate decl is itself
/// diagnosed, and methods declared under it would otherwise dangle.
fn insert_struct_entry(
    identifier: &Identifier,
    span: Span,
    type_params: Vec<String>,
    visibility: VisibilityScope,
    deprecation: Option<String>,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<GlobalRegistryId> {
    match registry.insert_struct(identifier.clone(), span, type_params, visibility) {
        InsertOutcome::Fresh(id) => {
            stamp_deprecation(registry, id, deprecation);
            Some(id)
        }
        InsertOutcome::Collision { existing } => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`{}` is already defined", existing.identifier),
                format!(
                    "previous {} definition is at line {}",
                    existing.kind.label(),
                    existing.span.start.line
                ),
                span,
            ));
            registry.lookup(identifier).map(|(id, _)| id)
        }
    }
}

/// `@intrinsic` on a struct was the pre-`builtin` way to declare a
/// compiler-owned type. `@intrinsic fn` is untouched.
fn diagnose_intrinsic_on_struct(decl: &StructDecl, diagnostics: &mut Vec<Diagnostic>) {
    if !is_intrinsic(&decl.annotations) {
        return;
    }
    let span = decl
        .annotations
        .iter()
        .find(|annotation| annotation.name == "intrinsic")
        .map(|annotation| annotation.span)
        .unwrap_or(decl.span);
    diagnostics.push(Diagnostic::error_with_hint(
        format!(
            "`@intrinsic` on struct `{}` is replaced by the `builtin` declaration",
            decl.name(),
        ),
        format!("declare it as `builtin {} ... end`", decl.name()),
        span,
    ));
}

/// Register a `builtin` declaration by claiming the compiler's
/// seeded stub. An unknown name is a compile error. Failed claims
/// fall back to an ordinary struct insert so duplicates get the
/// standard "already defined" diagnostic and the decl's methods
/// never dangle.
fn register_builtin(
    decl: &BuiltinDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_builtin_feature_gaps(decl, diagnostics);
    diagnose_doc_on_private(
        decl.name(),
        "builtin",
        decl.visibility,
        &decl.annotations,
        diagnostics,
    );
    let deprecation = deprecation_message(&decl.annotations, diagnostics);
    let identifier = Identifier::new(package, decl.path.clone());
    let type_params = type_param_names(&decl.type_params);
    let builtin_id = match registry.claim_builtin_stub(&identifier, decl.span, type_params) {
        Some(ClaimOutcome::Claimed(id)) => {
            stamp_deprecation(registry, id, deprecation);
            Some(id)
        }
        Some(ClaimOutcome::ArityMismatch { id, expected_arity }) => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "builtin `{}` takes exactly {expected_arity} type parameter{}, found {}",
                    decl.name(),
                    if expected_arity == 1 { "" } else { "s" },
                    decl.type_params.len(),
                ),
                decl.span,
            ));
            stamp_deprecation(registry, id, deprecation);
            Some(id)
        }
        None => {
            if registry.lookup(&identifier).is_none() {
                diagnostics.push(Diagnostic::error_with_hint(
                    format!("`{identifier}` is not a builtin type"),
                    "`builtin` declares a compiler-provided type like `String` or `List`. \
                     Declare an ordinary type with `struct` or `enum`."
                        .to_string(),
                    decl.span,
                ));
            }
            insert_struct_entry(
                &identifier,
                decl.span,
                type_param_names(&decl.type_params),
                package_visibility_scope(decl.visibility),
                deprecation,
                registry,
                diagnostics,
            )
        }
    };
    for function in &decl.functions {
        let method_identifier = Identifier::member(package, &decl.path, &function.name);
        register_function_with_identifier(
            function,
            method_identifier,
            SelfContext::AllowSelf,
            builtin_id,
            registry,
            diagnostics,
        );
    }
}

/// Shape and annotation checks for `builtin` declarations.
fn diagnose_builtin_feature_gaps(decl: &BuiltinDecl, diagnostics: &mut Vec<Diagnostic>) {
    if decl.visibility != Visibility::Public {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("builtin type `{}` cannot be private", decl.name()),
            "builtin types are always public".to_string(),
            decl.span,
        ));
    }
    for annotation in &decl.annotations {
        if has_dedicated_validation(annotation) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support annotations on builtin items \
                 (`@{}` on `{}`)",
                annotation.name,
                decl.name(),
            ),
            annotation.span,
        ));
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
    diagnose_doc_on_private(
        decl.name(),
        "enum",
        decl.visibility,
        &decl.annotations,
        diagnostics,
    );
    let deprecation = deprecation_message(&decl.annotations, diagnostics);
    let identifier = Identifier::new(package, decl.path.clone());
    let type_params = type_param_names(&decl.type_params);
    let visibility = package_visibility_scope(decl.visibility);
    let enum_id = match registry.insert_enum(identifier.clone(), decl.span, type_params, visibility)
    {
        InsertOutcome::Fresh(id) => {
            stamp_deprecation(registry, id, deprecation);
            Some(id)
        }
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
            registry.lookup(&identifier).map(|(id, _)| id)
        }
    };
    for function in &decl.functions {
        let method_identifier = Identifier::member(package, &decl.path, &function.name);
        register_function_with_identifier(
            function,
            method_identifier,
            SelfContext::AllowSelf,
            enum_id,
            registry,
            diagnostics,
        );
    }
}

/// Register every method declared in an `impl Trait for Type` block
/// under `(package, [type_name, fn_name])`. Conformance facts
/// (`target : protocol`) are recorded at lift time onto the target's
/// struct/enum definition. Duplicate `impl P for T` blocks surface
/// there. The impl's `package` is the package the block lives in,
/// which (for now) also has to be where `Type` is declared. Cross-
/// package protocol impls are not yet supported.
fn register_impl(
    impl_block: &ImplBlock,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_impl_member_feature_gaps(impl_block, diagnostics);
    let Some(target_path) = nominal_target_path(&impl_block.target) else {
        diagnostics.push(Diagnostic::error(
            "typecheck does not yet support generic impl targets".to_string(),
            type_expr_span(&impl_block.target),
        ));
        return;
    };
    // Impls are same-package (orphan rule), so the whole dotted path is
    // the target's path within `package`.
    let target_identifier = Identifier::new(package, target_path.to_vec());
    let Some((target_id, entry)) = registry.lookup(&target_identifier) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck cannot extend unknown type `{}`",
                target_path.join(".")
            ),
            type_expr_span(&impl_block.target),
        ));
        return;
    };
    if !matches!(
        entry.kind,
        GlobalKind::Builtin(_) | GlobalKind::Enum(_) | GlobalKind::Struct(_)
    ) {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck only supports `impl` on structs, enums, and builtins \
                 (`{}` is a {})",
                target_path.join("."),
                entry.kind.label(),
            ),
            type_expr_span(&impl_block.target),
        ));
        return;
    }
    register_block_methods(
        package,
        target_path,
        target_id,
        &impl_block.members,
        registry,
        diagnostics,
    );
}

/// Register every method declared in an `extend Type ... end` block,
/// routing the methods through the target's qualified identifier so
/// cross-package extends land in the same collision-detection slot
/// as same-package ones.
fn register_extend(
    extend_block: &ExtendBlock,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_extend_member_feature_gaps(extend_block, diagnostics);
    let Some(path) = nominal_target_path(&extend_block.target) else {
        diagnostics.push(Diagnostic::error(
            "typecheck does not yet support generic or function `extend` targets".to_string(),
            type_expr_span(&extend_block.target),
        ));
        return;
    };
    let Some((target_id, target_package, target_path)) = lookup_owner_path(path, package, registry)
    else {
        diagnostics.push(Diagnostic::error(
            format!("typecheck cannot extend unknown type `{}`", path.join(".")),
            type_expr_span(&extend_block.target),
        ));
        return;
    };
    let target_entry = registry
        .get(target_id)
        .expect("lookup_owner_path returned a live id");
    check_reference_visibility(
        target_entry,
        package,
        type_expr_span(&extend_block.target),
        diagnostics,
    );
    let entry_kind = &target_entry.kind;
    // Protocol targets are admitted for static methods only. Lift
    // diagnoses `self` receivers on them.
    if !matches!(
        entry_kind,
        GlobalKind::Builtin(_)
            | GlobalKind::Enum(_)
            | GlobalKind::Protocol(_)
            | GlobalKind::Struct(_)
    ) {
        diagnostics.push(Diagnostic::error(
            format!(
                "`extend` only supports structs, enums, builtins, and protocols \
                 (`{}` is a {})",
                target_path.join("."),
                entry_kind.label(),
            ),
            type_expr_span(&extend_block.target),
        ));
        return;
    }
    register_block_methods(
        &target_package,
        &target_path,
        target_id,
        &extend_block.members,
        registry,
        diagnostics,
    );
}

/// Shared method-registration loop for `impl` and `extend` bodies.
/// Each `fn` registers under `<target_package>.<target_path…>.<method>`,
/// so methods on a nested type land in that type's namespace.
fn register_block_methods(
    target_package: &str,
    target_path: &[String],
    target_id: GlobalRegistryId,
    members: &[ImplMember],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let method_identifier = Identifier::member(target_package, target_path, &function.name);
        register_function_with_identifier(
            function,
            method_identifier,
            SelfContext::AllowSelf,
            Some(target_id),
            registry,
            diagnostics,
        );
    }
}

/// Register a protocol decl. Stamps `type_params` as
/// `["Self", ...user_declared]` so `Self` lives at index 0 and
/// resolves through the same machinery as user-declared params.
/// Reserves the literal `"Self"`: a user-declared param named
/// `Self` would shadow the implicit slot, so we diagnose and
/// register without it.
fn register_protocol(
    decl: &ProtocolDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_protocol_feature_gaps(decl, diagnostics);
    diagnose_doc_on_private(
        &decl.name,
        "protocol",
        decl.visibility,
        &decl.annotations,
        diagnostics,
    );
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
    let visibility = package_visibility_scope(decl.visibility);
    let deprecation = deprecation_message(&decl.annotations, diagnostics);
    match registry.insert_protocol(identifier, decl.span, type_params, visibility) {
        InsertOutcome::Fresh(id) => stamp_deprecation(registry, id, deprecation),
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
}

/// Register a package-level `const NAME = expr` declaration. Stamps
/// the constant in the `Constant(None)` state. `lift_signatures`
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
    diagnose_doc_on_private(
        &constant.name,
        "constant",
        constant.visibility,
        &constant.annotations,
        diagnostics,
    );
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    let visibility = package_visibility_scope(constant.visibility);
    let deprecation = deprecation_message(&constant.annotations, diagnostics);
    match registry.insert_constant(identifier, constant.span, visibility) {
        InsertOutcome::Fresh(id) => stamp_deprecation(registry, id, deprecation),
        InsertOutcome::Collision { existing } => {
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
}

/// Register a `type X = ...` alias with the package-qualified
/// identifier `<package>.<name>`. Only flag unsupported annotations
/// here. The RHS `TypeExpr` is resolved later by lift's
/// `lift_type_aliases` pass and the resulting `ResolvedType` is
/// stamped via `set_type_alias_definition`.
fn register_type_alias(
    alias: &TypeAlias,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_alias_annotations(&alias.name, &alias.annotations, diagnostics);
    diagnose_doc_on_private(
        &alias.name,
        "type alias",
        alias.visibility,
        &alias.annotations,
        diagnostics,
    );
    let identifier = Identifier::new(package, vec![alias.name.clone()]);
    let visibility = package_visibility_scope(alias.visibility);
    let deprecation = deprecation_message(&alias.annotations, diagnostics);
    match registry.insert_type_alias(identifier, alias.span, visibility) {
        InsertOutcome::Fresh(id) => stamp_deprecation(registry, id, deprecation),
        InsertOutcome::Collision { existing } => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`{}` is already defined", existing.identifier),
                format!(
                    "previous {} definition is at line {}",
                    existing.kind.label(),
                    existing.span.start.line
                ),
                alias.span,
            ));
        }
    }
}

fn diagnose_alias_annotations(
    alias_name: &str,
    annotations: &[Annotation],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for annotation in annotations {
        if has_dedicated_validation(annotation) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support `@{}` on type alias `{alias_name}`",
                annotation.name,
            ),
            annotation.span,
        ));
    }
}

fn diagnose_constant_annotations(
    constant_name: &str,
    annotations: &[Annotation],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for annotation in annotations {
        if has_dedicated_validation(annotation) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support annotations on constant items \
                 (`@{}` on `{constant_name}`)",
                annotation.name,
            ),
            annotation.span,
        ));
    }
}

/// The dotted type path of an `impl` / `extend` target (`[Foo]`,
/// `[Outer, Inner]`), or `None` for non-nominal shapes. Type-args
/// don't affect keying.
pub(crate) fn nominal_target_path(target: &TypeExpr) -> Option<&[String]> {
    match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => Some(path.as_slice()),
        _ => None,
    }
}

/// Resolve a target `path` to its owning `(id, package, path)`. A
/// same-package nested type (`Outer.Inner`) wins over the
/// `<package>.<rest>` reading, matching type/value resolution.
pub(crate) fn lookup_owner_path(
    path: &[String],
    current_package: &str,
    registry: &GlobalRegistry,
) -> Option<(GlobalRegistryId, String, Vec<String>)> {
    if let Some((id, _)) = registry.lookup(&Identifier::new(current_package, path.to_vec())) {
        return Some((id, current_package.to_string(), path.to_vec()));
    }
    if path.len() >= 2
        && let Some((id, _)) = registry.lookup(&Identifier::new(&path[0], path[1..].to_vec()))
    {
        return Some((id, path[0].clone(), path[1..].to_vec()));
    }
    None
}

/// Project the AST `[TypeParam]` list down to the param-name `Vec`
/// the registry stores. Bounds are not stamped here. `lift_signatures`
/// resolves bound names against registered protocols once every
/// protocol id exists, then writes them through
/// [`crate::registry::GlobalRegistry::set_type_param_bounds`].
fn type_param_names(type_params: &[TypeParam]) -> Vec<String> {
    type_params.iter().map(|p| p.name.clone()).collect()
}

/// Diagnose every feature gap on a struct decl up front so collect
/// is the single seam covering them. The struct still registers (so
/// downstream `resolve` finds it for diagnostic-friendly error
/// messages). lift_signatures stamps a permissive "best effort"
/// definition in the presence of these gaps so the surrounding
/// program shape stays accurate.
fn diagnose_struct_feature_gaps(decl: &StructDecl, diagnostics: &mut Vec<Diagnostic>) {
    for annotation in &decl.annotations {
        // `@intrinsic` on a struct gets the targeted replacement
        // diagnostic from [`diagnose_intrinsic_on_struct`].
        if has_dedicated_validation(annotation)
            || matches!(annotation.kind(), AnnotationKind::Intrinsic)
        {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support annotations on struct items \
                 (`@{}` on `{}`)",
                annotation.name,
                decl.name(),
            ),
            annotation.span,
        ));
    }
}

/// Diagnose every feature gap on an enum decl up front so collect is
/// the single seam covering them. Mirrors [`diagnose_struct_feature_gaps`]:
/// the decl still registers in the presence of any gap so resolve
/// sees a populated registry.
fn diagnose_enum_feature_gaps(decl: &EnumDecl, diagnostics: &mut Vec<Diagnostic>) {
    for annotation in &decl.annotations {
        if has_dedicated_validation(annotation) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support annotations on enum items \
                 (`@{}` on `{}`)",
                annotation.name,
                decl.name(),
            ),
            annotation.span,
        ));
    }
}

/// Diagnose the only impl-block member shape we don't yet support:
/// `type Alias = ...`. `Function` members flow through normal
/// registration in [`register_impl`]. This pass surfaces a diagnostic
/// for every other shape so the user sees one error per offending
/// member rather than a single block-level message.
fn diagnose_impl_member_feature_gaps(impl_block: &ImplBlock, diagnostics: &mut Vec<Diagnostic>) {
    for member in &impl_block.members {
        if let ImplMember::TypeAlias(alias) = member {
            diagnostics.push(Diagnostic::error(
                "typecheck does not yet support `type` aliases inside `impl` blocks".to_string(),
                alias.span,
            ));
        }
    }
}

/// Mirror of [`diagnose_impl_member_feature_gaps`] for `extend`.
fn diagnose_extend_member_feature_gaps(
    extend_block: &ExtendBlock,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in &extend_block.members {
        if let ImplMember::TypeAlias(alias) = member {
            diagnostics.push(Diagnostic::error(
                "typecheck does not yet support `type` aliases inside `extend` blocks".to_string(),
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
        if has_dedicated_validation(annotation) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support annotations on protocols \
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
                "typecheck does not yet support generic protocol methods \
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
                "typecheck does not yet support annotations on protocol methods \
                 (`@{}` on `{protocol_name}.{}`)",
                annotation.name, method.name,
            ),
            annotation.span,
        ));
    }
}
