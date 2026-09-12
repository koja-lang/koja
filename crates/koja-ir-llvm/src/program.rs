//! Compile a sealed [`IRProgram`] into the borrowed [`EmitContext`]'s
//! module: pre-emit every package's struct + enum types, emit the
//! runtime-name and exit-code globals, declare + define every
//! function (including the `ProcessEntryWrapper`), then synthesize
//! the host `main` trampoline ([`emit_process_entry_main`]) that
//! spawns the entry wrapper and returns the stored exit code.
//!
//! Types and functions are both declared opaque across every package
//! before any body is set, so forward references between structs,
//! enums, and mutually recursive functions resolve through
//! placeholders. Bodies that answer size and alignment queries
//! (enum complete and outer structs) are set last, in the dependency
//! order [`enums_in_dependency_order`] computes.

use koja_ir::IRProgram;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::function::{declare_function, define_function};
use crate::layout::enum_order::enums_in_dependency_order;
use crate::layout::enums::{
    declare_enum_type, define_enum_completes_and_outer, define_enum_payload_bodies,
};
use crate::layout::structs::{declare_struct_type, define_struct_body};
use crate::layout::unions::{declare_union_type, define_union_body};
use crate::layout::wire_contract::assert_wire_enum_order;
use crate::main_wrapper::{emit_app_name_global, emit_exit_code_global, emit_process_entry_main};

pub(crate) fn compile_program(
    ctx: &EmitContext<'_>,
    program: &IRProgram,
    app_name: &str,
) -> Result<(), LlvmError> {
    ctx.attach_constant_pool(crate::constant_pool::ConstantPoolSnapshot::from_packages(
        &program.packages,
    ));
    for package in &program.packages {
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
    for package in &program.packages {
        for decl in package.unions.values() {
            define_union_body(ctx, decl);
        }
        for decl in package.structs.values() {
            define_struct_body(ctx, decl)?;
        }
    }
    for package in &program.packages {
        for decl in package.enums.values() {
            define_enum_payload_bodies(ctx, decl)?;
        }
    }
    for decl in enums_in_dependency_order(&program.packages) {
        define_enum_completes_and_outer(ctx, decl)?;
    }
    assert_wire_enum_order(ctx)?;
    emit_app_name_global(ctx, app_name);
    let entry = program.entry_function();
    emit_exit_code_global(ctx);
    let mut declared = Vec::with_capacity(program.packages.iter().map(|p| p.functions.len()).sum());
    for package in &program.packages {
        for function in package.functions.values() {
            // The entry wrapper declares and defines like any other
            // helper (its kind is `ProcessEntryWrapper`). The host
            // `main` trampoline is synthesized separately by
            // `emit_process_entry_main`.
            declared.push((function, declare_function(ctx, function)?));
        }
    }
    for (function, llvm_function) in &declared {
        define_function(ctx, function, *llvm_function).map_err(|e| {
            LlvmError::Codegen(format!("while defining `{}`: {e:?}", function.symbol))
        })?;
    }
    emit_process_entry_main(ctx, entry)?;
    Ok(())
}
