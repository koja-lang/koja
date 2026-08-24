//! Shared `Option` tag resolution for intrinsic emitters. Tags are
//! resolved by variant name so stdlib declaration order is not baked
//! into emitted code.

use koja_ir::{IRSymbol, IRVariantTag};

use crate::ctx::EmitContext;

/// Tag of `Option.Some` for a monomorphized `Option` symbol.
pub(super) fn some_tag(ctx: &EmitContext<'_>, option_symbol: &IRSymbol) -> IRVariantTag {
    ctx.layouts.enum_variant_tag(option_symbol, "Some")
}

/// Tag of `Option.None`, resolved by name like [`some_tag`].
pub(super) fn none_tag(ctx: &EmitContext<'_>, option_symbol: &IRSymbol) -> IRVariantTag {
    ctx.layouts.enum_variant_tag(option_symbol, "None")
}
