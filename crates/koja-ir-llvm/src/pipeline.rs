//! The compile sequence [`crate::program::compile_program`] and
//! [`crate::script::compile_script`] share. Types and functions are
//! both declared opaque across every package before any body is set,
//! so forward references between structs, enums, and mutually
//! recursive functions resolve through placeholders. Bodies that
//! answer size and alignment queries (enum complete and outer
//! structs) are set last, in the dependency order
//! [`enums_in_dependency_order`] computes. Only the host `main`
//! differs between the two entries, see [`EntryShape`].

use koja_ir::{IRBasicBlock, IRFunction, IRPackage, IRSourceDef, IRSymbol};

use crate::constant_pool::ConstantPoolSnapshot;
use crate::ctx::EmitContext;
use crate::emit::built_constants::{declare_built_constant_globals, emit_built_constant_init};
use crate::error::LlvmError;
use crate::function::{declare_function, define_function};
use crate::layout::enum_order::enums_in_dependency_order;
use crate::layout::enums::{
    declare_enum_type, define_enum_completes_and_outer, define_enum_payload_bodies,
};
use crate::layout::structs::{declare_struct_type, define_struct_body};
use crate::layout::unions::{declare_union_type, define_union_body};
use crate::layout::wire_contract::assert_wire_enum_order;
use crate::main_wrapper::{
    emit_app_name_global, emit_exit_code_global, emit_process_entry_main, emit_script_main,
};

/// What becomes the host `main`. A project spawns its
/// `ProcessEntryWrapper` and returns the stored exit code. A script
/// runs its top-level blocks as PID 1.
pub(crate) enum EntryShape<'a> {
    Process(&'a IRFunction),
    Script {
        blocks: &'a [IRBasicBlock],
        def_location: Option<&'a IRSourceDef>,
    },
}

/// Compile `packages` into the borrowed [`EmitContext`]'s module and
/// synthesize the host `main` for `entry`.
pub(crate) fn compile_packages(
    ctx: &EmitContext<'_>,
    packages: &[IRPackage],
    built_constant_order: &[IRSymbol],
    app_name: &str,
    entry: EntryShape<'_>,
) -> Result<(), LlvmError> {
    ctx.attach_constant_pool(ConstantPoolSnapshot::from_packages(packages));
    declare_types(ctx, packages);
    define_types(ctx, packages)?;
    assert_wire_enum_order(ctx)?;
    // Built constant globals need every struct and enum body above,
    // and every function body below loads them.
    declare_built_constant_globals(ctx, packages)?;
    emit_app_name_global(ctx, app_name);
    if let EntryShape::Process(_) = entry {
        emit_exit_code_global(ctx);
    }
    let mut declared = Vec::with_capacity(packages.iter().map(|p| p.functions.len()).sum());
    for package in packages {
        for function in package.functions.values() {
            // The entry wrapper declares and defines like any other
            // helper (its kind is `ProcessEntryWrapper`). The host
            // `main` is synthesized separately below.
            declared.push((function, declare_function(ctx, function)?));
        }
    }
    // Defined before the bodies so the entry wrapper or the user-main
    // thunk can call it.
    emit_built_constant_init(ctx, packages, built_constant_order)?;
    for (function, llvm_function) in declared {
        define_function(ctx, function, llvm_function).map_err(|e| {
            LlvmError::Codegen(format!("while defining `{}`: {e:?}", function.symbol))
        })?;
    }
    match entry {
        EntryShape::Process(function) => emit_process_entry_main(ctx, function),
        EntryShape::Script {
            blocks,
            def_location,
        } => emit_script_main(ctx, blocks, def_location),
    }
}

/// Declare every union, struct, and enum type opaque.
fn declare_types(ctx: &EmitContext<'_>, packages: &[IRPackage]) {
    for package in packages {
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
}

/// Set the type bodies. Unions and structs first, then enum payloads,
/// then enum completes and outer blobs in dependency order.
fn define_types(ctx: &EmitContext<'_>, packages: &[IRPackage]) -> Result<(), LlvmError> {
    for package in packages {
        for decl in package.unions.values() {
            define_union_body(ctx, decl);
        }
        for decl in package.structs.values() {
            define_struct_body(ctx, decl)?;
        }
    }
    for package in packages {
        for decl in package.enums.values() {
            define_enum_payload_bodies(ctx, decl)?;
        }
    }
    for decl in enums_in_dependency_order(packages) {
        define_enum_completes_and_outer(ctx, decl)?;
    }
    Ok(())
}
