//! Lowering functions: take the typed AST plus semantic context (a
//! `&TypeContext` from typecheck and a `&TypeLayouts` from this crate)
//! and produce `Resolved*` values that emission consumes mechanically.
//!
//! This is the destination for codegen's `lower_*` / `resolve_*` helpers as
//! they shed their dependency on the `Compiler<'ctx>` god-object. Each
//! lifted function loses its `&Compiler` parameter and gains explicit
//! borrows of just the semantic state it needs, matching the canonical
//! signature shape `(layouts, type_ctx, ast_inputs) -> Resolved*`.
//!
//! The first members live in [`fields`]; future sessions will add
//! `construction`, `calls`, and so on.

pub mod fields;
