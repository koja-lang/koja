//! Synthesizes `impl Equality for T` for every user-defined struct /
//! enum that doesn't already have one. Mirrors
//! [`super::derive_debug`]: runs **pre-collect**, mutates
//! `file.items` by appending the synthetic impl block.
//!
//! Body shapes:
//!
//! - Struct: `self.f1.equals?(other.f1) and self.f2.equals?(other.f2) and …`,
//!   or `true` when the struct has no fields. Only the compiler-internal
//!   wrappers ([`super::derive_debug::is_internal_wrapper_type`]) are
//!   skipped. Every other field is compared: `Equality` is total, so
//!   functions and unions compare like any other value.
//! - Enum: nested match. Outer arm dispatches on `self`, inner arm
//!   on `other`. Matching variants compare payload-wise, mismatches
//!   fall through to `false`. Unit-only enums collapse to
//!   `match self … _ -> false end`.
//! - Generic types route field / payload `.equals?()` calls through the
//!   universal-`Equality` fallback in
//!   [`crate::pipeline::resolve::calls::bounded`] (see
//!   [`crate::registry::UNIVERSAL_PROTOCOLS`]).
//!
//! Builtins are never synthesized. The synthesizer cannot know what
//! equality means for an opaque type (a zero-field body once made every
//! `CPtr` pair compare equal), so each builtin carries an explicit impl
//! in the stdlib and a missing one surfaces as a missing conformance.

use koja_ast::ast::{
    Annotation, Arg, BinOp, EnumDecl, EnumVariant, EnumVariantData, Expr, ExprKind, FieldPattern,
    Function, FunctionOrigin, ImplBlock, ImplMember, Item, Literal, MatchArm, Name, Param, Pattern,
    Statement, StructDecl, StructField, TypeExpr, TypeParam, Visibility,
};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

use crate::program::CheckedPackage;

use super::derive_debug::is_internal_wrapper_type;
use super::{ident_expr, named_type, self_expr, self_target_type, synthetic_path};

const BOOL_TYPE: &str = "Bool";
const EQ_METHOD: &str = "equals?";
const EQUALITY_PROTOCOL: &str = "Equality";
const OTHER_PARAM: &str = "other";

/// Append `impl Equality for T` for each user struct / enum in `pkg`
/// that doesn't already have one. See [`super::derive_protocol`]
/// for the existing-impl scan.
pub(crate) fn derive_equality_package(pkg: &mut CheckedPackage) {
    super::derive_protocol(
        pkg,
        EQUALITY_PROTOCOL,
        synthesize_struct_impl,
        synthesize_enum_impl,
    );
}

fn synthesize_struct_impl(decl: &StructDecl) -> Item {
    let span = decl.span.as_synthetic();
    let body = struct_eq_body(&decl.fields, span);
    equality_impl_block(&decl.path, &decl.type_params, body, span)
}

fn synthesize_enum_impl(decl: &EnumDecl) -> Item {
    let span = decl.span.as_synthetic();
    let body = enum_eq_body(&decl.path, &decl.variants, span);
    equality_impl_block(&decl.path, &decl.type_params, body, span)
}

/// Builds `impl Equality for Target<Params> fn equals?(...) <body> end`.
/// The impl is unconditional because `Equality` is total. The `other:
/// Target<Params>` param mirrors the impl target so the signature
/// matches what the `Equality.equals?(self, other: Self)` protocol
/// method substitutes to.
fn equality_impl_block(
    path: &[Name],
    type_params: &[TypeParam],
    body_expr: Expr,
    span: Span,
) -> Item {
    let target = self_target_type(path, type_params, span);
    let other_type = target.clone();
    Item::Impl(ImplBlock {
        target,
        target_bounds: Vec::new(),
        trait_expr: equality_trait_expr(span),
        members: vec![ImplMember::Function(eq_function(
            other_type, body_expr, span,
        ))],
        span,
        tests: Vec::new(),
    })
}

fn equality_trait_expr(span: Span) -> TypeExpr {
    named_type(EQUALITY_PROTOCOL, span)
}

/// Builds `fn equals?(self, other: <Target>) -> Bool <body> end`.
fn eq_function(other_type: TypeExpr, body_expr: Expr, span: Span) -> Function {
    Function {
        annotations: Vec::<Annotation>::new(),
        origin: FunctionOrigin::Explicit,
        visibility: Visibility::Public,
        name: Name::new(EQ_METHOD, span),
        type_params: Vec::new(),
        params: vec![
            Param::Self_ {
                local_id: None,
                span,
            },
            Param::Regular {
                name: Name::new(OTHER_PARAM, span),
                type_expr: other_type,
                default: None,
                local_id: None,
                span,
            },
        ],
        return_type: Some(named_type(BOOL_TYPE, span)),
        error_type: None,
        body: Some(vec![Statement::Expr(body_expr)]),
        span,
    }
}

/// Conjoins `self.f1.equals?(other.f1) and self.f2.equals?(other.f2) and …`.
/// Returns `true` for fieldless structs. Compiler-internal wrapper
/// fields are skipped and treated as trivially equal.
fn struct_eq_body(fields: &[StructField], span: Span) -> Expr {
    let parts: Vec<Expr> = fields
        .iter()
        .filter(|field| !is_internal_wrapper_type(&field.type_expr))
        .map(|field| field_eq_call(&field.name, span))
        .collect();
    conjunction(parts, span)
}

