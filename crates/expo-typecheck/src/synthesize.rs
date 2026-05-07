//! Synthesize sub-pass: AST-level rewrites that run before collection.
//!
//! Today the only synthesizer is [`derive_debug`], which appends an
//! `impl Debug for T` block for every user-defined struct / enum that
//! doesn't already have one. The synthesized impls are indistinguishable
//! from user-written code, so the rest of typecheck (collect / resolve /
//! check) needs no special-casing.
//!
//! Runs as the first step inside [`crate::collect_file`]. Mutates the
//! `File` in place. Purely syntactic -- no `TypeContext`, no resolution
//! data is required, so this can run before any name binding has happened.
//!
//! ## Generic types: degraded body
//!
//! Generic types (`Pair<A, B>`, `Option<T>`, `List<T>`, ...) get a
//! `format` body that returns just the type name. Field/payload
//! interpolation would call `.format()` on bare type parameters
//! (`A.format()`), which the typechecker rejects without an
//! `<A: Debug>` bound -- a syntax we don't have yet. Synthesizing the
//! type-name body still gives every generic instantiation a `format`
//! method, which is what other structs' interpolated bodies need when
//! they hold a generic field. Output is degraded (`"List"` instead of
//! `"[1, 2, 3]"`) but the protocol contract is satisfied.
//!
//! ## Planned future synthesizers
//!
//! - `cfg_prune` -- evaluate `@cfg` / `@target` annotations against the
//!   build context and drop items that don't match. Must run before any
//!   `derive_*` so we don't synthesize impls for items that get pruned.
//! - `derive_equality`, `derive_hash`, `derive_ord` -- mechanical
//!   follow-ups once `derive_debug` lands.
//! - `expand_destructuring` -- desugar struct destructuring assignments.
//! - `expand_command` -- desugar the planned `command` construct.

use expo_ast::ast::{
    Annotation, Arg, EnumDecl, EnumVariant, EnumVariantData, Expr, ExprKind, FieldPattern, File,
    Function, ImplBlock, ImplMember, Item, MatchArm, Param, PassMode, Pattern, Statement,
    StringPart, StructDecl, StructField, TypeExpr, TypeParam, Visibility,
};
use expo_ast::identifier::Resolution;
use expo_ast::span::Span;

const DEBUG_PROTOCOL: &str = "Debug";
const FORMAT_METHOD: &str = "format";
const PRINT_METHOD: &str = "print";
const INSPECT_METHOD: &str = "inspect";
const IO_TYPE: &str = "IO";
const PUTS_METHOD: &str = "puts";
const STRING_TYPE: &str = "String";

/// Synthesizes `impl Debug for T` for every struct / enum that doesn't
/// already have one. Mutates `file.items` in place by appending the
/// synthetic impl blocks.
pub(crate) fn derive_debug(file: &mut File) {
    let existing = collect_existing_debug_impls(file);
    let mut synthesized: Vec<Item> = Vec::new();

    for item in &file.items {
        match item {
            Item::Struct(decl) if needs_struct_derive(decl, &existing) => {
                synthesized.push(synthesize_struct_impl(decl));
            }
            Item::Enum(decl) if needs_enum_derive(decl, &existing) => {
                synthesized.push(synthesize_enum_impl(decl));
            }
            _ => {}
        }
    }

    file.items.extend(synthesized);
}

/// Returns the set of type names (e.g. `"User"`) that already have an
/// explicit `impl Debug for T` block in this file. Names are bare --
/// generic args on the target are intentionally ignored so
/// `impl Debug for List<T>` matches a struct named `List`.
fn collect_existing_debug_impls(file: &File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(block) => debug_impl_target(block),
            _ => None,
        })
        .collect()
}

/// If the impl block is `impl Debug for T` (any generic shape), returns
/// `Some(bare_target_name)`; otherwise `None`.
fn debug_impl_target(block: &ImplBlock) -> Option<String> {
    let trait_name = type_expr_head(block.trait_expr.as_ref()?)?;
    if trait_name != DEBUG_PROTOCOL {
        return None;
    }
    type_expr_head(&block.target).map(str::to_string)
}

