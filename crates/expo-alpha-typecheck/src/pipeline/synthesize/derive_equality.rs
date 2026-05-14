//! Synthesizes `impl Equality for T` for every user-defined struct /
//! enum that doesn't already have one. Mirrors
//! [`super::derive_debug`] — runs **pre-collect**, mutates
//! `file.items` by appending the synthetic impl block.
//!
//! Body shapes:
//!
//! - Struct: `self.f1.eq(other.f1) and self.f2.eq(other.f2) and …`,
//!   or `true` when the struct has no fields. Opaque field types
//!   (mirroring [`super::derive_debug::is_opaque_type`]) are
//!   skipped — same conservative bail as `Debug`'s `"..."`
//!   placeholder.
//! - Enum: nested match. Outer arm dispatches on `self`, inner arm
//!   on `other`; matching variants compare payload-wise, mismatches
//!   fall through to `false`. Unit-only enums collapse to
//!   `match self … _ -> false end`.
//! - Generic types route field / payload `.eq()` calls through the
//!   universal-`Equality` fallback in
//!   [`crate::pipeline::resolve::calls::bounded`] (see
//!   [`crate::registry::UNIVERSAL_PROTOCOLS`]).

use expo_ast::ast::{
    Annotation, Arg, BinOp, EnumDecl, EnumVariant, EnumVariantData, Expr, ExprKind, FieldPattern,
    File, Function, ImplBlock, ImplMember, Item, Literal, MatchArm, Param, PassMode, Pattern,
    Statement, StructDecl, StructField, TypeExpr, TypeParam, Visibility,
};
use expo_ast::identifier::Resolution;
use expo_ast::span::Span;

use crate::program::CheckedPackage;

use super::derive_debug::is_opaque_type;

const BOOL_TYPE: &str = "Bool";
const EQ_METHOD: &str = "eq";
const EQUALITY_PROTOCOL: &str = "Equality";
const OTHER_PARAM: &str = "other";

/// Append `impl Equality for T` for each user struct / enum in `pkg`
/// that doesn't already have one. Existing impls are scanned across
/// the whole package first so a hand-written impl in one file
/// suppresses synthesis in any other file of the same package.
pub(crate) fn derive_equality_package(pkg: &mut CheckedPackage) {
    let existing = collect_package_equality_impls(pkg);
    for file in &mut pkg.files {
        synthesize_into_file(file, &existing);
    }
}

fn collect_package_equality_impls(pkg: &CheckedPackage) -> Vec<String> {
    pkg.files
        .iter()
        .flat_map(collect_existing_equality_impls)
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

fn collect_existing_equality_impls(file: &File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(block) => equality_impl_target(block),
            _ => None,
        })
        .collect()
}

fn equality_impl_target(block: &ImplBlock) -> Option<String> {
    let trait_name = type_expr_head(block.trait_expr.as_ref()?)?;
    if trait_name != EQUALITY_PROTOCOL {
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
    !existing.iter().any(|n| n == &decl.name)
}

/// Empty enums (no variants) are uninhabited — a `match self end`
/// body with no arms is rejected by typecheck, and the type has no
/// value to compare anyway. Skip synthesis.
fn needs_enum_derive(decl: &EnumDecl, existing: &[String]) -> bool {
    !decl.variants.is_empty() && !existing.iter().any(|n| n == &decl.name)
}

fn synthesize_struct_impl(decl: &StructDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.name, &decl.type_params, span);
    let body = struct_eq_body(&decl.fields, span);
    equality_impl_block(target.clone(), body, span)
}

fn synthesize_enum_impl(decl: &EnumDecl) -> Item {
    let span = decl.span;
    let target = self_target_type(&decl.name, &decl.type_params, span);
    let body = enum_eq_body(&decl.name, &decl.variants, span);
    equality_impl_block(target.clone(), body, span)
}

