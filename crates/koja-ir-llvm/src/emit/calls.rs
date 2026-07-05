//! Direct-call emission. Closure-call dispatch lives in
//! [`super::closures`]. This module only handles the
//! statically-resolved [`koja_ir::IRInstruction::Call`] form.

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use koja_ir::{IRSymbol, ValueId};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};

use super::{ValueMap, lookup};

/// Call the function registered on `ctx.module` under the callee's
/// mangled symbol. Returns `None` for `Unit`-returning callees (LLVM
/// `void` calls), and the caller skips the value-map insert then.
pub(super) fn emit_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    args: &[ValueId],
    callee: &IRSymbol,
    values: &ValueMap<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
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
    Ok(call_site.try_as_basic_value().basic())
}
