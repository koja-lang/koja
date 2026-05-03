//! Closure sub-pass: whole-program walk that discovers required
//! generic instantiations and registers them with the planners that
//! emit specialized decls.
//!
//! Today this is a no-op — the POC scope (`fn main; 2 + 2; end`) ships
//! no generics. The pass exists in the pipeline so that when generic
//! specialization lands the orchestration in [`crate::lower_program`]
//! does not change shape.

use crate::IRProgram;

pub(crate) fn closure(program: IRProgram) -> IRProgram {
    program
}
