//! Per-function lowering object that owns the operand-lowering call
//! surface as inherent methods.
//!
//! [`Lowerer<'a>`] is the stateful companion to [`LowerCtx`]: where
//! `LowerCtx` is a borrow bundle of read-only references shared across
//! many free functions, `Lowerer` couples those same references to a
//! mutable [`FnLowerState`] borrow so the operand-lowering helpers
//! (`lower_expr_to_operand`, `lower_unless`, `lower_if_no_else`,
//! `lower_binary_op_or_stub`, `lower_unary_op_or_stub`,
//! `lower_field_access_or_stub`) can run as `&mut self` methods. This
//! dissolves the borrow conflict that arises when a lowering site
//! needs both `&LowerCtx` (for type / layout context, e.g.
//! [`lower_struct_field`](crate::lower::fields::lower_struct_field))
//! and `&mut FnLowerState` (for fresh `IRValueId` / `IRBlockId` mints).
//!
//! Migration is incremental: only the operand-lowering surface lives
//! on `Lowerer` today. Other `lower_*` helpers in [`crate::lower::calls`],
//! [`crate::lower::enums`], [`crate::lower::stmt`], [`crate::lower::types`],
//! etc. continue to take `&LowerCtx` and migrate as future slices touch
//! them. The [`Lowerer::ctx`] accessor builds an ad-hoc `LowerCtx<'_>`
//! for delegating to those un-migrated helpers from inside a `Lowerer`
//! method.

use std::path::Path;

use expo_typecheck::context::TypeContext;
use expo_typecheck::types::Package;

use crate::blocks::IRBlockId;
use crate::lower::ctx::LowerCtx;
use crate::program::IRProgram;
use crate::values::IRValueId;
use crate::{FnLowerState, TypeLayouts};

/// Per-function lowering object. Holds program-level read-only
/// references plus `&mut FnLowerState` for SSA / block id minting.
/// Constructed by `expo-codegen`'s `Compiler::lowerer()`.
///
/// `program` is the canonical callable-symbol registry from
/// [`IRProgram`]; lift helpers consult `program.contains_function`
/// to decide whether a call's mangled target is registered (and
/// therefore safe to lift to a typed [`crate::values::IRInstruction::Call`]
/// / [`crate::values::IRInstruction::MethodCall`]) versus deferring
/// to [`crate::values::IRInstruction::Stub`].
///
/// Slice 3b (Wave 32): the lowerer's local-binding view comes from
/// [`FnLowerState::local_types`] (typed-locals table populated by
/// the binding-site emitters; LLVM-free), so a separate `locals`
/// borrow on the lowerer would alias the `&mut fn_state`. The
/// `Self::ctx` accessor exposes both via the same `&FnLowerState`
/// re-borrow.
pub struct Lowerer<'a> {
    pub closure_site_path: Option<&'a Path>,
    pub fn_state: &'a mut FnLowerState,
    pub layouts: &'a TypeLayouts,
    pub package: Option<&'a Package>,
    pub program: &'a IRProgram,
    pub type_ctx: &'a TypeContext,
}

impl<'a> Lowerer<'a> {
    /// Build an ad-hoc [`LowerCtx<'_>`] view tied to this lowerer's
    /// program-level references. The returned view borrows
    /// [`Self::fn_state`] immutably (used both as `fn_lower` and as
    /// the `LocalBindings` source for typed-local lookups); once it
    /// is dropped the lowerer can resume `&mut self` operations on
    /// the same fn_state. Use when delegating to free functions in
    /// [`crate::lower`] that still take `&LowerCtx`.
    pub fn ctx(&self) -> LowerCtx<'_> {
        LowerCtx {
            closure_site_path: self.closure_site_path,
            fn_lower: &*self.fn_state,
            layouts: self.layouts,
            locals: &*self.fn_state,
            package: self.package,
            type_ctx: self.type_ctx,
        }
    }

    /// Mint a fresh function-scoped basic block identifier. Forwards
    /// to [`FnLowerState::next_block_id`].
    pub fn next_block_id(&mut self) -> IRBlockId {
        self.fn_state.next_block_id()
    }

    /// Mint a fresh function-scoped SSA value identifier. Forwards
    /// to [`FnLowerState::next_value_id`].
    pub fn next_value_id(&mut self) -> IRValueId {
        self.fn_state.next_value_id()
    }
}
