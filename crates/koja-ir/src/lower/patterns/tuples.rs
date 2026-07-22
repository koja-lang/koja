//! Anonymous-tuple pattern lowering.
//!
//! Mirrors the plain-struct walk in [`super::structs`] with
//! positional [`BindOp::TupleElement`] extraction instead of
//! decl-indexed fields. Tuples are structural, so the subject's
//! `AnonymousKind::Tuple` shape is the only source of element types.

use koja_ast::ast::Pattern;
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_typecheck::{GlobalRegistry, peel_alias};

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::package::resolved_type_to_ir_type;
use super::structs::lower_subpattern_into;
use super::{BindOp, BindStep, ChainMode, PatternCheck, PatternInputs};
use crate::function::IRBlockId;

pub(super) fn lower_tuple_check(
    elements: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> (PatternCheck, IRBlockId) {
    let element_types = tuple_element_types(inputs.subject_ty, elements.len(), inputs.registry);
    let mut binds = Vec::new();
    let mut steps = Vec::new();
    let mut current_block = block;
    for (index, (pattern, element_resolved)) in elements.iter().zip(&element_types).enumerate() {
        let element_ir = resolved_type_to_ir_type(
            element_resolved,
            inputs.registry,
            &mut output.instantiations,
        );
        let prefix = BindStep {
            op: BindOp::TupleElement {
                index: index as u32,
            },
            output_type: element_ir.clone(),
        };
        lower_subpattern_into(
            pattern,
            element_resolved,
            &element_ir,
            inputs.subject,
            prefix,
            inputs,
            ctx,
            &mut current_block,
            &mut steps,
            &mut binds,
            output,
        );
    }
    if steps.is_empty() {
        (PatternCheck::CatchAll { binds }, current_block)
    } else {
        (
            PatternCheck::Tests {
                chain_mode: ChainMode::And,
                payload_binds: binds,
                steps,
            },
            current_block,
        )
    }
}

/// The subject's element resolved types, arity-checked against the
/// pattern.
pub(super) fn tuple_element_types(
    subject_ty: &ResolvedType,
    arity: usize,
    registry: &GlobalRegistry,
) -> Vec<ResolvedType> {
    let ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) =
        peel_alias(subject_ty, registry)
    else {
        panic!(
            "IR lower: tuple pattern subject resolved to `{subject_ty:?}` after \
             typecheck seal (resolve invariant violation)",
        );
    };
    assert_eq!(
        elements.len(),
        arity,
        "IR lower: tuple pattern arity diverges from subject after typecheck",
    );
    elements
}
