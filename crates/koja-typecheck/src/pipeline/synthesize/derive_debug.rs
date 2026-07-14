//! Synthesizes `impl Debug for T` for every user-defined struct /
//! enum that doesn't already have one. Mutates `file.items` in place
//! by appending the synthetic impl blocks.
//!
//! Synthesized impls are indistinguishable from user-written code, so
//! the rest of typecheck (collect / lift / resolve / seal) needs no
//! special-casing. Runs as a **pre-collect** pass in
//! [`crate::check_program`] so the new items land before name binding.
//! The pipeline's main `synthesize` step (today: `for_desugar`)
//! runs after lift and only touches function bodies, so it can't
//! introduce items.
//!
//! ## Generic types
//!
//! Generic types (`Pair<A, B>`, `Container<T>`, …) get the same full
//! body as concrete ones. Field interpolations call `.format()` on
//! bare type parameters (`A.format()`). The typechecker resolves
//! those through the universal-`Debug` fallback in
//! [`crate::pipeline::resolve::calls::bounded`]: every concrete
//! monomorphization either has a synthesized `Debug` impl (user
//! types) or a hand-written stdlib impl (`List<T>`, `Map<K, V>`,
//! `Set<T>`, `Option<T>`, `Result<T, E>`, `Pair<A, B>`), so the call
//! always finds a provider after monomorphization.
//!
//! Stdlib generic types are skipped because their hand-written
//! impls live in the same files and [`collect_existing_debug_impls`]
//! detects them.
//!
//! ## Opaque field types
//!
//! Fields whose type is opaque to the synthesizer (`CPtr<T>`,
//! `Indirect<T>`, `Pointer<T>`, function / self / union / unit)
//! render as the literal `"..."` placeholder instead of an
//! interpolated `.format()` call. These types either have no
//! `Debug` impl (CPtr / Indirect / Pointer) or carry no value to
//! format (function / self-recursion / union / unit), so the
//! field-level fallback keeps the synthesizer total without
//! dragging the universal-Debug fallback into compiler-internal
//! types.

use koja_ast::ast::{
    Annotation, Arg, EnumDecl, EnumVariant, EnumVariantData, Expr, ExprKind, FieldPattern, File,
    Function, ImplBlock, ImplMember, Item, MatchArm, Param, Pattern, Statement, StringPart,
    StructDecl, StructField, TypeExpr, TypeParam, Visibility,
};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

use crate::program::CheckedPackage;

const DEBUG_PROTOCOL: &str = "Debug";
const FORMAT_METHOD: &str = "format";
const INSPECT_METHOD: &str = "inspect";
const IO_TYPE: &str = "IO";
const PRINT_METHOD: &str = "print";
const PUTS_METHOD: &str = "puts";
const STRING_TYPE: &str = "String";

/// Synthesizes `impl Debug for T` for every struct / enum in `pkg`
/// that doesn't already have one anywhere in the same package.
/// Mutates each file's `items` in place by appending the synthetic
/// impl blocks alongside the type's declaration.
///
/// The existing-impl scan runs across all of the package's files
/// first so a hand-written `impl Debug for List<T>` in
/// `debug_containers.koja` suppresses synthesis in
/// `list.koja`. A naive per-file scan would produce both the
/// hand-written impl and a synthesized one and trip the
/// `duplicate impl` collision in
/// [`crate::pipeline::collect`].
pub(crate) fn derive_debug_package(pkg: &mut CheckedPackage) {
    let existing = collect_package_debug_impls(pkg);
    for file in &mut pkg.files {
        synthesize_into_file(file, &existing);
    }
}

fn collect_package_debug_impls(pkg: &CheckedPackage) -> Vec<String> {
    pkg.files
        .iter()
        .flat_map(collect_existing_debug_impls)
        .collect()
}

fn synthesize_into_file(file: &mut File, existing: &[String]) {
    let mut synthesized: Vec<Item> = Vec::new();
    for item in &file.items {
        match item {
            Item::Struct(decl) if needs_struct_derive(decl, existing) => {
                synthesized.push(synthesize_struct_impl(decl));
            }
            Item::Enum(decl) if needs_enum_derive(decl, existing) => {
                synthesized.push(synthesize_enum_impl(decl));
            }
            _ => {}
        }
    }
    file.items.extend(synthesized);
}

/// Returns the bare type names that already have an explicit
/// `impl Debug for T` block in this file. Generic args are ignored
/// so `impl Debug for List<T>` matches a struct named `List`.
fn collect_existing_debug_impls(file: &File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(block) => debug_impl_target(block),
            _ => None,
        })
        .collect()
}

