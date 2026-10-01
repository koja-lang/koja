//! Synthesizes `impl Debug for T` for every user-defined struct or
//! enum that does not already have one. Mutates `file.items` in place
//! by appending the synthetic impl blocks.
//!
//! Synthesized impls are indistinguishable from user-written code, so
//! the rest of typecheck (collect, lift, resolve, seal) needs no
//! special-casing. Runs as a **pre-collect** pass in
//! [`crate::check_program`] so the new items land before name binding.
//! The `for` rewrite in [`crate::pipeline::resolve`] runs on function
//! bodies and cannot introduce items.
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
//! impls live in the same files and [`super::derive_protocol`]
//! detects them.
//!
//! Builtins are never synthesized. The synthesizer cannot know how an
//! opaque type should render, so each builtin carries an explicit impl
//! in the stdlib and a missing one surfaces as a missing conformance.
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
    Annotation, Arg, EnumDecl, EnumVariant, EnumVariantData, Expr, ExprKind, FieldPattern,
    Function, FunctionOrigin, ImplBlock, ImplMember, Item, MatchArm, Name, Param, Pattern,
    Statement, StringPart, StructDecl, StructField, TypeExpr, Visibility, name_texts, path_text,
};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

use crate::program::CheckedPackage;

use super::{ident_expr, named_type, self_expr, self_target_type, synthetic_path};

const DEBUG_PROTOCOL: &str = "Debug";
const FORMAT_METHOD: &str = "format";
const INSPECT_METHOD: &str = "inspect";
const IO_TYPE: &str = "IO";
const PRINT_METHOD: &str = "print";
const PUTS_METHOD: &str = "puts";
const STRING_TYPE: &str = "String";

/// Synthesizes `impl Debug for T` for every struct / enum in `pkg`
/// that does not already have one anywhere in the same package. See
/// [`super::derive_protocol`] for the existing-impl scan.
pub(crate) fn derive_debug_package(pkg: &mut CheckedPackage) {
    super::derive_protocol(
        pkg,
        DEBUG_PROTOCOL,
        synthesize_struct_impl,
        synthesize_enum_impl,
    );
}

fn synthesize_struct_impl(decl: &StructDecl) -> Item {
    let span = decl.span.as_synthetic();
    let target = self_target_type(&decl.path, &decl.type_params, span);
    let format_body = struct_format_body(&decl.path, &decl.fields, span);
    debug_impl_block(target, format_body, span)
}

fn synthesize_enum_impl(decl: &EnumDecl) -> Item {
    let span = decl.span.as_synthetic();
    let target = self_target_type(&decl.path, &decl.type_params, span);
    let format_body = enum_format_body(&decl.path, &decl.variants, span);
    debug_impl_block(target, format_body, span)
}

/// Builds the full `impl Debug for T` block carrying all three
/// methods (`format`, `print`, `inspect`). `format_body` is supplied.
/// `print` and `inspect` come from [`print_function`] /
/// [`inspect_function`] and inline the same bodies the `Debug`
/// protocol declares as defaults in `lib/global/src/debug.koja`.
/// Resolve does not pull protocol default bodies into impls that
/// omit them, so the synthesizer inlines them at synthesis time.
fn debug_impl_block(target: TypeExpr, format_body: Expr, span: Span) -> Item {
    Item::Impl(ImplBlock {
        target,
        target_bounds: Vec::new(),
        trait_expr: debug_trait_expr(span),
        members: vec![
            ImplMember::Function(format_function(format_body, span)),
            ImplMember::Function(print_function(span)),
            ImplMember::Function(inspect_function(span)),
        ],
        span,
        tests: Vec::new(),
    })
}

fn debug_trait_expr(span: Span) -> TypeExpr {
    named_type(DEBUG_PROTOCOL, span)
}

/// Builds `fn format(self) -> String <body> end`.
fn format_function(body_expr: Expr, span: Span) -> Function {
    Function {
        annotations: Vec::<Annotation>::new(),
        origin: FunctionOrigin::Explicit,
        visibility: Visibility::Public,
        name: Name::new(FORMAT_METHOD, span),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            local_id: None,
            span,
        }],
        return_type: Some(named_type(STRING_TYPE, span)),
        error_type: None,
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
            method: Name::new(PUTS_METHOD, span),
            args: vec![Arg {
                name: None,
                value: format_call,
                span,
            }],
            target: Resolution::Unresolved,
            type_args: Vec::new(),
        },
        span,
    );
    Function {
        annotations: Vec::<Annotation>::new(),
        origin: FunctionOrigin::Explicit,
        visibility: Visibility::Public,
        name: Name::new(PRINT_METHOD, span),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            local_id: None,
            span,
        }],
        return_type: None,
        error_type: None,
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
        origin: FunctionOrigin::Explicit,
        visibility: Visibility::Public,
        name: Name::new(INSPECT_METHOD, span),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            local_id: None,
            span,
        }],
        return_type: Some(TypeExpr::Self_ { span }),
        error_type: None,
        body: Some(vec![
            Statement::Expr(print_call),
            Statement::Expr(self_expr(span)),
        ]),
        span,
    }
}

