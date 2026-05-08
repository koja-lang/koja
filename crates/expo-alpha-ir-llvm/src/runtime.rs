//! Runtime-symbol declarations shared between [`crate::main_wrapper`]
//! (which calls them from the auto-print scaffolding) and
//! [`crate::intrinsics`] (which calls them from compiler-synthesized
//! `@intrinsic` bodies).
//!
//! Each runtime helper lives in `expo-runtime/src/alpha.rs`; this
//! module owns the LLVM-side declarations so the two callers stamp
//! exactly one `module.get_function` lookup per symbol.

use inkwell::AddressSpace;
use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;

pub(crate) const FREE_SYMBOL: &str = "free";
pub(crate) const PRINT_BOOL_SYMBOL: &str = "__expo_alpha_print_bool";
pub(crate) const PRINT_F32_SYMBOL: &str = "__expo_alpha_print_f32";
pub(crate) const PRINT_F64_SYMBOL: &str = "__expo_alpha_print_f64";
pub(crate) const PRINT_INT_SYMBOL: &str = "__expo_alpha_print_i64";
pub(crate) const PRINT_STRING_SYMBOL: &str = "__expo_alpha_print_string";

/// Get the existing declaration for `symbol` or stamp a fresh
/// `void(arg_type)` external one. Idempotent so callers can declare
/// the same printer from multiple emit sites without duplicating.
pub(crate) fn declare_runtime_printer<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    let signature = ctx.context.void_type().fn_type(&[argument_type], false);
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `free` extern. The drop emitter
/// calls this once per heap-typed slot at function exit. Signature
/// is `void(i8*)`; alpha's heap-block pointers are computed by
/// adjusting the SSA payload pointer (`payload - 8`) before the
/// call so `free` sees the allocator's block base.
pub(crate) fn declare_free_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(FREE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(FREE_SYMBOL, signature, Some(Linkage::External))
}