fn debug_impl_target(block: &ImplBlock) -> Option<String> {
    let trait_name = type_expr_head(&block.trait_expr)?;
    if trait_name != DEBUG_PROTOCOL {
        return None;
    }
    type_expr_path(&block.target)
}

/// The target type's full dotted path (`Net.TCPSocket`,
/// `Process.ExitSignal`), used to match an existing impl against a
/// decl's [`StructDecl::path`] / [`EnumDecl::path`].
fn type_expr_path(te: &TypeExpr) -> Option<String> {
    match te {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => Some(path.join(".")),
        _ => None,
    }
}

fn type_expr_head(te: &TypeExpr) -> Option<&str> {
    match te {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
            path.last().map(String::as_str)
        }
        TypeExpr::Function { .. }
        | TypeExpr::Self_ { .. }
        | TypeExpr::Union { .. }
        | TypeExpr::Unit { .. } => None,
    }
}

fn needs_struct_derive(decl: &StructDecl, existing: &[String]) -> bool {
    !existing.iter().any(|n| n == &decl.path.join("."))
}

/// Empty enums (no variants) are uninhabited: a `match self end`
/// body with no arms is rejected by typecheck, and there's no value
/// to format anyway. Skip them.
fn needs_enum_derive(decl: &EnumDecl, existing: &[String]) -> bool {
    !decl.variants.is_empty() && !existing.iter().any(|n| n == &decl.path.join("."))
}

fn synthesize_struct_impl(decl: &StructDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.path, &decl.type_params, span);
    let format_body = struct_format_body(&decl.path, &decl.fields, span);
    debug_impl_block(target, format_body, span)
}

fn synthesize_enum_impl(decl: &EnumDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.path, &decl.type_params, span);
    let format_body = enum_format_body(&decl.path, &decl.variants, span);
    debug_impl_block(target, format_body, span)
}

/// Builds the full `impl Debug for T` block carrying all three
/// methods (`format`, `print`, `inspect`). `format_body` is supplied.
/// `print` and `inspect` come from [`print_function`] /
/// [`inspect_function`] and inline the same bodies the `Debug`
/// protocol declares as defaults in `lib/global/src/debug.koja`.
/// Resolve doesn't yet pull protocol default bodies into impls
/// that omit them, so we inline them at synthesis time.
fn debug_impl_block(target: TypeExpr, format_body: Expr, span: Span) -> Item {
    Item::Impl(ImplBlock {
        target,
        trait_expr: debug_trait_expr(span),
        members: vec![
            ImplMember::Function(format_function(format_body, span)),
            ImplMember::Function(print_function(span)),
            ImplMember::Function(inspect_function(span)),
        ],
        span,
    })
}

/// Builds the `Target<Params>` type expression on the `impl ... for`
/// side, mirroring the type's own generic parameters so the impl
/// monomorphizes per concrete instantiation.
fn self_target_type(path: &[String], type_params: &[TypeParam], span: Span) -> TypeExpr {
    if type_params.is_empty() {
        TypeExpr::Named {
            path: path.to_vec(),
            span,
        }
    } else {
        let args = type_params
            .iter()
            .map(|tp| named_type(&tp.name, span))
            .collect();
        TypeExpr::Generic {
            path: path.to_vec(),
            args,
            span,
        }
    }
}

fn debug_trait_expr(span: Span) -> TypeExpr {
    named_type(DEBUG_PROTOCOL, span)
}

fn named_type(name: &str, span: Span) -> TypeExpr {
    TypeExpr::Named {
        path: vec![name.to_string()],
        span,
    }
}

/// Builds `fn format(self) -> String <body> end`.
fn format_function(body_expr: Expr, span: Span) -> Function {
    Function {
        annotations: Vec::<Annotation>::new(),
        visibility: Visibility::Public,
        name: FORMAT_METHOD.to_string(),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            local_id: None,
            span,
        }],
        return_type: Some(named_type(STRING_TYPE, span)),
        body: Some(vec![Statement::Expr(body_expr)]),
        span,
    }
}

/// Builds `fn print(self) IO.puts(self.format()) end`. Mirrors the
/// default body declared on `Debug.print` in
/// `lib/global/src/debug.koja`.
fn print_function(span: Span) -> Function {
    let format_call = method_call_no_args(self_expr(span), FORMAT_METHOD, span);
    let puts_call = Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(ident_expr(IO_TYPE, span)),
            method: PUTS_METHOD.to_string(),
            args: vec![Arg {
                name: None,
                value: format_call,
                span,
            }],
            type_args: Vec::new(),
        },
        span,
    );
    Function {
        annotations: Vec::<Annotation>::new(),
        visibility: Visibility::Public,
        name: PRINT_METHOD.to_string(),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            local_id: None,
            span,
        }],
        return_type: None,
        body: Some(vec![Statement::Expr(puts_call)]),
        span,
    }
}

