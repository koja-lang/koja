//! Pre-codegen elaboration pass.
//!
//! Runs after monomorphization planning and before codegen consumes
//! [`crate::IRProgram`]. The intended responsibility set is
//! coercion decisions that need a fully-lowered IR view:
//!
//! - Protocol-driven shape rewriting -- e.g.
//!   [`crate::IRInstruction::FromListLiteral`] -> typed
//!   [`crate::IRInstruction::Call`] after monomorphizing the
//!   `from_list` impl.
//! - Generic phi-incoming coercion -- walk every
//!   [`crate::IRInstruction::Phi`], compare each incoming operand's
//!   type to the phi's `ty`, and prepend
//!   [`crate::IRInstruction::UnionWrap`] (or future
//!   `NumericCoerce`) where they disagree. Subsumes the per-arm
//!   widening lowering pre-stages today inside the value-context
//!   conditional/match constructs (transitional, see
//!   [`crate::Lowerer::build_arm_union_wrap`]).
//! - Numeric-coercion staging -- replaces the codegen-side
//!   `coerce_numeric` / `apply_coercion` decisions with explicit
//!   IR instructions so backends emit without inference.
//!
//! Today this is a no-op architectural seam: it ships so callers
//! can wire up the boundary now and downstream work fills the body
//! incrementally without relocating call sites.

use crate::program::IRProgram;

/// Elaborates `program` in place. No-op today; see the module
/// documentation for the planned responsibility set.
pub fn elaborate_program(_program: &mut IRProgram) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elaborate_program_is_a_no_op() {
        let mut program = IRProgram::default();
        elaborate_program(&mut program).expect("no-op elaboration always succeeds");
        assert!(program.functions.is_empty());
        assert!(program.structs.is_empty());
        assert!(program.enums.is_empty());
    }
}
