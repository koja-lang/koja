//! Synthesizes `impl Clone for T` for every user-defined struct /
//! enum that doesn't already have one. Mirrors
//! [`super::derive_equality`] / [`super::derive_debug`] — runs
//! **pre-collect**, mutates `file.items` by appending the synthetic
//! impl block.
//!
//! Body shapes:
//!
//! - Struct: `Name{f1: self.f1.clone(), f2: self.f2.clone(), …}`,
//!   or `Name{}` when the struct has no fields. Passthrough-typed
//!   fields ([`is_clone_passthrough`]) emit `self.f` unchanged —
//!   their types are pointer-shaped Copy or otherwise have no
//!   meaningful deep clone (function pointers, unions, unit).
//! - Enum: `match self <arms> end` where each arm reconstructs the
//!   same variant with each payload cloned (passthrough for opaque
//!   payloads).
//! - Generic types route field / payload `.clone()` calls through
//!   the universal-`Clone` fallback in
//!   [`crate::pipeline::resolve::calls::bounded`] (see
//!   [`crate::registry::UNIVERSAL_PROTOCOLS`]).

use koja_ast::ast::{
    Annotation, Arg, EnumConstructionData, EnumDecl, EnumVariant, EnumVariantData, Expr, ExprKind,
    FieldInit, FieldPattern, File, Function, ImplBlock, ImplMember, Item, MatchArm, Param,
    PassMode, Pattern, Statement, StructDecl, StructField, TypeExpr, TypeParam, Visibility,
};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

use crate::program::CheckedPackage;

const CLONE_METHOD: &str = "clone";
const CLONE_PROTOCOL: &str = "Clone";

/// Struct decls that the synthesizer can't auto-derive a `Clone` impl
/// for because the target type rejects struct-literal construction
/// (see [`koja-typecheck::pipeline::resolve::structs::is_unconstructable_primitive`]).
/// Of the unconstructable primitives the only one with a `.koja` decl
/// — i.e. the only one this loop sees — is `CPtr`. The hand-written
/// `impl Clone for CPtr<T>` in `cptr.koja` fills the gap with a
/// `self` passthrough since `CPtr<T>` is pointer-shaped Copy.
const SKIP_STRUCT_DERIVES: &[&str] = &["CPtr"];

/// Append `impl Clone for T` for each user struct / enum in `pkg`
/// that doesn't already have one. Existing impls are scanned across
/// the whole package first so a hand-written impl in one file
/// suppresses synthesis in any other file of the same package.
pub(crate) fn derive_clone_package(pkg: &mut CheckedPackage) {
    let existing = collect_package_clone_impls(pkg);
    for file in &mut pkg.files {
        synthesize_into_file(file, &existing);
    }
}

fn collect_package_clone_impls(pkg: &CheckedPackage) -> Vec<String> {
    pkg.files
        .iter()
        .flat_map(collect_existing_clone_impls)
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

fn collect_existing_clone_impls(file: &File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(block) => clone_impl_target(block),
            _ => None,
        })
        .collect()
}

fn clone_impl_target(block: &ImplBlock) -> Option<String> {
    let trait_name = type_expr_head(&block.trait_expr)?;
    if trait_name != CLONE_PROTOCOL {
        return None;
    }
    type_expr_head(&block.target).map(str::to_string)
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
    if SKIP_STRUCT_DERIVES.contains(&decl.name.as_str()) {
        return false;
    }
    !existing.iter().any(|n| n == &decl.name)
}

/// Empty enums (no variants) are uninhabited — a `match self end`
/// body with no arms is rejected by typecheck, and the type has no
/// value to clone anyway. Skip synthesis.
fn needs_enum_derive(decl: &EnumDecl, existing: &[String]) -> bool {
    !decl.variants.is_empty() && !existing.iter().any(|n| n == &decl.name)
}

fn synthesize_struct_impl(decl: &StructDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.name, &decl.type_params, span);
    let body = struct_clone_body(&decl.name, &decl.fields, span);
    clone_impl_block(target, body, span)
}

fn synthesize_enum_impl(decl: &EnumDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.name, &decl.type_params, span);
    let body = enum_clone_body(&decl.name, &decl.variants, span);
    clone_impl_block(target, body, span)
}