/// Builds `fn inspect(self) -> Self self.print(); self end`.
/// Mirrors the default body declared on `Debug.inspect` in
/// `lib/global/src/debug.koja`.
fn inspect_function(span: Span) -> Function {
    let print_call = method_call_no_args(self_expr(span), PRINT_METHOD, span);
    Function {
        annotations: Vec::<Annotation>::new(),
        visibility: Visibility::Public,
        name: INSPECT_METHOD.to_string(),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            local_id: None,
            span,
        }],
        return_type: Some(TypeExpr::Self_ { span }),
        body: Some(vec![
            Statement::Expr(print_call),
            Statement::Expr(self_expr(span)),
        ]),
        span,
    }
}

fn ident_expr(name: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::Ident {
            name: name.to_string(),
            resolution: Resolution::Unresolved,
        },
        span,
    )
}

fn method_call_no_args(receiver: Expr, method: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: method.to_string(),
            args: Vec::<Arg>::new(),
            type_args: Vec::new(),
        },
        span,
    )
}

/// Builds the body for a struct's `format`:
/// `"Name{field1: #{self.field1.format()}, field2: #{self.field2.format()}}"`.
fn struct_format_body(path: &[String], fields: &[StructField], span: Span) -> Expr {
    let surface = path.join(".");
    let mut parts: Vec<StringPart> = Vec::new();
    parts.push(literal_part(format!("{surface}{{"), span));
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            parts.push(literal_part(", ".to_string(), span));
        }
        parts.push(literal_part(format!("{}: ", field.name), span));
        parts.push(field_format_part(&field.name, &field.type_expr, span));
    }
    parts.push(literal_part("}".to_string(), span));
    string_expr(parts, span)
}

/// Returns the interpolation segment for a single struct field.
/// Wraps the field access in `format()` so the result is the field's
/// debug representation. Fields with opaque types render as `"..."`.
fn field_format_part(field_name: &str, field_type: &TypeExpr, span: Span) -> StringPart {
    if is_opaque_type(field_type) {
        return literal_part("...".to_string(), span);
    }
    let field_access = Expr::new(
        ExprKind::FieldAccess {
            receiver: Box::new(self_expr(span)),
            field: field_name.to_string(),
        },
        span,
    );
    interpolation_part(field_access, span)
}

/// Returns `true` for type expressions that can't be safely run
/// through `.format()` in a synthesized body, so the field renders
/// as `"..."`:
///
/// - Compiler-internal recursion-break wrappers (`Indirect`,
///   `Pointer`, `CPtr`).
/// - Anything that isn't a plain named or generic type
///   ([`TypeExpr::Function`], [`TypeExpr::Self_`], [`TypeExpr::Union`],
///   [`TypeExpr::Unit`]): functions / unions / etc. don't carry
///   `format` and there's no syntactic `Self.format()` recursion
///   contract.
///
/// Generic instantiations (`List<Int>`, `Pair<A, B>`, …) are *not*
/// opaque: they pick up either a hand-written stdlib impl or a
/// synthesized impl, so `.format()` always resolves after
/// monomorphization.
///
/// Shared with [`super::derive_equality`]: both synthesizers bail
/// on the same shapes (no `Debug` / `Equality` impl available).
pub(super) fn is_opaque_type(te: &TypeExpr) -> bool {
    match te {
        TypeExpr::Named { path, .. } => matches!(
            path.last().map(String::as_str),
            Some("CPtr") | Some("Indirect") | Some("Pointer")
        ),
        TypeExpr::Generic { path, .. } => matches!(
            path.last().map(String::as_str),
            Some("CPtr") | Some("Indirect") | Some("Pointer")
        ),
        TypeExpr::Function { .. }
        | TypeExpr::Self_ { .. }
        | TypeExpr::Union { .. }
        | TypeExpr::Unit { .. } => true,
    }
}

/// Builds the body for an enum's `format`:
/// `match self <arms> end` where each arm renders one variant.
fn enum_format_body(enum_path: &[String], variants: &[EnumVariant], span: Span) -> Expr {
    let arms = variants
        .iter()
        .map(|v| variant_match_arm(enum_path, v, span))
        .collect();
    Expr::new(
        ExprKind::Match {
            subject: Box::new(self_expr(span)),
            arms,
        },
        span,
    )
}

