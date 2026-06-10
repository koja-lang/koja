//! Compile a sealed [`IRProgram`] into the borrowed [`EmitContext`]'s
//! module: pre-emit every package's struct + enum types, emit the
//! runtime-name and exit-code globals, declare + define every
//! function (including the `ProcessEntryWrapper`), then synthesize
//! the host `main` trampoline ([`emit_process_entry_main`]) that
//! spawns the entry wrapper and returns the stored exit code.
//!
//! Struct + enum types are pre-emitted in two phases (declare
//! opaque, then set body) across every package so a struct- or
//! enum-typed parameter, return type, or payload field resolves
//! before any function signature is built. The two-phase
//! declare-then-define pattern on functions lets mutually-recursive
//! calls resolve through `module.get_function` before either body
//! has been walked.
//!
//! Phase ordering rationale: structs and enums share a single
//! declare-then-define pair so a struct field can carry an
//! `IRType::Enum(_)` and an enum's tuple/struct variant can carry
//! an `IRType::Struct(_)`. Both forward references resolve through
//! the opaque placeholders the declare phase mints up-front.
//!
//! The define phase splits into four sub-steps so size and
//! alignment queries always see fully-bodied operands:
//!
//! 1. Set every union and struct body across all packages
//!    ([`define_union_body`] / [`define_struct_body`]). Neither
//!    queries `get_abi_size` so opaque inner enum references in a
//!    struct field are fine.
//! 2. Set every enum variant *payload* body across all packages
//!    ([`define_enum_payload_bodies`]). Same property — no size
//!    query, so opaque inner enum-outer references in a payload
//!    field are still fine.
//! 3. Sort the enum decls in dependency order (every enum E whose
//!    payload references enum F is placed after F).
//! 4. Walk the sorted decls and set each one's variant *complete*
//!    body and outer chunk body ([`define_enum_completes_and_outer`]).
//!    These query `get_abi_size` / `get_abi_alignment`, so every
//!    transitively-referenced enum outer must already be bodied —
//!    the topological order guarantees that.
//!
//! Without step 3 a stdlib enum like `Option<TestApp.TokenKind>`
//! in `Global` would have its complete body set before
//! `TestApp.TokenKind`'s outer, leaving the variant payload
//! reading an opaque inner (`align 1`, `size 0`) and collapsing
//! the outer chunk count to a single byte — wire-format wrong.

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
    emit_app_name_global(ctx, app_name);
    let entry = program.entry_function();
    emit_exit_code_global(ctx);
    let mut declared = Vec::with_capacity(program.packages.iter().map(|p| p.functions.len()).sum());
    for package in &program.packages {
        for function in package.functions.values() {
            // The entry wrapper declares and defines like any other
            // helper (its kind is `ProcessEntryWrapper`); the host
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
