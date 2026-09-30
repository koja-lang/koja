//! Storage and startup for [`IRConstantValue::Built`] constants. Each
//! `Built` entry gets one zero-initialized internal global, and one
//! function `__koja_const_init` runs every init in the order the IR
//! computed and stores the results. PID 1 calls it as its first
//! compiled code, right after the budget seed, in the script
//! user-main thunk and in the process entry wrapper. Every other
//! process is spawned from PID 1 or a descendant, so no read needs a
//! guard.
//!
//! The call sits inside PID 1 rather than in `@llvm.global_ctors`
//! because each init is a `FunctionKind::Regular` body with yield
//! checks, and [`crate::reductions`] requires that only glue runs
//! outside a process stack. A static constructor would spend the
//! aarch64 budget register on the loader's frame. Inside PID 1 an
//! init also has a valid pid, a seeded budget, and the panic handler.
//!
//! The order comes from [`koja_ir::IRProgram::built_constant_order`]
//! (or the script counterpart), and this module has no graph logic
//! of its own.

use inkwell::module::Linkage;
use koja_ir::{IRConstantValue, IRPackage, IRSymbol};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::ir_basic_type;

/// Symbol of the function that builds every `Built` constant.
pub(crate) const CONST_INIT_SYMBOL: &str = "__koja_const_init";

/// The LLVM global that holds the built value of `symbol`.
pub(crate) fn built_global_name(symbol: &IRSymbol) -> String {
    format!("koja_const.{}", symbol.mangled())
}

/// Declare one zero-initialized [`Linkage::Internal`] global per
/// `Built` entry across `packages`. Runs right after the constant
/// pool attaches, so every function body can load the global.
pub(crate) fn declare_built_constant_globals(
    ctx: &EmitContext<'_>,
    packages: &[IRPackage],
) -> Result<(), LlvmError> {
    for package in packages {
        for (symbol, value) in &package.constants {
            let IRConstantValue::Built { ty, .. } = value else {
                continue;
            };
            let llvm_ty = ir_basic_type(ctx, ty)?;
            let global = ctx
                .module
                .add_global(llvm_ty, None, &built_global_name(symbol));
            global.set_initializer(&llvm_ty.const_zero());
            global.set_linkage(Linkage::Internal);
        }
    }
    Ok(())
}

/// Define `void __koja_const_init()` that calls each init in `order`
/// and stores the result into the constant's global. Every init must
/// already be declared. Emits nothing when `order` is empty, and
/// [`emit_built_constant_init_call`] then emits nothing either.
pub(crate) fn emit_built_constant_init(
    ctx: &EmitContext<'_>,
    packages: &[IRPackage],
    order: &[IRSymbol],
) -> Result<(), LlvmError> {
    if order.is_empty() {
        return Ok(());
    }
    let init_fn = ctx.module.add_function(
        CONST_INIT_SYMBOL,
        ctx.context.void_type().fn_type(&[], false),
        Some(Linkage::Internal),
    );
    ctx.set_function_attributes(init_fn);
    let entry = ctx.context.append_basic_block(init_fn, "entry");
    ctx.builder.position_at_end(entry);
    for symbol in order {
        let init = packages
            .iter()
            .find_map(|package| package.constants.get(symbol))
            .and_then(|value| match value {
                IRConstantValue::Built { init, .. } => Some(init),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("built constant order names `{symbol}`, which is not a built pool entry")
            });
        let init_fn = ctx.declared_function(init).unwrap_or_else(|| {
            panic!(
                "built constant `{symbol}` init `{init}` is not declared \
                 (declare every function before `__koja_const_init`)",
            )
        });
        let value = ctx.call_basic(init_fn, &[], "built")?;
        let global = ctx
            .module
            .get_global(&built_global_name(symbol))
            .unwrap_or_else(|| {
                panic!(
                    "built constant `{symbol}` has no global \
                     (`declare_built_constant_globals` must precede `__koja_const_init`)",
                )
            });
        ctx.builder
            .build_store(global.as_pointer_value(), value)
            .or_ice()?;
    }
    ctx.builder.build_return(None).or_ice()?;
    Ok(())
}

/// Call `__koja_const_init` at the builder's current position when
/// the module defines it. PID 1's entry code calls this once, after
/// the budget seed and before any user code can read a constant.
pub(crate) fn emit_built_constant_init_call(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let Some(init_fn) = ctx.module.get_function(CONST_INIT_SYMBOL) else {
        return Ok(());
    };
    ctx.builder.build_call(init_fn, &[], "").or_ice()?;
    Ok(())
}
