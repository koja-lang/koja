//! Direct-call emission. Closure-call dispatch lives in
//! [`super::closures`]. This module only handles the
//! statically-resolved [`koja_ir::IRInstruction::Call`] form.

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use koja_ir::{IRSymbol, ValueId};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};

use super::ops::emit_finite_guard;
use super::{ValueMap, lookup};

/// Call the function registered on `ctx.module` under the callee's
/// mangled symbol. `Unit`-returning callees compile to LLVM `void`
/// calls, so their result is the inert `i8 0` unit placeholder.
/// Downstream consumers (local binds, returns, branch args) then
/// resolve the dest like any other value. Float-returning
/// `@extern "C"` callees get a finiteness trap on the result,
/// keeping foreign NaN / inf out of the finite-only `Float` types.
pub(super) fn emit_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    args: &[ValueId],
    callee: &IRSymbol,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let function = ctx.declared_function(callee).unwrap_or_else(|| {
        panic!(
            "LLVM emit: callee `{}` not registered in the declared-functions \
             index (declaration order or seal violation)",
            callee.mangled(),
        )
    });
    let mut arg_values: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for arg in args {
        arg_values.push(lookup(values, *arg)?.into());
    }
    let call_site = ctx
        .builder
        .build_call(function, &arg_values, "call")
        .or_ice()?;
    let Some(result) = call_site.try_as_basic_value().basic() else {
        return Ok(ctx.context.i8_type().const_zero().into());
    };
    if let (BasicValueEnum::FloatValue(float), Some(c_name)) =
        (result, ctx.extern_float_return(callee))
    {
        let message = format!("non-finite float returned by {c_name}");
        emit_finite_guard(ctx, float, &message)?;
    }
    Ok(result)
}
