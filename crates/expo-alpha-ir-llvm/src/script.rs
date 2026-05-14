//! Compile a sealed [`IRScript`] into the borrowed [`EmitContext`]'s
//! module: pre-emit every package's struct types, emit the
//! runtime-name global, declare every helper, synthesize the script
//! body as `main` (the spawn-driven trampoline in
//! [`crate::main_wrapper`]), then define each helper's body.
//!
//! Same shape as [`crate::program::compile_program`] minus the
//! "skip the entry function" step — script-mode has no `fn main`
//! item; `script.blocks` is the body that becomes `main`.

use expo_alpha_ir::IRScript;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::function::{declare_function, define_function};
use crate::layout::enum_order::enums_in_dependency_order;
use crate::layout::enums::{
    declare_enum_type, define_enum_completes_and_outer, define_enum_payload_bodies,
};
use crate::layout::structs::{declare_struct_type, define_struct_body};
use crate::layout::unions::{declare_union_type, define_union_body};
use crate::main_wrapper::{emit_app_name_global, emit_as_main};

pub(crate) fn compile_script(
    ctx: &EmitContext<'_>,
    script: &IRScript,
    app_name: &str,
) -> Result<(), LlvmError> {
    ctx.attach_constant_pool(crate::constant_pool::ConstantPoolSnapshot::from_packages(
        &script.packages,
    ));
    for package in &script.packages {
        for decl in package.unions.values() {
            declare_union_type(ctx, decl);
        }
        for decl in package.structs.values() {
            declare_struct_type(ctx, decl);
        }
        for decl in package.enums.values() {
            declare_enum_type(ctx, decl);
        }
    }
    for package in &script.packages {
        for decl in package.unions.values() {
            define_union_body(ctx, decl);
        }
        for decl in package.structs.values() {
            define_struct_body(ctx, decl)?;
        }
    }
    for package in &script.packages {
        for decl in package.enums.values() {
            define_enum_payload_bodies(ctx, decl)?;
        }
    }
    for decl in enums_in_dependency_order(&script.packages) {
        define_enum_completes_and_outer(ctx, decl)?;
    }
    emit_app_name_global(ctx, app_name);
    let mut declared = Vec::with_capacity(script.packages.iter().map(|p| p.functions.len()).sum());
    for package in &script.packages {
        for function in package.functions.values() {
            declared.push((function, declare_function(ctx, function)?));
        }
    }
    emit_as_main(ctx, &script.blocks)?;
    for (function, llvm_function) in declared {
        define_function(ctx, function, llvm_function).map_err(|e| {
            LlvmError::Codegen(format!("while defining `{}`: {e:?}", function.symbol))
        })?;
    }
    Ok(())
}
