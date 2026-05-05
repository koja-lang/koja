//! Bundle of the three inkwell handles every emission step needs:
//! the borrowed [`Context`], a fresh [`Module`], and a [`Builder`]
//! tied to the same `'ctx` lifetime.
//!
//! Deliberately a passive bundle — no business logic. Every
//! orchestration concern (program / script entry, function emission,
//! main-wrapper synthesis, instruction-level emission) lives in its
//! own module and takes `&EmitCtx` as a parameter, so this struct
//! never grows into a god object.
//!
//! Named [`EmitCtx`] rather than `LlvmCtx` because the role is
//! "context threaded through every emit operation," and to avoid
//! visual collision with [`inkwell::context::Context`] (which we
//! borrow inside).

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;

/// Fields are `pub(crate)` so sibling emission modules can borrow
/// them directly; outside the crate the bundle is opaque.
pub(crate) struct EmitCtx<'ctx> {
    pub(crate) builder: Builder<'ctx>,
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
}

impl<'ctx> EmitCtx<'ctx> {
    pub(crate) fn new(context: &'ctx Context) -> Self {
        Self {
            builder: context.create_builder(),
            context,
            module: context.create_module("expo_alpha_module"),
        }
    }
}
