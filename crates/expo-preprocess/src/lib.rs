//! AST-level preprocessing passes for the Expo language.
//!
//! Runs between [`expo_parser`](../expo_parser/index.html) and
//! [`expo_typecheck`](../expo_typecheck/index.html), rewriting the
//! [`Module`] in place. Operates on structured AST -- this is **not** a
//! C-style text preprocessor.
//!
//! Today the only pass is [`derive::derive_debug`], which synthesizes
//! `impl Debug for T` blocks for every user-defined struct/enum without
//! one. After this pass, the rest of the pipeline (typecheck, IR, codegen)
//! sees the synthesized impls as regular code and needs no special-casing.
//!
//! ## Planned future passes
//!
//! - `cfg_prune` -- evaluate `@cfg` / `@target` annotations against the
//!   build context and drop items that don't match.
//! - `derive_equality`, `derive_hash`, `derive_ord` -- mechanical
//!   follow-ups once `derive_debug` lands.
//! - `expand_destructuring` -- desugar struct destructuring assignments.
//! - `expand_command` -- desugar the planned `command` construct.
//!
//! Pass ordering matters when more land. `cfg_prune` must run before any
//! `derive_*` so we don't synthesize impls for items that get pruned.

use expo_ast::ast::Module;

pub mod derive;

/// Runs every preprocessing pass against `module`. Mutates in place.
///
/// Call sites: [`expo_driver`](../expo_driver/index.html)'s typecheck
/// pipeline and [`expo_lsp`](../expo_lsp/index.html)'s diagnostic
/// pipeline. `expo fmt` and `expo parse` deliberately bypass this --
/// they show the user what they wrote.
pub fn preprocess_module(module: &mut Module) {
    derive::derive_debug(module);
}