fn variant_match_arm(enum_path: &[String], variant: &EnumVariant, span: Span) -> MatchArm {
    let type_path = enum_path.to_vec();
    let display = format!("{}.{}", enum_path.join("."), variant.name);
    let (pattern, body_expr) = match &variant.data {
        EnumVariantData::Unit => (
            Pattern::EnumUnit {
                type_path,
                variant: variant.name.clone(),
                span,
            },
            unit_variant_body(&display, span),
        ),
        EnumVariantData::Tuple(types) => {
            let bindings: Vec<String> = (0..types.len()).map(|i| format!("__v{i}")).collect();
            let elements = bindings
                .iter()
                .map(|name| Pattern::Binding {
                    local_id: None,
                    name: name.clone(),
                    span,
                })
                .collect();
            (
                Pattern::EnumTuple {
                    type_path,
                    variant: variant.name.clone(),
                    elements,
                    span,
                },
                tuple_variant_body(&display, &bindings, types, span),
            )
        }
        EnumVariantData::Struct(fields) => {
            let field_patterns = fields
                .iter()
                .map(|f| FieldPattern {
                    name: f.name.clone(),
                    pattern: Pattern::Binding {
                        local_id: None,
                        name: f.name.clone(),
                        span,
                    },
                    span,
                })
                .collect();
            (
                Pattern::EnumStruct {
                    type_path,
                    variant: variant.name.clone(),
                    fields: field_patterns,
                    span,
                },
                struct_variant_body(&display, fields, span),
            )
        }
    };
    MatchArm {
        pattern,
        guard: None,
        body: vec![Statement::Expr(body_expr)],
        span,
    }
}

/// Body for a unit variant: the variant's surface name (`Enum.Variant`)
/// as a literal.
fn unit_variant_body(label: &str, span: Span) -> Expr {
    string_expr(vec![literal_part(label.to_string(), span)], span)
}

/// Body for a tuple variant: `"Enum.Variant(#{__v0.format()}, …)"`.
fn tuple_variant_body(label: &str, bindings: &[String], types: &[TypeExpr], span: Span) -> Expr {
    let mut parts: Vec<StringPart> = Vec::new();
    parts.push(literal_part(format!("{label}("), span));
    for (idx, (binding, ty)) in bindings.iter().zip(types.iter()).enumerate() {
        if idx > 0 {
            parts.push(literal_part(", ".to_string(), span));
        }
        parts.push(binding_format_part(binding, ty, span));
    }
    parts.push(literal_part(")".to_string(), span));
    string_expr(parts, span)
}

/// Body for a struct variant: `"Enum.Variant{f1: #{f1.format()}, …}"`.
fn struct_variant_body(label: &str, fields: &[StructField], span: Span) -> Expr {
    let mut parts: Vec<StringPart> = Vec::new();
    parts.push(literal_part(format!("{label}{{"), span));
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            parts.push(literal_part(", ".to_string(), span));
        }
        parts.push(literal_part(format!("{}: ", field.name), span));
        parts.push(binding_format_part(&field.name, &field.type_expr, span));
    }
    parts.push(literal_part("}".to_string(), span));
    string_expr(parts, span)
}

/// Same as [`field_format_part`] but for an identifier binding (used
/// by match-arm bodies) instead of `self.field`.
fn binding_format_part(name: &str, ty: &TypeExpr, span: Span) -> StringPart {
    if is_opaque_type(ty) {
        return literal_part("...".to_string(), span);
    }
    let ident = Expr::new(
        ExprKind::Ident {
            name: name.to_string(),
            resolution: Resolution::Unresolved,
        },
        span,
    );
    interpolation_part(ident, span)
}

fn self_expr(span: Span) -> Expr {
    Expr::new(ExprKind::Self_ { local_id: None }, span)
}

fn literal_part(value: String, span: Span) -> StringPart {
    StringPart::Literal { value, span }
}

/// Wraps an expression in a `format()` method call before splicing
/// into a string literal. The `.format()` wrap means IR-lower's
/// interpolation handler sees an already-`String`-typed value per
/// part (no per-part type dispatch needed at lower time).
fn interpolation_part(expr: Expr, span: Span) -> StringPart {
    let formatted = Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(expr),
            method: FORMAT_METHOD.to_string(),
            args: Vec::<Arg>::new(),
            type_args: Vec::new(),
        },
        span,
    );
    StringPart::Interpolation {
        expr: Box::new(formatted),
        format: None,
        span,
    }
}

fn string_expr(parts: Vec<StringPart>, span: Span) -> Expr {
    Expr::new(
        ExprKind::String {
            parts,
            multiline: false,
        },
        span,
    )
}
