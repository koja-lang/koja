//! Elaborate sub-pass: reserved slot for later refinements that need
//! to run after generic discovery (`closure`) but before sealing.
//!
//! Today this is a no-op. Lands when a real refinement pass needs it
//! (e.g. coercion-instruction emission once `Coercion` annotations
//! flow through from `expo-typecheck-v2`).

use crate::IRProgram;

pub(crate) fn elaborate(program: IRProgram) -> IRProgram {
    program
}
