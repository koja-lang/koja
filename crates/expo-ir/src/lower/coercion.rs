//! Span-keyed coercion staging.
//!
//! Typed coercions live in
//! [`expo_typecheck::context::TypeContext::coercions`], keyed by the
//! coerced subexpression's [`Span`]. Lowering reads this table at the
//! consuming call-site (assignment RHS, return value, method-call
//! receiver / argument, ...) and stages the corresponding typed
//! [`crate::values::IRInstruction`] so emission stays purely
//! mechanical.
//!
//! Slice 1 (Wave 32) lifted the only registered variant
//! ([`Coercion::UnionWiden`]) into [`crate::values::IRInstruction::UnionWrap`]
//! at the `MethodCall` receiver / argument seam, retiring the matching
//! `expo-codegen` `apply_coercion` path. Future `Coercion` variants
//! (e.g. numeric coercion) join this module as new
//! `stage_*` helpers and the same call-sites pick them up via the
//! shared [`Lowerer::stage_coercion`] dispatch.

use expo_ast::ast::Arg;
use expo_ast::span::Span;
use expo_typecheck::context::Coercion;

use crate::Lowerer;
use crate::blocks::IRBlockId;
use crate::cfg::CFGBuilder;
use crate::lower::stmt::resolve_coercion;
use crate::values::{IRInstruction, IROperand};

impl<'a> Lowerer<'a> {
    /// Stage an [`IRInstruction::UnionWrap`] when typecheck registered
    /// a [`Coercion::UnionWiden`] for `span`; otherwise return `op`
    /// unchanged.
    ///
    /// Call this at every site that consumes an already-lowered
    /// operand whose value must satisfy a wider union-typed slot
    /// (assignment RHS, return value, method-call receiver / argument,
    /// ...). The caller passes the *consuming* span so this matches
    /// the typecheck side's keying. The guard-clause short-circuit
    /// keeps call-sites a single line for the no-coercion case.
    pub fn stage_union_widen(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        span: Span,
        op: IROperand,
    ) -> IROperand {
        let Some(Coercion::UnionWiden { source, target }) = resolve_coercion(&self.ctx(), span)
        else {
            return op;
        };
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::UnionWrap {
                dest,
                value: op,
                source_ty: source,
                target_union: target,
            },
        );
        IROperand::Local(dest)
    }

    /// Stage [`stage_union_widen`] for every arg slot whose source span
    /// has a registered [`Coercion::UnionWiden`]. Shared by all three
    /// call lifters (`emit_call_instruction`,
    /// `emit_static_call_instruction`, `emit_method_call_instruction`)
    /// so the per-arg coercion seam lives in one place.
    ///
    /// `lowered_args[i]` is updated in-place to the wrapped operand
    /// when arg `i` needs widening; otherwise it stays unchanged.
    /// Length must match `args` (callers always lower one operand per
    /// arg via `lower_expr_sequence`).
    pub fn stage_arg_coercions(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        args: &[Arg],
        lowered_args: &mut [IROperand],
    ) {
        for (arg, slot) in args.iter().zip(lowered_args.iter_mut()) {
            *slot = self.stage_union_widen(builder, open, arg.value.span, slot.clone());
        }
    }
}
