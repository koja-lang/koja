//! Runtime-symbol declarations shared between [`crate::main_wrapper`]
//! (which calls them from the auto-print scaffolding) and
//! [`crate::intrinsics`] (which calls them from compiler-synthesized
//! `@intrinsic` bodies).
//!
//! Each runtime helper lives in `expo-runtime/src/alpha.rs`; this
//! module owns the LLVM-side declarations so the two callers stamp
//! exactly one `module.get_function` lookup per symbol.

use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::FunctionValue;

use crate::ctx::EmitCtx;

pub(crate) const PRINT_BOOL_SYMBOL: &str = "__expo_alpha_print_bool";
pub(crate) const PRINT_F32_SYMBOL: &str = "__expo_alpha_print_f32";
pub(crate) const PRINT_F64_SYMBOL: &str = "__expo_alpha_print_f64";
pub(crate) const PRINT_INT_SYMBOL: &str = "__expo_alpha_print_i64";
pub(crate) const PRINT_STRING_SYMBOL: &str = "__expo_alpha_print_string";

/// Get the existing declaration for `symbol` or stamp a fresh
/// `void(arg_type)` external one. Idempotent so callers can declare
/// the same printer from multiple emit sites without duplicating.
pub(crate) fn declare_runtime_printer<'ctx>(
    ctx: &EmitCtx<'ctx>,
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
