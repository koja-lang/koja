//! Lowering for statement-level decisions: field-path traversal,
//! type-annotation substitution, and coercion resolution. None of these
//! touch LLVM directly -- they thread Expo `Type`s through the typecheck
//! tables so emission can branch on a small, explicit decision shape.

use expo_ast::ast::TypeExpr;
use expo_ast::span::Span;
use expo_typecheck::context::Coercion;
use expo_typecheck::types::{Type, mangle_type, substitute, substitute_preserving};

use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::fields::lower_struct_field;
use crate::lower::types::resolve_type_expr;
use crate::resolved::fields::{ResolvedFieldStep, ResolvedUnionMember};

/// Resolves a dotted field path (`segments[0].segments[1]...`) into the
/// root variable's type plus a sequence of [`ResolvedFieldStep`]s. Each
/// step is decided via [`lower_struct_field`].
///
/// `var_type` looks the root binding up in the surrounding LLVM-bound
/// variables map (which expo-ir cannot reach into directly because that
/// map's value carries `BasicValueEnum<'ctx>`).
pub fn resolve_field_path(
    ctx: &LowerCtx<'_>,
    segments: &[String],
    var_type: impl Fn(&str) -> Option<Type>,
) -> Result<(Type, Vec<ResolvedFieldStep>), String> {
    let variable_name = &segments[0];
    let variable_type =
        var_type(variable_name).ok_or_else(|| format!("undefined variable: {variable_name}"))?;

    let mut current_type = variable_type.clone();
    let mut steps = Vec::with_capacity(segments.len() - 1);

    for field_name in &segments[1..] {
        if !matches!(&current_type, Type::Named { .. }) {
            return Err(format!(
                "cannot access field `{field_name}` on non-struct type"
            ));
        }

        let step = lower_struct_field(ctx, &current_type, field_name).ok_or_else(|| {
            format!(
                "unknown field `{field_name}` on struct `{}`",
                current_type.display()
            )
        })?;

        current_type = step.field_type.clone();
        steps.push(step);
    }

    Ok((variable_type, steps))
}

/// Resolves type-annotation substitutions needed before compiling the RHS
/// of an assignment. Returns `(param_name, type_arg)` pairs to insert into
/// `type_subst` so generic type parameters are available during
/// compilation.
pub fn resolve_annotation_subst(
    ctx: &LowerCtx<'_>,
    type_annotation: &TypeExpr,
) -> Vec<(String, Type)> {
    let annotated = resolve_type_expr(ctx, type_annotation);
    match &annotated {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let Some(type_params) = ctx
                .type_ctx
                .get_type(identifier)
                .map(|type_info| type_info.type_params.clone())
            else {
                return Vec::new();
            };
            type_params
                .iter()
                .zip(type_args.iter())
                .map(|(param, arg)| {
                    let concrete = substitute(arg, &ctx.fn_lower.type_subst);
                    (param.name.clone(), concrete)
                })
                .collect()
        }
        Type::Pointer(inner) => {
            let Some(type_info) = ctx
                .type_ctx
                .resolve_name("CPtr")
                .and_then(|id| ctx.type_ctx.get_type(id))
            else {
                return Vec::new();
            };
            if type_info.type_params.is_empty() {
                return Vec::new();
            }
            vec![(type_info.type_params[0].name.clone(), *inner.clone())]
        }
        _ => Vec::new(),
    }
}

/// Resolves the final annotated type after the RHS has been compiled,
/// substituting generic type args with their concrete bindings.
pub fn resolve_final_annotation_type(ctx: &LowerCtx<'_>, type_annotation: &TypeExpr) -> Type {
    let annotated = resolve_type_expr(ctx, type_annotation);
    match annotated {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let resolved_args: Vec<Type> = type_args
                .iter()
                .map(|t| substitute_preserving(t, &ctx.fn_lower.type_subst))
                .collect();
            Type::Named {
                identifier,
                type_args: resolved_args,
            }
        }
        other => other,
    }
}

/// Looks up a recorded coercion for the given span from the type context.
pub fn resolve_coercion(ctx: &LowerCtx<'_>, span: Span) -> Option<Coercion> {
    ctx.type_ctx.coercions.get(&span).cloned()
}

/// Resolves how to wrap `source` into `target_union`: looks up the
/// source's position within the union's member list (= its
/// discriminant tag) and returns the union's mangled name so emission
/// can `get_monomorphized` its LLVM `StructType`.
pub fn resolve_union_member(
    source: &Type,
    target_union: &Type,
) -> Result<ResolvedUnionMember, String> {
    let Type::Union(members) = target_union else {
        return Err("resolve_union_member called with non-union target".to_string());
    };

    let source_mangled = mangle_type(source);
    let union_mangled = mangle_type(target_union);

    let tag = members
        .iter()
        .position(|m| mangle_type(m) == source_mangled)
        .ok_or_else(|| {
            format!(
                "{} is not a member of union {}",
                source.display(),
                target_union.display()
            )
        })? as u64;

    Ok(ResolvedUnionMember {
        tag,
        union_mangled: MonomorphizedTypeIdentifier::new(union_mangled),
    })
}
