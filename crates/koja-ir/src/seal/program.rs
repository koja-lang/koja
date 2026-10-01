//! Program-shaped seal entry. Asserts entry-point existence, then
//! delegates per-package work to [`super::function::seal_package`]
//! and finishes with the cross-function call-target lookup against
//! the assembled [`IRProgram`].

use crate::IRProgram;
use crate::function::FunctionKind;
use crate::mangling::mangled_method_name;
use crate::types::IRType;

use super::calls::seal_calls;
use super::closures::seal_closure_ops;
use super::constants::{seal_built_constants, seal_loadconst_pool};
use super::enums::seal_enum_ops;
use super::function::seal_package;
use super::seal_panic;
use super::structs::{package_instructions, seal_struct_ops};
use super::types::seal_package_types;

/// Assert every sealed-IR invariant over a merged program. Per-package
/// checks run first, then the cross-package ones that need the whole
/// function table.
pub(crate) fn seal_program(program: &IRProgram) {
    let Some(entry) = program.function(program.entry_point.mangled()) else {
        seal_panic(&format!(
            "entry point `{}` not registered in any package",
            program.entry_point
        ));
    };
    if !matches!(entry.kind, FunctionKind::ProcessEntryWrapper { .. }) {
        seal_panic(&format!(
            "entry point `{}` is not a `ProcessEntryWrapper` (got `{:?}`)",
            program.entry_point, entry.kind,
        ));
    }
    let declarations = program.declarations();
    for pkg in &program.packages {
        seal_package(pkg);
        seal_package_types(pkg, &declarations);
    }
    seal_program_calls(program);
    seal_program_struct_ops(program);
    seal_program_enum_ops(program);
    seal_program_closure_ops(program);
    seal_program_loadconst_pool(program);
    seal_built_constants(&declarations, &program.built_constant_order);
    seal_program_entry_wrappers(program);
}

/// Every [`FunctionKind::ProcessEntryWrapper`] must point at a struct
/// state whose `start`, `run`, and `priority` methods are registered
/// in the IRProgram. LLVM emit dereferences these symbols when
/// synthesizing the wrapper body, so a miss here would surface as a
/// codegen-time panic instead of a clear seal violation.
fn seal_program_entry_wrappers(program: &IRProgram) {
    for pkg in &program.packages {
        for (owner, function) in &pkg.functions {
            let FunctionKind::ProcessEntryWrapper { state } = &function.kind else {
                continue;
            };
            let IRType::Struct(state_symbol) = state else {
                seal_panic(&format!(
                    "process entry wrapper `{owner}` declared with non-struct state `{state:?}`",
                ));
            };
            for method in ["priority", "run", "start"] {
                let symbol = mangled_method_name(state_symbol, &[], method, 1, &[]);
                if program.function(symbol.mangled()).is_none() {
                    seal_panic(&format!(
                        "process entry wrapper `{owner}` references state method `{symbol}`, but \
                         that function is not registered in the IRProgram",
                    ));
                }
            }
        }
    }
}

/// Cross-package closure check: every `MakeClosure::body` must
/// resolve to a registered `FunctionKind::Closure` whose
/// `env_layout` and exposed signature line up with the
/// instruction's `captures` arity and `IRType::Function` value
/// type. See [`super::closures::seal_closure_ops`] for the full
/// rule list.
fn seal_program_closure_ops(program: &IRProgram) {
    let lookup = |mangled: &str| program.function(mangled);
    for pkg in &program.packages {
        seal_closure_ops(package_instructions(pkg), &lookup);
    }
}

/// Cross-package enum check: every `EnumConstruct::ty` must name an
/// enum decl registered in some package, and the supplied tag +
/// payload shape must match the variant. See
/// [`super::enums::seal_enum_ops`] for the full rule list.
fn seal_program_enum_ops(program: &IRProgram) {
    let lookup = |mangled: &str| program.enum_decl(mangled);
    for pkg in &program.packages {
        seal_enum_ops(package_instructions(pkg), &lookup);
    }
}

/// Cross-package struct check: every `StructInit::ty` and
/// `FieldGet::struct_symbol` must name a struct decl registered in
/// some package. Field-init counts/positions and field-index/type
/// matches are validated against the resolved decl. See
/// [`super::structs::seal_struct_ops`] for the full rule list.
fn seal_program_struct_ops(program: &IRProgram) {
    let lookup = |mangled: &str| program.struct_decl(mangled);
    for pkg in &program.packages {
        seal_struct_ops(package_instructions(pkg), &lookup);
    }
}

/// Cross-package constants check: every `LoadConst::const_id` must
/// resolve to a registered [`crate::IRConstantValue`] in some
/// package's pool. See [`super::constants::seal_loadconst_pool`].
fn seal_program_loadconst_pool(program: &IRProgram) {
    let lookup = |mangled: &str| program.constant_value(mangled);
    for pkg in &program.packages {
        seal_loadconst_pool(package_instructions(pkg), &lookup);
    }
}

/// Cross-function check: every `Call` callee and `Spawn` wrapper must
/// be a registered function in the IRProgram. See
/// [`super::calls::seal_calls`] for the full rule list.
fn seal_program_calls(program: &IRProgram) {
    let lookup = |mangled: &str| program.function(mangled);
    for pkg in &program.packages {
        seal_calls(package_instructions(pkg), &lookup);
    }
}
