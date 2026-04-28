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
//! Future waves can grow this struct (e.g. add `&LLVMTypeCache` for an
//! emit-side ctx) without rewriting call sites — the bundle is the single
//! point of accretion. Wave 7 added `&TypeLayouts` so layout-aware lowering
//! (concrete enum/struct construction, variant equality, indirect-field
//! drop traversal, constant enum tag lookup, etc.) can run as free
//! functions in [`crate::lower`] rather than inherent methods on
//! `Compiler`.

use std::path::Path;

use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Package, Type};

use crate::{FnLowerState, TypeLayouts};

/// Lookup oracle for the in-scope local bindings of the function
/// currently being lowered. Source of truth lives in `expo-codegen`'s
/// `Compiler.fn_state.variables`; the trait keeps `expo-ir` LLVM-free
/// while still giving lowering enough information to classify
/// [`expo_ast::ast::ExprKind::Ident`] occurrences as local-binding
/// reads (vs. constants or function references).
///
/// A trivial no-op implementation for `()` is provided so call sites
/// outside a live function context (e.g. monomorphization planners
/// that only see top-level declarations) can construct a `LowerCtx`
/// without inventing a binding store.
pub trait LocalBindings {
    /// Resolved Expo type of the binding, or `None` if no in-scope
    /// binding has that name. Mirrors the lookup in
    /// `Compiler.fn_state.variables.get(name).map(|(_, ty, _)| ty.clone())`.
    fn type_of(&self, name: &str) -> Option<Type>;
}

impl LocalBindings for () {
    fn type_of(&self, _name: &str) -> Option<Type> {
        None
    }
}

pub struct LowerCtx<'a> {
    pub closure_site_path: Option<&'a Path>,
    pub fn_lower: &'a FnLowerState,
    pub layouts: &'a TypeLayouts,
    /// Local bindings in scope at the current lowering site. See
    /// [`LocalBindings`].
    pub locals: &'a dyn LocalBindings,
    pub package: Option<&'a Package>,
    pub type_ctx: &'a TypeContext,
}