/// `self.<name>.equals?(other.<name>)` against the matching field on
/// `other`.
fn field_eq_call(name: &Name, span: Span) -> Expr {
    let self_field = field_access(self_expr(span), name.as_str(), span);
    let other_field = field_access(ident_expr(OTHER_PARAM, span), name.as_str(), span);
    method_call_one_arg(self_field, EQ_METHOD, other_field, span)
}

/// Builds the body for an enum's `equals?`: outer `match self` dispatches
/// on the receiver's variant. Each arm's body is `match other …`
/// that compares against the same variant and falls through to
/// `false` for any mismatch.
fn enum_eq_body(enum_path: &[Name], variants: &[EnumVariant], span: Span) -> Expr {
    let arms = variants
        .iter()
        .map(|v| outer_variant_arm(enum_path, v, variants, span))
        .collect();
    match_expr(self_expr(span), arms, span)
}

/// Outer-`match self` arm: bind `self`'s payload under `__l*` names,
/// then nested-`match other` against the same variant for a real
/// comparison, falling through to `_ -> false` for every other
/// variant.
fn outer_variant_arm(
    enum_path: &[Name],
    variant: &EnumVariant,
    all_variants: &[EnumVariant],
    span: Span,
) -> MatchArm {
    let (pattern, body) = match &variant.data {
        EnumVariantData::Unit => (
            enum_unit_pattern(enum_path, &variant.name, span),
            inner_match_for_unit(enum_path, &variant.name, all_variants, span),
        ),
        EnumVariantData::Tuple(types) => {
            let l_bindings: Vec<String> = (0..types.len()).map(|i| format!("__l{i}")).collect();
            let pattern = enum_tuple_pattern(enum_path, &variant.name, &l_bindings, span);
            let body = inner_match_for_tuple(
                enum_path,
                &variant.name,
                &l_bindings,
                types.len(),
                all_variants,
                span,
            );
            (pattern, body)
        }
        EnumVariantData::Struct(fields) => {
            let pattern = enum_struct_pattern(enum_path, &variant.name, fields, "__l_", span);
            let body = inner_match_for_struct(enum_path, &variant.name, fields, all_variants, span);
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
    enum_path: &[Name],
    variant_name: &Name,
    all_variants: &[EnumVariant],
    span: Span,
) -> Expr {
    let arms = vec![
        MatchArm {
            pattern: enum_unit_pattern(enum_path, variant_name, span),
            guard: None,
            body: vec![Statement::Expr(bool_literal(true, span))],
            span,
        },
        wildcard_false_arm(span),
    ];
    fallback_or_match(arms, all_variants, span)
}

fn inner_match_for_tuple(
    enum_path: &[Name],
    variant_name: &Name,
    l_bindings: &[String],
    arity: usize,
    all_variants: &[EnumVariant],
    span: Span,
) -> Expr {
    let r_bindings: Vec<String> = (0..arity).map(|i| format!("__r{i}")).collect();
    let pattern = enum_tuple_pattern(enum_path, variant_name, &r_bindings, span);
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
    enum_path: &[Name],
    variant_name: &Name,
    fields: &[StructField],
    all_variants: &[EnumVariant],
    span: Span,
) -> Expr {
    let pattern = enum_struct_pattern(enum_path, variant_name, fields, "__r_", span);
    let comparisons: Vec<Expr> = fields
        .iter()
        .filter(|field| !is_internal_wrapper_type(&field.type_expr))
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

fn enum_unit_pattern(enum_path: &[Name], variant_name: &Name, span: Span) -> Pattern {
    Pattern::EnumUnit {
        type_path: synthetic_path(enum_path, span),
        type_resolution: Resolution::Unresolved,
        variant: Name::new(variant_name.as_str(), span),
        span,
    }
}

fn enum_tuple_pattern(
    enum_path: &[Name],
    variant_name: &Name,
    bindings: &[String],
    span: Span,
) -> Pattern {
    let elements = bindings
        .iter()
        .map(|name| Pattern::Binding {
            local_id: None,
            name: Name::new(name, span),
            span,
        })
        .collect();
    Pattern::EnumTuple {
        type_path: synthetic_path(enum_path, span),
        type_resolution: Resolution::Unresolved,
        variant: Name::new(variant_name.as_str(), span),
        elements,
        span,
    }
}

fn enum_struct_pattern(
    enum_path: &[Name],
    variant_name: &Name,
    fields: &[StructField],
    binding_prefix: &str,
    span: Span,
) -> Pattern {
    let field_patterns = fields
        .iter()
        .map(|f| FieldPattern {
            name: Name::new(f.name.as_str(), span),
            pattern: Pattern::Binding {
                local_id: None,
                name: Name::new(format!("{binding_prefix}{}", f.name), span),
                span,
            },
            span,
        })
        .collect();
    Pattern::EnumStruct {
        type_path: synthetic_path(enum_path, span),
        type_resolution: Resolution::Unresolved,
        variant: Name::new(variant_name.as_str(), span),
        fields: field_patterns,
        span,
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

fn field_access(receiver: Expr, field: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::FieldAccess {
            receiver: Box::new(receiver),
            field: Name::new(field, span),
        },
        span,
    )
}

fn method_call_one_arg(receiver: Expr, method: &str, arg: Expr, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: Name::new(method, span),
            args: vec![Arg {
                name: None,
                value: arg,
                span,
            }],
            target: Resolution::Unresolved,
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
