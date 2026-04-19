//! Lowering functions: take the typed AST plus semantic context (a
//! `&TypeContext` from typecheck and a `&TypeLayouts` from this crate)
//! and produce `Resolved*` values that emission consumes mechanically.
//!
//! This is the destination for codegen's `lower_*` / `resolve_*` helpers as
//! they shed their dependency on the `Compiler<'ctx>` god-object. Each
//! lifted function loses its `&Compiler` parameter and gains explicit
//! borrows of just the semantic state it needs.
//!
//! Most lowering functions take a [`LowerCtx`] borrow bundle that bundles
//! the read-only context they all share (`&TypeContext`, current package,
//! `&FnLowerState`, current closure-site path). Construct one with
//! `Compiler::lower_ctx()` (in `expo-codegen`) and pass it by reference.
//! See [`ctx`] for the bundle and [`types`] / [`naming`] / [`closures`]
//! for the helpers it serves; field-access lowering predates the bundle
//! and lives in [`fields`].

pub mod closures;
pub mod ctx;
pub mod fields;
pub mod naming;
pub mod types;

pub use ctx::LowerCtx;
