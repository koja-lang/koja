//! Pre-emit phase for `IRType::Union { mangled, ... }`: build one
//! [`super::UnionLayout`] per [`expo_ir::IRUnionDecl`] on
//! [`super::TypeLayouts`].
//!
//! ## Layout
//!
//! Outer named struct is `{ i8 tag, [N x i8] payload }` where `N
//! = decl.max_payload_size`. Same opaque-then-define two-phase
//! shape used for structs and enums so a union's member can
//! itself be a struct / enum / nested union regardless of decl
//! order. Members do **not** get distinct LLVM types — extraction
//! emits a `bitcast` from the payload buffer to the member's
//! IRType-derived basic type at use sites.

use expo_ir::{IRSymbol, IRUnionDecl};
use inkwell::types::StructType;

use crate::ctx::EmitContext;
use crate::layout::UnionLayout;

/// Phase 1: mint the opaque struct type and register its layout
/// handle so any later [`crate::types::ir_basic_type`] call (e.g.
/// from a struct body that carries a union-typed field) can hand
/// back the outer type. The body is still opaque at this point
/// — pointer-shaped uses (alloca / GEP) work fine off the named
/// type alone, and the body lands in [`define_union_body`] before
/// any code that needs to load / store a payload runs.
pub(crate) fn declare_union_type<'ctx>(ctx: &EmitContext<'ctx>, decl: &IRUnionDecl) {
    let outer = ctx.context.opaque_struct_type(decl.symbol.mangled());
    ctx.layouts.register_union_layout(
        decl.symbol.clone(),
        UnionLayout {
            outer,
            payload_size: decl.max_payload_size,
        },
    );
}

/// Phase 2: install the `{ i8 tag, [N x i8] payload }` body on the
/// outer struct minted by [`declare_union_type`].
pub(crate) fn define_union_body<'ctx>(ctx: &EmitContext<'ctx>, decl: &IRUnionDecl) {
    let outer = lookup_named_struct(ctx, &decl.symbol);
    let i8_type = ctx.context.i8_type();
    let payload = i8_type.array_type(decl.max_payload_size);
    outer.set_body(&[i8_type.into(), payload.into()], false);
}

fn lookup_named_struct<'ctx>(ctx: &EmitContext<'ctx>, symbol: &IRSymbol) -> StructType<'ctx> {
    ctx.context
        .get_struct_type(symbol.mangled())
        .unwrap_or_else(|| {
            panic!(
                "LLVM emit: union outer struct `{symbol}` not declared — \
                 declare_union_type ordering violation",
            )
        })
}
