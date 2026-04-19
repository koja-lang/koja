//! Borrow bundle for lowering functions. Holds all read-only context that
//! semantic decision-making needs (typecheck output, current package,
//! per-function semantic state, current closure site).
//!
//! Constructed on demand from `expo-codegen`'s `Compiler::lower_ctx()` and
//! passed by reference to every free function in [`crate::lower`]. Lifetimes
//! flow through the borrow: `Option<&TypeIdentifier>` results from
//! `resolve_name_current` borrow from `ctx.type_ctx`, which in turn borrows
//! from the owning `Compiler`. Same effective lifetime as the inherent
//! methods this replaces.
//!
//! Future waves can grow this struct (add `&LLVMTypeCache` for an emit-side
//! ctx, or `&TypeLayouts` for layout-aware lowering) without rewriting
//! call sites — the bundle is the single point of accretion.

use std::path::Path;

use expo_typecheck::context::TypeContext;
use expo_typecheck::types::Package;

use crate::FnLowerState;

pub struct LowerCtx<'a> {
    pub closure_site_path: Option<&'a Path>,
    pub fn_lower: &'a FnLowerState,
    pub package: Option<&'a Package>,
    pub type_ctx: &'a TypeContext,
}