/// Builds `impl Clone for Target<Params> fn clone(self) -> Self <body> end`.
fn clone_impl_block(target: TypeExpr, body_expr: Expr, span: Span) -> Item {
    Item::Impl(ImplBlock {
        target,
        trait_expr: clone_trait_expr(span),
        members: vec![ImplMember::Function(clone_function(body_expr, span))],
        span,
    })
}

/// Mirrors the type's own generic params on the impl target so the
/// impl monomorphizes alongside the type.
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

fn clone_trait_expr(span: Span) -> TypeExpr {
    named_type(CLONE_PROTOCOL, span)
}

fn named_type(name: &str, span: Span) -> TypeExpr {
    TypeExpr::Named {
        path: vec![name.to_string()],
        span,
    }
}

/// Builds `fn clone(self) -> Self <body> end`. `self` borrows
/// (Clone is non-consuming by design); the result is a fresh owned
/// value.
fn clone_function(body_expr: Expr, span: Span) -> Function {
    Function {
        annotations: Vec::<Annotation>::new(),
        visibility: Visibility::Public,
        name: CLONE_METHOD.to_string(),
        type_params: Vec::new(),
        params: vec![Param::Self_ {
            mode: PassMode::Borrow,
            local_id: None,
            span,
        }],
        return_type: Some(TypeExpr::Self_ { span }),
        body: Some(vec![Statement::Expr(body_expr)]),
        span,
    }
}

/// Builds the body for a struct's `clone`:
/// `Name{f1: self.f1.clone(), f2: self.f2.clone(), …}`. Passthrough
/// fields ([`is_clone_passthrough`]) keep the raw `self.f` —
/// pointer-shaped Copy types and unclonable shapes (functions,
/// unions, unit) don't need a `.clone()` call.
fn struct_clone_body(name: &str, fields: &[StructField], span: Span) -> Expr {
    let field_inits = fields
        .iter()
        .map(|field| FieldInit {
            name: field.name.clone(),
            value: clone_field_value(&field.name, &field.type_expr, span),
            span,
        })
        .collect();
    Expr::new(
        ExprKind::StructConstruction {
            type_path: vec![name.to_string()],
            fields: field_inits,
        },
        span,
    )
}

fn clone_field_value(field_name: &str, field_type: &TypeExpr, span: Span) -> Expr {
    let receiver = field_access(self_expr(span), field_name, span);
    if is_clone_passthrough(field_type) {
        return receiver;
    }
    method_call_no_args(receiver, CLONE_METHOD, span)
}

/// Builds the body for an enum's `clone`:
/// `match self <arms> end` where each arm rebuilds the receiver's
/// variant with cloned payloads.
fn enum_clone_body(enum_name: &str, variants: &[EnumVariant], span: Span) -> Expr {
    let arms = variants
        .iter()
        .map(|v| variant_clone_arm(enum_name, v, span))
        .collect();
    match_expr(self_expr(span), arms, span)
}

fn variant_clone_arm(enum_name: &str, variant: &EnumVariant, span: Span) -> MatchArm {
    let (pattern, body_expr) = match &variant.data {
        EnumVariantData::Unit => (
            enum_unit_pattern(enum_name, &variant.name, span),
            unit_variant_construction(enum_name, &variant.name, span),
        ),
        EnumVariantData::Tuple(types) => {
            let bindings: Vec<String> = (0..types.len()).map(|i| format!("__l{i}")).collect();
            let pattern = enum_tuple_pattern(enum_name, &variant.name, &bindings, span);
            let body = tuple_variant_construction(enum_name, &variant.name, &bindings, types, span);
            (pattern, body)
        }
        EnumVariantData::Struct(fields) => {
            let pattern = enum_struct_pattern(enum_name, &variant.name, fields, "__l_", span);
            let body = struct_variant_construction(enum_name, &variant.name, fields, span);
            (pattern, body)
        }
    };
    MatchArm {
        pattern,
        guard: None,
        body: vec![Statement::Expr(body_expr)],
        span,
    }
}

fn unit_variant_construction(enum_name: &str, variant_name: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::EnumConstruction {
            type_path: vec![enum_name.to_string()],
            variant: variant_name.to_string(),
            data: EnumConstructionData::Unit,
        },
        span,
    )
}

