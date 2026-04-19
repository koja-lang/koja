//! Closure metadata lookup. Captures and effective signatures collected
//! during typecheck are keyed by `(source path, span)`; the source path is
//! threaded through [`LowerCtx::closure_site_path`] so merged graphs from
//! multiple modules don't collide on identical line/column positions.
//!
//! Lifted off `Compiler` in Wave 6. Single-function module today; future
//! closure-lowering helpers will land here.

use expo_ast::span::Span;
use expo_typecheck::context::ClosureInfo;

use crate::lower::ctx::LowerCtx;

/// Look up the typecheck-collected [`ClosureInfo`] for the closure at
/// `span` in the current source module.
pub fn closure_info_at<'a>(ctx: &LowerCtx<'a>, span: Span) -> Option<&'a ClosureInfo> {
    ctx.type_ctx
        .closure_info
        .get(&(ctx.closure_site_path.map(|p| p.to_path_buf()), span))
}
