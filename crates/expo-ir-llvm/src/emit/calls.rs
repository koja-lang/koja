//! Direct-call emission. Closure-call dispatch lives in
//! [`super::closures`]; this module only handles the
//! statically-resolved [`expo_alpha_ir::IRInstruction::Call`] form.

use expo_alpha_ir::{IRSymbol, ValueId};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::ctx::EmitContext;
use crate::error::LlvmError;

use super::{ValueMap, inkwell_err, lookup};

/// Call the function registered on `ctx.module` under the callee's
/// mangled symbol. Returns `None` for `Unit`-returning callees (LLVM
/// `void` calls); the caller skips the value-map insert in that case.
pub(super) fn emit_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    args: &[ValueId],
    callee: &IRSymbol,
    values: &ValueMap<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
    let function = ctx.declared_function(callee).unwrap_or_else(|| {
        panic!(
            "alpha LLVM emit: callee `{}` not registered in the declared-functions \
             index — declaration order or seal violation",
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
        .map_err(|e| inkwell_err(format_args!("build_call for `{}`", callee.mangled()), e))?;
    Ok(call_site.try_as_basic_value().basic())
}
