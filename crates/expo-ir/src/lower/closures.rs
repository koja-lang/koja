//! Closure metadata lookup. Captures and effective signatures collected
//! during typecheck are keyed by `(source path, span)`; the source path is
//! threaded through [`LowerCtx::closure_site_path`] so merged graphs from
//! multiple modules don't collide on identical line/column positions.
//!
//! Lifted off `Compiler` in Wave 6. Single-function module today; future
//! closure-lowering helpers will land here.

use expo_ast::ast::ClosureParam;
use expo_ast::span::Span;
use expo_typecheck::context::ClosureInfo;
use expo_typecheck::types::{Primitive, Type};

use crate::lower::ctx::LowerCtx;
use crate::lower::types::resolve_type_expr;

/// Look up the typecheck-collected [`ClosureInfo`] for the closure at
/// `span` in the current source module.
pub fn closure_info_at<'a>(ctx: &LowerCtx<'a>, span: Span) -> Option<&'a ClosureInfo> {
    ctx.type_ctx
        .closure_info
        .get(&(ctx.closure_site_path.map(|p| p.to_path_buf()), span))
}

/// Resolves the parameter types of a closure literal. When all parameters
/// are annotated, types come straight from the annotations. Otherwise the
/// typecheck-collected closure info supplies inferred types; missing
/// annotations on individual parameters fall back to `Int32`.
pub fn resolve_closure_params(
    ctx: &LowerCtx<'_>,
    params: &[ClosureParam],
    span: Span,
) -> Vec<Type> {
    let all_annotated = params.iter().all(|p| {
        matches!(
            p,
            ClosureParam::Name {
                type_expr: Some(_),
                ..
            }
        )
    });

    if all_annotated {
        return params
            .iter()
            .map(|p| match p {
                ClosureParam::Name {
                    type_expr: Some(type_expr),
                    ..
                } => resolve_type_expr(ctx, type_expr),
                _ => unreachable!(),
            })
            .collect();
    }

    if let Some(closure_info) = closure_info_at(ctx, span) {
        return closure_info.param_types.clone();
    }

    params
        .iter()
        .map(|p| match p {
            ClosureParam::Name {
                type_expr: Some(type_expr),
                ..
            } => resolve_type_expr(ctx, type_expr),
            _ => Type::Primitive(Primitive::I32),
        })
        .collect()
}