fn method_call_no_args(receiver: Expr, method: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: Name::new(method, span),
            args: Vec::<Arg>::new(),
            target: Resolution::Unresolved,
            type_args: Vec::new(),
        },
        span,
    )
}

/// Builds the body for a struct's `format`:
/// `"Name{field1: #{self.field1.format()}, field2: #{self.field2.format()}}"`.
fn struct_format_body(path: &[Name], fields: &[StructField], span: Span) -> Expr {
    let surface = name_texts(path).join(".");
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
fn field_format_part(field_name: &Name, field_type: &TypeExpr, span: Span) -> StringPart {
    if is_opaque_type(field_type) {
        return literal_part("...".to_string(), span);
    }
    let field_access = Expr::new(
        ExprKind::FieldAccess {
            receiver: Box::new(self_expr(span)),
            field: Name::new(field_name.as_str(), span),
        },
        span,
    );
    interpolation_part(field_access, span)
}

/// Returns `true` for type expressions that cannot be safely run
/// through `.format()` in a synthesized body, so the field renders
/// as `"..."`:
///
/// - Compiler-internal recursion-break wrappers (`Indirect`,
///   `Pointer`, `CPtr`).
/// - Anything that is not a plain named, generic, or tuple type
///   ([`TypeExpr::Function`], [`TypeExpr::Self_`], [`TypeExpr::Union`],
///   [`TypeExpr::Unit`]). Functions / unions / etc. do not carry
///   `format` and there is no syntactic `Self.format()` recursion
///   contract. Tuples conform to `Debug` structurally, so they
///   format like any named field type.
///
/// Generic instantiations (`List<Int>`, `Pair<A, B>`, …) are *not*
/// opaque. They pick up either a hand-written stdlib impl or a
/// synthesized impl, so `.format()` always resolves after
/// monomorphization.
///
/// `Equality` derivation only shares the internal-wrapper carve-out
/// ([`is_internal_wrapper_type`]). Every other field type, functions
/// included, is `Equality`.
fn is_opaque_type(te: &TypeExpr) -> bool {
    match te {
        TypeExpr::Named { .. } | TypeExpr::Generic { .. } => is_internal_wrapper_type(te),
        TypeExpr::Tuple { .. } => false,
        TypeExpr::Function { .. }
        | TypeExpr::Self_ { .. }
        | TypeExpr::Union { .. }
        | TypeExpr::Unit { .. } => true,
    }
}

/// Compiler-internal recursion-break and pointer wrappers (`CPtr`,
/// `Indirect`, `Pointer`) that no derived protocol body should call
/// into. Shared with [`super::derive_equality`].
pub(super) fn is_internal_wrapper_type(te: &TypeExpr) -> bool {
    let (TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. }) = te else {
        return false;
    };
    matches!(
        path.last().map(Name::as_str),
        Some("CPtr") | Some("Indirect") | Some("Pointer")
    )
}

/// Builds the body for an enum's `format`:
/// `match self <arms> end` where each arm renders one variant.
fn enum_format_body(enum_path: &[Name], variants: &[EnumVariant], span: Span) -> Expr {
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

fn variant_match_arm(enum_path: &[Name], variant: &EnumVariant, span: Span) -> MatchArm {
    let type_path = synthetic_path(enum_path, span);
    let variant_name = Name::new(variant.name.as_str(), span);
    let display = format!("{}.{}", path_text(enum_path), variant.name);
    let (pattern, body_expr) = match &variant.data {
        EnumVariantData::Unit => (
            Pattern::EnumUnit {
                type_path,
                type_resolution: Resolution::Unresolved,
                variant: variant_name,
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
                    name: Name::new(name, span),
                    span,
                })
                .collect();
            (
                Pattern::EnumTuple {
                    type_path,
                    type_resolution: Resolution::Unresolved,
                    variant: variant_name,
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
                    name: Name::new(f.name.as_str(), span),
                    pattern: Pattern::Binding {
                        local_id: None,
                        name: Name::new(f.name.as_str(), span),
                        span,
                    },
                    span,
                })
                .collect();
            (
                Pattern::EnumStruct {
                    type_path,
                    type_resolution: Resolution::Unresolved,
                    variant: variant_name,
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

/// The body for a unit variant is the variant's surface name
/// (`Enum.Variant`) as a literal.
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
        parts.push(binding_format_part(
            field.name.as_str(),
            &field.type_expr,
            span,
        ));
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
            method: Name::new(FORMAT_METHOD, span),
            args: Vec::<Arg>::new(),
            target: Resolution::Unresolved,
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