/// Builds `impl Equality for Target<Params> fn eq(...) <body> end`.
/// The `other: Target<Params>` param mirrors the impl target so the
/// signature matches what the `Equality.eq(self, other: Self)`
/// protocol method substitutes to.
fn equality_impl_block(target: TypeExpr, body_expr: Expr, span: Span) -> Item {
    let other_type = target.clone();
    Item::Impl(ImplBlock {
        target,
        trait_expr: Some(equality_trait_expr(span)),
        members: vec![ImplMember::Function(eq_function(
            other_type, body_expr, span,
        ))],
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

fn equality_trait_expr(span: Span) -> TypeExpr {
    named_type(EQUALITY_PROTOCOL, span)
}

fn named_type(name: &str, span: Span) -> TypeExpr {
    TypeExpr::Named {
        path: vec![name.to_string()],
        span,
    }
}

/// Builds `fn eq(self, other: <Target>) -> Bool <body> end`.
fn eq_function(other_type: TypeExpr, body_expr: Expr, span: Span) -> Function {
    Function {
        annotations: Vec::<Annotation>::new(),
        visibility: Visibility::Public,
        name: EQ_METHOD.to_string(),
        type_params: Vec::new(),
        params: vec![
            Param::Self_ {
                mode: PassMode::Borrow,
                local_id: None,
                span,
            },
            Param::Regular {
                mode: PassMode::Borrow,
                name: OTHER_PARAM.to_string(),
                type_expr: other_type,
                default: None,
                local_id: None,
                span,
            },
        ],
        return_type: Some(named_type(BOOL_TYPE, span)),
        body: Some(vec![Statement::Expr(body_expr)]),
        span,
    }
}

/// Conjoins `self.f1.eq(other.f1) and self.f2.eq(other.f2) and …`.
/// Returns `true` for fieldless structs and treats opaque-typed
/// fields (mirroring [`super::derive_debug::is_opaque_type`]) as
/// trivially equal — same conservative skip `Debug` uses with its
/// `"..."` placeholder. A struct that's all-opaque collapses to
/// `true`.
fn struct_eq_body(fields: &[StructField], span: Span) -> Expr {
    let parts: Vec<Expr> = fields
        .iter()
        .filter(|field| !is_opaque_type(&field.type_expr))
        .map(|field| field_eq_call(&field.name, span))
        .collect();
    conjunction(parts, span)
}

/// `self.<name>.eq(other.<name>)` against the matching field on
/// `other`.
fn field_eq_call(name: &str, span: Span) -> Expr {
    let self_field = field_access(self_expr(span), name, span);
    let other_field = field_access(ident_expr(OTHER_PARAM, span), name, span);
    method_call_one_arg(self_field, EQ_METHOD, other_field, span)
}

/// Builds the body for an enum's `eq`: outer `match self` dispatches
/// on the receiver's variant; each arm's body is `match other …`
/// that compares against the same variant and falls through to
/// `false` for any mismatch.
fn enum_eq_body(enum_name: &str, variants: &[EnumVariant], span: Span) -> Expr {
    let arms = variants
        .iter()
        .map(|v| outer_variant_arm(enum_name, v, variants, span))
        .collect();
    match_expr(self_expr(span), arms, span)
}

/// Outer-`match self` arm: bind `self`'s payload under `__l*` names,
/// then nested-`match other` against the same variant for a real
/// comparison, falling through to `_ -> false` for every other
/// variant.
fn outer_variant_arm(
    enum_name: &str,
    variant: &EnumVariant,
    all_variants: &[EnumVariant],
    span: Span,
) -> MatchArm {
    let (pattern, body) = match &variant.data {
        EnumVariantData::Unit => (
            enum_unit_pattern(enum_name, &variant.name, span),
            inner_match_for_unit(enum_name, &variant.name, all_variants, span),
        ),
        EnumVariantData::Tuple(types) => {
            let l_bindings: Vec<String> = (0..types.len()).map(|i| format!("__l{i}")).collect();
            let pattern = enum_tuple_pattern(enum_name, &variant.name, &l_bindings, span);
            let body = inner_match_for_tuple(
                enum_name,
                &variant.name,
                &l_bindings,
                types.len(),
                all_variants,
                span,
            );
            (pattern, body)
        }
        EnumVariantData::Struct(fields) => {
            let pattern = enum_struct_pattern(enum_name, &variant.name, fields, "__l_", span);
            let body = inner_match_for_struct(enum_name, &variant.name, fields, all_variants, span);
            (pattern, body)
        }
    };
    MatchArm {
        pattern,
        guard: None,
        body: vec![Statement::Expr(body)],
        span,
    }
}

fn inner_match_for_unit(
    enum_name: &str,
    variant_name: &str,
    all_variants: &[EnumVariant],
    span: Span,
) -> Expr {
    let arms = vec![
        MatchArm {
            pattern: enum_unit_pattern(enum_name, variant_name, span),
            guard: None,
            body: vec![Statement::Expr(bool_literal(true, span))],
            span,
        },
        wildcard_false_arm(span),
    ];
    fallback_or_match(arms, all_variants, span)
}

fn inner_match_for_tuple(
    enum_name: &str,
    variant_name: &str,
    l_bindings: &[String],
    arity: usize,
    all_variants: &[EnumVariant],
    span: Span,
) -> Expr {
    let r_bindings: Vec<String> = (0..arity).map(|i| format!("__r{i}")).collect();
    let pattern = enum_tuple_pattern(enum_name, variant_name, &r_bindings, span);
    let comparisons: Vec<Expr> = l_bindings
        .iter()
        .zip(r_bindings.iter())
        .map(|(l, r)| {
            method_call_one_arg(ident_expr(l, span), EQ_METHOD, ident_expr(r, span), span)
        })
        .collect();
    let body = conjunction(comparisons, span);
    let arms = vec![
        MatchArm {
            pattern,
            guard: None,
            body: vec![Statement::Expr(body)],
            span,
        },
        wildcard_false_arm(span),
    ];
    fallback_or_match(arms, all_variants, span)
}

fn inner_match_for_struct(
    enum_name: &str,
    variant_name: &str,
    fields: &[StructField],
    all_variants: &[EnumVariant],
    span: Span,
) -> Expr {
    let pattern = enum_struct_pattern(enum_name, variant_name, fields, "__r_", span);
    let comparisons: Vec<Expr> = fields
        .iter()
        .filter(|field| !is_opaque_type(&field.type_expr))
        .map(|field| {
            method_call_one_arg(
                ident_expr(&format!("__l_{}", field.name), span),
                EQ_METHOD,
                ident_expr(&format!("__r_{}", field.name), span),
                span,
            )
        })
        .collect();
    let body = conjunction(comparisons, span);
    let arms = vec![
        MatchArm {
            pattern,
            guard: None,
            body: vec![Statement::Expr(body)],
            span,
        },
        wildcard_false_arm(span),
    ];
    fallback_or_match(arms, all_variants, span)
}

/// Single-variant enums don't need a `_ -> false` arm because the
/// matching arm is exhaustive on its own. Two+-variant enums keep
/// the wildcard so the match stays total.
fn fallback_or_match(arms: Vec<MatchArm>, all_variants: &[EnumVariant], span: Span) -> Expr {
    if all_variants.len() == 1 {
        let mut single = arms;
        single.truncate(1);
        match_expr(ident_expr(OTHER_PARAM, span), single, span)
    } else {
        match_expr(ident_expr(OTHER_PARAM, span), arms, span)
    }
}

fn wildcard_false_arm(span: Span) -> MatchArm {
    MatchArm {
        pattern: Pattern::Wildcard { span },
        guard: None,
        body: vec![Statement::Expr(bool_literal(false, span))],
        span,
    }
}

fn enum_unit_pattern(enum_name: &str, variant_name: &str, span: Span) -> Pattern {
    Pattern::EnumUnit {
        type_path: vec![enum_name.to_string()],
        variant: variant_name.to_string(),
        span,
        resolved_type: None,
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
        resolved_type: None,
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
        resolved_type: None,
    }
}

/// Joins `parts` with `and`. Empty input collapses to `true` so the
/// caller stays total for fieldless structs / unit variants.
fn conjunction(parts: Vec<Expr>, span: Span) -> Expr {
    let mut iter = parts.into_iter();
    let Some(mut acc) = iter.next() else {
        return bool_literal(true, span);
    };
    for next in iter {
        acc = Expr::new(
            ExprKind::Binary {
                op: BinOp::And,
                left: Box::new(acc),
                right: Box::new(next),
            },
            span,
        );
    }
    acc
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

fn self_expr(span: Span) -> Expr {
    Expr::new(ExprKind::Self_ { local_id: None }, span)
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

fn method_call_one_arg(receiver: Expr, method: &str, arg: Expr, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: method.to_string(),
            args: vec![Arg {
                name: None,
                value: arg,
                span,
            }],
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

fn bool_literal(value: bool, span: Span) -> Expr {
    Expr::new(
        ExprKind::Literal {
            value: Literal::Bool(value),
        },
        span,
    )
}