fn tuple_variant_construction(
    enum_name: &str,
    variant_name: &str,
    bindings: &[String],
    types: &[TypeExpr],
    span: Span,
) -> Expr {
    let elements = bindings
        .iter()
        .zip(types.iter())
        .map(|(name, ty)| clone_binding_value(name, ty, span))
        .collect();
    Expr::new(
        ExprKind::EnumConstruction {
            type_path: vec![enum_name.to_string()],
            variant: variant_name.to_string(),
            data: EnumConstructionData::Tuple(elements),
        },
        span,
    )
}

fn struct_variant_construction(
    enum_name: &str,
    variant_name: &str,
    fields: &[StructField],
    span: Span,
) -> Expr {
    let field_inits = fields
        .iter()
        .map(|field| FieldInit {
            name: field.name.clone(),
            value: clone_binding_value(&format!("__l_{}", field.name), &field.type_expr, span),
            span,
        })
        .collect();
    Expr::new(
        ExprKind::EnumConstruction {
            type_path: vec![enum_name.to_string()],
            variant: variant_name.to_string(),
            data: EnumConstructionData::Struct(field_inits),
        },
        span,
    )
}

fn clone_binding_value(binding_name: &str, ty: &TypeExpr, span: Span) -> Expr {
    let receiver = ident_expr(binding_name, span);
    if is_clone_passthrough(ty) {
        return receiver;
    }
    method_call_no_args(receiver, CLONE_METHOD, span)
}

/// Returns `true` for type expressions that don't need (and can't
/// run) a `.clone()` call in the synthesized body — the field /
/// payload value is emitted unchanged:
///
/// - Compiler-internal pointer-shaped types (`CPtr`, `Indirect`,
///   `Pointer`) — Copy-by-value at the LLVM ABI; cloning them just
///   means passing the same pointer along.
/// - [`TypeExpr::Function`] — function pointers are Copy.
/// - [`TypeExpr::Self_`] — recursing into a self-typed field
///   would loop the synthesized impl; bail to passthrough. Real
///   recursive types use `Indirect<T>` instead, which is also
///   passthrough.
/// - [`TypeExpr::Union`] — no syntactic `.clone()` contract on a
///   bare union; field-flowed clone goes through whatever Clone
///   impl the member type provides at construction time.
/// - [`TypeExpr::Unit`] — unit is Copy.
///
/// Differs from [`super::derive_debug::is_opaque_type`] by *not*
/// listing `Binary` / `Bits`: both have hand-written `Clone` impls
/// in [`koja/lib/global/src/clone.koja`], so the universal `.clone()`
/// call works on them. They're rendered as `"..."` for Debug
/// (no impl yet) but cloned for real here.
fn is_clone_passthrough(te: &TypeExpr) -> bool {
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

fn enum_unit_pattern(enum_name: &str, variant_name: &str, span: Span) -> Pattern {
    Pattern::EnumUnit {
        type_path: vec![enum_name.to_string()],
        variant: variant_name.to_string(),
        span,
    }
}

fn enum_tuple_pattern(
    enum_name: &str,
    variant_name: &str,
    bindings: &[String],
    span: Span,
) -> Pattern {
    let elements = bindings
        .iter()
        .map(|name| Pattern::Binding {
            local_id: None,
            name: name.clone(),
            span,
        })
        .collect();
    Pattern::EnumTuple {
        type_path: vec![enum_name.to_string()],
        variant: variant_name.to_string(),
        elements,
        span,
    }
}

fn enum_struct_pattern(
    enum_name: &str,
    variant_name: &str,
    fields: &[StructField],
    binding_prefix: &str,
    span: Span,
) -> Pattern {
    let field_patterns = fields
        .iter()
        .map(|f| FieldPattern {
            name: f.name.clone(),
            pattern: Pattern::Binding {
                local_id: None,
                name: format!("{binding_prefix}{}", f.name),
                span,
            },
            span,
        })
        .collect();
    Pattern::EnumStruct {
        type_path: vec![enum_name.to_string()],
        variant: variant_name.to_string(),
        fields: field_patterns,
        span,
    }
}

fn self_expr(span: Span) -> Expr {
    Expr::new(ExprKind::Self_ { local_id: None }, span)
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

fn field_access(receiver: Expr, field: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::FieldAccess {
            receiver: Box::new(receiver),
            field: field.to_string(),
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

fn match_expr(subject: Expr, arms: Vec<MatchArm>, span: Span) -> Expr {
    Expr::new(
        ExprKind::Match {
            subject: Box::new(subject),
            arms,
        },
        span,
    )
}
