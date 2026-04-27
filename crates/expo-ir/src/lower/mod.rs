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
//! the read-only context they all share (`&TypeContext`, `&TypeLayouts`,
//! current package, `&FnLowerState`, current closure-site path).
//! Construct one with `Compiler::lower_ctx()` (in `expo-codegen`) and pass
//! it by reference. See [`ctx`] for the bundle.

pub mod binary;
pub mod calls;
pub mod closures;
pub mod conditionals;
pub mod constants;
pub mod ctx;
pub mod debug;
pub mod enums;
pub mod fields;
pub mod inference;
pub mod loops;
pub mod mangling;
pub mod methods;
pub mod monomorphize;
pub mod naming;
pub mod ops;
pub mod patterns;
pub mod processes;
pub mod stmt;
pub mod strings;
pub mod structs;
pub mod types;
pub mod values;

pub use ctx::LowerCtx;
