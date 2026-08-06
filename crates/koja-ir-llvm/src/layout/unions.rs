//! Pre-emit phase for `IRType::Union { mangled, ... }`: build one
//! [`super::UnionLayout`] per [`koja_ir::IRUnionDecl`] on
//! [`super::TypeLayouts`].
//!
//! ## Layout
//!
//! Outer named struct is `{ i64 tag, [M x i64] payload }` where
//! `M = ceil(decl.max_payload_size / 8)`. The tag is the member
//! index widened to a word and the payload is chunked to words so
//! the whole value is 8-aligned and, passed by value, splits into
//! `1 + M` word-sized pieces instead of one piece per byte (see
//! the aggregate-ABI entry in `design/GAPS.md`). Same
//! opaque-then-define two-phase shape used for structs and enums
//! so a union's member can itself be a struct / enum / nested
//! union regardless of decl order. Members do **not** get
//! distinct LLVM types: extraction loads the payload buffer as
//! the member's IRType-derived basic type at use sites.

use inkwell::types::StructType;
use koja_ir::{IRSymbol, IRUnionDecl};

use crate::ctx::EmitContext;
use crate::layout::UnionLayout;

/// Phase 1: mint the opaque struct type and register its layout
/// handle so any later [`crate::types::ir_basic_type`] call (e.g.
/// from a struct body that carries a union-typed field) can hand
/// back the outer type. The body is still opaque at this point.
/// Pointer-shaped uses (alloca / GEP) work fine off the named
/// type alone, and the body lands in [`define_union_body`] before
/// any code that needs to load / store a payload runs.
pub(crate) fn declare_union_type<'ctx>(ctx: &EmitContext<'ctx>, decl: &IRUnionDecl) {
    let outer = ctx.context.opaque_struct_type(decl.symbol.mangled());
    ctx.layouts
        .register_union_layout(decl.symbol.clone(), UnionLayout { outer });
}

/// Phase 2: install the `{ i64 tag, [M x i64] payload }` body on the
/// outer struct minted by [`declare_union_type`].
pub(crate) fn define_union_body<'ctx>(ctx: &EmitContext<'ctx>, decl: &IRUnionDecl) {
    let outer = lookup_named_struct(ctx, &decl.symbol);
    let i64_type = ctx.context.i64_type();
    let payload = i64_type.array_type(decl.max_payload_size.div_ceil(8));
    outer.set_body(&[i64_type.into(), payload.into()], false);
}

fn lookup_named_struct<'ctx>(ctx: &EmitContext<'ctx>, symbol: &IRSymbol) -> StructType<'ctx> {
    ctx.context
        .get_struct_type(symbol.mangled())
        .unwrap_or_else(|| {
            panic!(
                "LLVM emit: union outer struct `{symbol}` not declared \
                 (declare_union_type ordering violation)",
            )
        })
}
