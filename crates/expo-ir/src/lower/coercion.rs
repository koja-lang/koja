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
}