/// Returns the leading identifier of a type expression: `User`, `List`
/// (from `List<T>`), etc. `None` for `Self`/`Unit`/function/union types.
fn type_expr_head(te: &TypeExpr) -> Option<&str> {
    match te {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
            path.last().map(String::as_str)
        }
        TypeExpr::Self_ { .. } | TypeExpr::Unit { .. } => None,
        TypeExpr::Function { .. } | TypeExpr::Union { .. } => None,
    }
}

fn needs_struct_derive(decl: &StructDecl, existing: &[String]) -> bool {
    !existing.iter().any(|n| n == &decl.name)
}

/// Empty enums (no variants) are uninhabited -- a `match self end` body
/// with no arms is rejected by typecheck, and there's no value to
/// format anyway. Skip them.
fn needs_enum_derive(decl: &EnumDecl, existing: &[String]) -> bool {
    !decl.variants.is_empty() && !existing.iter().any(|n| n == &decl.name)
}

// ----- impl-block construction --------------------------------------------

fn synthesize_struct_impl(decl: &StructDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.name, &decl.type_params, span);
    let format_body = if decl.type_params.is_empty() {
        struct_format_body(&decl.name, &decl.fields, span)
    } else {
        type_name_body(&decl.name, span)
    };
    debug_impl_block(target, format_body, span)
}

fn synthesize_enum_impl(decl: &EnumDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.name, &decl.type_params, span);
    let format_body = if decl.type_params.is_empty() {
        enum_format_body(&decl.name, &decl.variants, span)
    } else {
        type_name_body(&decl.name, span)
    };
    debug_impl_block(target, format_body, span)
}

/// Builds the full `impl Debug for T` block carrying all three methods
/// (`format`, `print`, `inspect`). `format_body` is supplied; `print`
/// and `inspect` come from [`print_function`] / [`inspect_function`]
/// and inline the same bodies the `Debug` protocol declares as
/// defaults in `global/debug.expo`.
fn debug_impl_block(target: TypeExpr, format_body: Expr, span: Span) -> Item {
    Item::Impl(ImplBlock {
        target,
        trait_expr: Some(debug_trait_expr(span)),
        members: vec![
            ImplMember::Function(format_function(format_body, span)),
            ImplMember::Function(print_function(span)),
            ImplMember::Function(inspect_function(span)),
        ],
        span,
    })
}

/// Degraded body for generic types: just a string literal of the type
/// name. Documented in the module-level doc-comment. Unblocks any
/// non-generic struct that holds a generic-typed field.
fn type_name_body(name: &str, span: Span) -> Expr {
    string_expr(vec![literal_part(name.to_string(), span)], span)
}

/// Builds the `Target<Params>` type expression on the `impl ... for`
/// side, mirroring the type's own generic parameters so the impl
/// monomorphizes per concrete instantiation.
fn self_target_type(name: &str, type_params: &[TypeParam], span: Span) -> TypeExpr {
    if type_params.is_empty() {
        named_type(name, span)
    } else {
        let args = type_params
            .iter()
            .map(|tp| named_type(&tp.name, span))
            .collect();
        TypeExpr::Generic {
            path: vec![name.to_string()],
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
            mode: PassMode::Borrow,
            local_id: None,
            span,
        }],
        return_type: Some(named_type(STRING_TYPE, span)),
        body: Some(vec![Statement::Expr(body_expr)]),
        span,
    }
}

/// Builds `fn print(self) IO.puts(self.format()) end`. Mirrors the
/// default body declared on `Debug.print` in `global/debug.expo`.
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
            mode: PassMode::Borrow,
            local_id: None,
            span,
        }],
        return_type: None,
        body: Some(vec![Statement::Expr(puts_call)]),
        span,
    }
}

