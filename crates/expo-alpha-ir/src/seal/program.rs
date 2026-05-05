//! Program-shaped seal entry. Asserts entry-point existence, then
//! delegates per-package work to [`super::function::seal_package`]
//! and finishes with the cross-function call-target lookup against
//! the assembled [`IRProgram`].

use crate::IRProgram;
use crate::function::IRInstruction;

use super::function::seal_package;
use super::seal_panic;

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
