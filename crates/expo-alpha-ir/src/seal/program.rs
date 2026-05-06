//! Program-shaped seal entry. Asserts entry-point existence, then
//! delegates per-package work to [`super::function::seal_package`]
//! and finishes with the cross-function call-target lookup against
//! the assembled [`IRProgram`].

use crate::IRProgram;
use crate::function::IRInstruction;

use super::enums::seal_enum_ops;
use super::function::seal_package;
use super::seal_panic;
use super::structs::{package_instructions, seal_struct_ops};

pub(crate) fn seal_program(program: &IRProgram) {
    if program.function(program.entry_point.mangled()).is_none() {
        seal_panic(&format!(
            "entry point `{}` not registered in any package",
            program.entry_point
        ));
    }
    for pkg in &program.packages {
        seal_package(pkg);
    }
    seal_program_calls(program);
    seal_program_struct_ops(program);
    seal_program_enum_ops(program);
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

/// Cross-function check: every `IRInstruction::Call` must name a
/// callee that exists as a registered function in the IRProgram. Lower
/// dereferences the callee id through the typecheck registry, so a
/// missing target here would indicate either a registry / IRProgram
/// drift or a genuine lowering bug — both compiler issues.
fn seal_program_calls(program: &IRProgram) {
    for pkg in &program.packages {
        for (owner, function) in &pkg.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    if let IRInstruction::Call { callee, .. } = inst
                        && program.function(callee.mangled()).is_none()
                    {
                        seal_panic(&format!(
                            "function `{owner}` calls `{callee}`, but that function is not \
                             registered in the IRProgram",
                        ));
                    }
                }
            }
        }
    }
}
