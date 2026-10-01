//! Cross-function call-target check shared by the program and script
//! seal entries. Both shapes feed it an `(owner, instruction)` stream
//! (see [`super::structs::package_instructions`]) and a lookup over
//! their own assembled function table.

use crate::function::{FunctionKind, IRFunction, IRInstruction};

use super::seal_panic;

/// Every [`IRInstruction::Call`] must name a callee that the lookup
/// can find. Lower dereferences the callee id through the typecheck
/// registry, so a missing target here means either a registry / IR
/// drift or a genuine lowering bug, both compiler issues.
///
/// The same check applies to [`IRInstruction::Spawn::wrapper`]. Spawn
/// wrappers are minted by the spawn-wrapper monomorphization planner,
/// and a missing one means the closure pass failed to discover the
/// spawn site. Wrappers must register as `FunctionKind::SpawnWrapper`.
pub(super) fn seal_calls<'inst, 'fun>(
    instructions: impl IntoIterator<Item = (String, &'inst IRInstruction)>,
    lookup: &impl Fn(&str) -> Option<&'fun IRFunction>,
) {
    for (owner, inst) in instructions {
        match inst {
            IRInstruction::Call { callee, .. } => {
                if lookup(callee.mangled()).is_none() {
                    seal_panic(&format!(
                        "{owner} calls `{callee}`, but that function is not registered",
                    ));
                }
            }
            IRInstruction::Spawn { wrapper, .. } => {
                let Some(target) = lookup(wrapper.mangled()) else {
                    seal_panic(&format!(
                        "{owner} spawns `{wrapper}`, but no spawn wrapper with that symbol is \
                         registered",
                    ));
                };
                if !matches!(target.kind, FunctionKind::SpawnWrapper { .. }) {
                    seal_panic(&format!(
                        "{owner} spawns `{wrapper}` but that function's kind is not \
                         `SpawnWrapper`",
                    ));
                }
            }
            _ => {}
        }
    }
}