/// Builds `fn inspect(move self) -> Self self.print(); self end`.
/// Mirrors the default body declared on `Debug.inspect` in
/// `global/debug.expo`.
fn inspect_function(span: Span) -> Function {
    let print_call = method_call_no_args(self_expr(span), PRINT_METHOD, span);
    Function {
        annotations: Vec::<Annotation>::new(),
        visibility: Visibility::Public,
        name: INSPECT_METHOD.to_string(),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            mode: PassMode::Move,
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

// ----- struct format body -------------------------------------------------

/// Builds the body expression for a struct's `format`:
/// `"Name{field1: #{self.field1}, field2: #{self.field2}}"`.
fn struct_format_body(name: &str, fields: &[StructField], span: Span) -> Expr {
    let mut parts: Vec<StringPart> = Vec::new();
    parts.push(literal_part(format!("{name}{{"), span));
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

/// Returns the interpolation segment for a single field. Wraps the
/// field access in `format()` so the result is the field's debug
/// representation. Fields with opaque types (Indirect/Pointer) get a
/// literal `"..."` placeholder to break recursion.
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
/// through `.format()` in a synthesized body, so the field renders as
/// a `"..."` placeholder instead:
///
/// - Compiler-internal recursion-break wrappers (`Indirect`,
///   `Pointer`, `CPtr`).
/// - Stdlib primitives without a `Debug` impl (`Binary`, `Bits`).
/// - Anything that isn't a plain named or generic type
///   ([`TypeExpr::Function`], [`TypeExpr::Self_`], [`TypeExpr::Union`],
///   [`TypeExpr::Unit`]) -- functions/unions/etc. don't carry `format`
///   and there's no syntactic `Self.format()` recursion contract.
///
/// Generic instantiations (`List<Int>`, `Pair<A, B>`, ...) are *not*
/// opaque: every generic struct/enum gets a synthesized
/// type-name-only `format` (see module doc), so calling
/// `self.field.format()` resolves and returns the type name.
fn is_opaque_type(te: &TypeExpr) -> bool {
    match te {
        TypeExpr::Named { path, .. } => matches!(
            path.last().map(String::as_str),
            Some("CPtr") | Some("Indirect") | Some("Pointer") | Some("Binary") | Some("Bits")
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

// ----- enum format body ---------------------------------------------------

/// Builds the body expression for an enum's `format`:
/// `match self <arms> end` where each arm renders one variant.
fn enum_format_body(enum_name: &str, variants: &[EnumVariant], span: Span) -> Expr {
    let arms = variants
        .iter()
        .map(|v| variant_match_arm(enum_name, v, span))
        .collect();
    Expr::new(
        ExprKind::Match {
            subject: Box::new(self_expr(span)),
            arms,
        },
        span,
    )
}

fn variant_match_arm(enum_name: &str, variant: &EnumVariant, span: Span) -> MatchArm {
    let (pattern, body_expr) = match &variant.data {
        EnumVariantData::Unit => (
            Pattern::EnumUnit {
                type_path: vec![enum_name.to_string()],
                variant: variant.name.clone(),
                span,
                resolved_type: None,
            },
            unit_variant_body(&variant.name, span),
        ),
        EnumVariantData::Tuple(types) => {
            let bindings: Vec<String> = (0..types.len()).map(|i| format!("__v{i}")).collect();
            let elements = bindings
                .iter()
                .map(|name| Pattern::Binding {
                    name: name.clone(),
                    span,
                })
                .collect();
            (
                Pattern::EnumTuple {
                    type_path: vec![enum_name.to_string()],
                    variant: variant.name.clone(),
                    elements,
                    span,
                    resolved_type: None,
                },
                tuple_variant_body(&variant.name, &bindings, types, span),
            )
        }
        EnumVariantData::Struct(fields) => {
            let field_patterns = fields
                .iter()
                .map(|f| FieldPattern {
                    name: f.name.clone(),
                    pattern: Pattern::Binding {
                        name: f.name.clone(),
                        span,
                    },
                    span,
                })
                .collect();
            (
                Pattern::EnumStruct {
                    type_path: vec![enum_name.to_string()],
                    variant: variant.name.clone(),
                    fields: field_patterns,
                    span,
                    resolved_type: None,
                },
                struct_variant_body(&variant.name, fields, span),
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

/// Body for a unit variant: just the variant name as a literal.
fn unit_variant_body(variant_name: &str, span: Span) -> Expr {
    string_expr(vec![literal_part(variant_name.to_string(), span)], span)
}

/// Body for a tuple variant: `"Name(#{__v0}, #{__v1})"`.
fn tuple_variant_body(
    variant_name: &str,
    bindings: &[String],
    types: &[TypeExpr],
    span: Span,
) -> Expr {
    let mut parts: Vec<StringPart> = Vec::new();
    parts.push(literal_part(format!("{variant_name}("), span));
    for (idx, (binding, ty)) in bindings.iter().zip(types.iter()).enumerate() {
        if idx > 0 {
            parts.push(literal_part(", ".to_string(), span));
        }
        parts.push(binding_format_part(binding, ty, span));
    }
    parts.push(literal_part(")".to_string(), span));
    string_expr(parts, span)
}

/// Body for a struct variant: `"Name{f1: #{f1}, f2: #{f2}}"`.
fn struct_variant_body(variant_name: &str, fields: &[StructField], span: Span) -> Expr {
    let mut parts: Vec<StringPart> = Vec::new();
    parts.push(literal_part(format!("{variant_name}{{"), span));
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

/// Same as [`field_format_part`] but for an identifier binding (used by
/// match-arm bodies) instead of `self.field`.
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

// ----- low-level AST helpers ----------------------------------------------

fn self_expr(span: Span) -> Expr {
    Expr::new(ExprKind::Self_ { local_id: None }, span)
}

fn literal_part(value: String, span: Span) -> StringPart {
    StringPart::Literal { value, span }
}

/// Wraps an expression in a `format()` method call before splicing into
/// a string literal. Without the explicit `.format()`, codegen's
/// interpolation path would dispatch through `debug::call_format` --
/// but we want this synthesis to depend only on the public `Debug`
/// protocol so the codegen-side helper can be retired.
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

#[cfg(test)]
mod tests {
    use super::*;
    use expo_parser::{ParseMode, parse};

    fn parse_file(source: &str) -> File {
        let result = parse(source, ParseMode::File);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        result.ast
    }

    fn count_impl_debug(file: &File, target: &str) -> usize {
        file.items
            .iter()
            .filter(|item| match item {
                Item::Impl(block) => debug_impl_target(block).as_deref() == Some(target),
                _ => false,
            })
            .count()
    }

    #[test]
    fn struct_with_fields_gets_synthesized_impl() {
        let mut file = parse_file(
            r#"
struct User
  name: String
  age: Int
end
"#,
        );
        derive_debug(&mut file);
        assert_eq!(count_impl_debug(&file, "User"), 1);
    }

    #[test]
    fn empty_generic_struct_gets_type_name_body() {
        // Opaque stdlib-style types like `struct List<T> end` get a
        // degraded format that returns just the type name, so other
        // structs holding `List<T>` fields can still call `.format()`.
        let mut file = parse_file(
            r#"
struct Opaque<T>
end
"#,
        );
        derive_debug(&mut file);
        assert_eq!(count_impl_debug(&file, "Opaque"), 1);
    }

    #[test]
    fn user_impl_takes_precedence() {
        let mut file = parse_file(
            r#"
struct User
  name: String
end

impl Debug for User
  fn format(self) -> String
    "custom"
  end
end
"#,
        );
        derive_debug(&mut file);
        assert_eq!(count_impl_debug(&file, "User"), 1);
    }

    #[test]
    fn enum_with_all_variant_shapes_gets_synthesized_impl() {
        let mut file = parse_file(
            r#"
enum Shape
  Point
  Circle(Int)
  Rect{width: Int, height: Int}
end
"#,
        );
        derive_debug(&mut file);
        assert_eq!(count_impl_debug(&file, "Shape"), 1);
    }

    #[test]
    fn empty_enum_is_skipped() {
        let mut file = parse_file(
            r#"
enum Empty<T>
end
"#,
        );
        derive_debug(&mut file);
        assert_eq!(count_impl_debug(&file, "Empty"), 0);
    }

    #[test]
    fn generic_struct_with_fields_uses_type_name_body() {
        // Field interpolation would call `T.format()`, which the
        // typechecker rejects without a `T: Debug` bound. Until
        // impl-level bounds land, generics fall back to the
        // type-name-only body so callers still see a `format` method.
        let mut file = parse_file(
            r#"
struct Pair<A, B>
  first: A
  second: B
end
"#,
        );
        derive_debug(&mut file);
        assert_eq!(count_impl_debug(&file, "Pair"), 1);
    }
}
