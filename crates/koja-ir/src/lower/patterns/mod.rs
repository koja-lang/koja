//! Pattern lowering walks a [`Pattern`] against a subject `ValueId`
//! and produces a [`PatternCheck`] describing whether the arm fires
//! unconditionally, after one or more chained predicates (joined
//! either by [`ChainMode::And`] for struct/enum field-test chains
//! or [`ChainMode::Or`] for or-pattern alternatives), and what
//! field binds (`EnumPayloadFieldGet` / `FieldGet` + `LocalWrite`
//! chains) the success edge needs to perform before the arm body
//! runs. The match driver in [`super::match_expr`] consumes the
//! result to wire the gating `CondBranch`(es) and the per-arm body
//! block.
//!
//! Admits leaves (wildcard / binding / literal), `EnumUnit`,
//! `EnumTuple` / `EnumStruct` / `Struct` with arbitrary nested
//! payload / field patterns, and `Or` (alternatives restricted to
//! literal / EnumUnit, no bindings). Every other shape is a feature
//! gap diagnosed in typecheck and is unreachable on the success
//! path.
//!
//! # Module layout
//!
//! - [`enums`]: `EnumUnit` / `EnumTuple` / `EnumStruct` shapes,
//!   `emit_enum_tag_eq`, and the shared enum-payload metadata
//!   resolver. Tuple / struct variants recursively lower their
//!   payload patterns and emit AND-chained `TestStep`s for any
//!   non-binding inner shape.
//! - [`structs`]: plain-struct destructure. Field patterns
//!   recursively lower against the field type. Bindings produce
//!   chained [`BindOp::StructField`] binds, non-binding fields add
//!   AND-chained `TestStep`s.
//! - [`or_pattern`]: `A | B | C` chained through fresh
//!   `match_or_alt_<n>` blocks, one [`TestStep`] per alternative
//!   under [`ChainMode::Or`].
//! - [`typed_binding`]: `name: Type`. A binding of the whole subject
//!   has no test. A binding of one union member is one tag test and
//!   one [`BindOp::UnionPayload`] bind. A binding of a union of
//!   members is one [`TestStep`] for each member the binding leaves
//!   out, and one [`BindOp::UnionNarrow`] bind.
//! - [`union_member`]: the wrapper that the typecheck puts around a
//!   variant or struct pattern on a union subject. One tag test for
//!   the member, then the inner pattern against the payload through
//!   a [`BindOp::UnionPayload`] prefix.
//! - [`literals`]: the `subject == const(value)` emission shared
//!   between the dispatcher's `Pattern::Literal` arm and the
//!   or-pattern literal-alternative arm.

mod enums;
mod literals;
mod or_pattern;
mod structs;
mod tuples;
mod typed_binding;
mod union_member;

use std::collections::HashSet;

use koja_ast::ast::{Diagnostic, ExprKind, Name, Pattern};
use koja_ast::identifier::{GlobalRegistryId, LocalId, Resolution, ResolvedType};
use koja_ast::labels::{pattern_kind_label, pattern_span};
use koja_typecheck::{Substitution, substitute};

use super::arms::lower_result_ty;
use super::ctx::FnLowerCtx;
use super::package::resolved_type_to_ir_type;
use super::unions::emit_union_narrow;
use crate::enum_decl::IRVariantTag;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

use literals::emit_literal_eq;
use typed_binding::{binds_whole_subject, typed_binding_ir_type};

/// Collect every [`LocalId`] a pattern binds, recursing through
/// enum / struct / list / constructor payloads and OR branches.
/// Binary-pattern segment bindings carry their ids on the segment's
/// `Ident` expression resolution.
pub(super) fn pattern_binding_ids(pattern: &Pattern) -> HashSet<LocalId> {
    let mut ids = HashSet::new();
    collect_pattern_binding_ids(pattern, &mut ids);
    ids
}

fn collect_pattern_binding_ids(pattern: &Pattern, ids: &mut HashSet<LocalId>) {
    match pattern {
        Pattern::Binding { local_id, .. } | Pattern::TypedBinding { local_id, .. } => {
            if let Some(id) = local_id {
                ids.insert(*id);
            }
        }
        Pattern::Binary { segments, .. } => {
            for segment in segments {
                if let ExprKind::Ident {
                    resolution: Resolution::Local(id),
                    ..
                } = &segment.value.kind
                {
                    ids.insert(*id);
                }
            }
        }
        Pattern::Constructor { elements, .. }
        | Pattern::EnumTuple { elements, .. }
        | Pattern::List { elements, .. }
        | Pattern::Tuple { elements, .. } => {
            for element in elements {
                collect_pattern_binding_ids(element, ids);
            }
        }
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for field in fields {
                collect_pattern_binding_ids(&field.pattern, ids);
            }
        }
        Pattern::Or { patterns, .. } => {
            for branch in patterns {
                collect_pattern_binding_ids(branch, ids);
            }
        }
        Pattern::UnionMember { pattern, .. } => collect_pattern_binding_ids(pattern, ids),
        Pattern::EnumUnit { .. } | Pattern::Literal { .. } | Pattern::Wildcard { .. } => {}
    }
}

/// The subject a pattern is matched against, threaded through every
/// recursive helper. Nested patterns rebind it to the projected
/// field or payload value.
pub(super) struct PatternInputs<'a> {
    pub(super) subject: ValueId,
    pub(super) subject_ty: &'a ResolvedType,
}

/// What the `match` driver needs to wire after lowering one arm's
/// pattern against the subject.
pub(super) enum PatternCheck {
    /// Pattern fires unconditionally (wildcard / binding / struct
    /// destructure whose every listed field is itself a catch-all).
    /// `binds` carries any field-extraction binds the driver must
    /// emit at the head of the success block before running the
    /// guard / body. Wildcard and binding always have empty binds,
    /// while struct destructure carries one entry per named binding field
    /// (potentially through nested fields).
    CatchAll { binds: Vec<PayloadBind> },
    /// One or more chained predicates. `chain_mode` decides the
    /// wiring: [`ChainMode::And`] for struct/enum field-test chains
    /// (every step must succeed for the arm to fire), and
    /// [`ChainMode::Or`] for or-pattern alternatives (any step
    /// succeeding fires the arm). Single-step patterns
    /// (`Literal` / `EnumUnit` / a struct or enum whose interior
    /// reduces to one test) admit either mode, and lowerers default
    /// to [`ChainMode::And`] for those.
    Tests {
        chain_mode: ChainMode,
        payload_binds: Vec<PayloadBind>,
        steps: Vec<TestStep>,
    },
}

/// Wiring discipline for a chain of [`TestStep`]s. Drives the
/// `then`/`else` choice in `match_expr::wire_test_chain`.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ChainMode {
    /// Every step must succeed for the arm to fire. Interior
    /// successes chain to the next step's `test_block`, and any
    /// failure short-circuits to the caller-supplied fall-through.
    And,
    /// Any step succeeding fires the arm. Interior failures chain
    /// to the next step's `test_block`, and only the last step's
    /// failure reaches the caller-supplied fall-through.
    Or,
}

/// One predicate gating arm execution. The test instructions are
/// already emitted into `test_block`. The driver sets that block's
/// terminator to a `CondBranch` keyed on `cond`.
pub(super) struct TestStep {
    pub(super) cond: ValueId,
    pub(super) test_block: IRBlockId,
}

/// One field binding emitted on the success edge. The driver
/// applies `chain` to the outer subject (one extraction op per
/// nesting level) and writes the final value into `local`.
pub(super) struct PayloadBind {
    pub(super) local: IRLocalId,
    pub(super) chain: Vec<BindStep>,
}

/// One extraction step in a [`PayloadBind`]'s chain. `output_type`
/// is the type of the value this step produces (handed to the next
/// step or to `LocalWrite` if this is the last step).
#[derive(Clone)]
pub(super) struct BindStep {
    pub(super) op: BindOp,
    pub(super) output_type: IRType,
}

/// Which IR instruction a [`BindStep`] emits. `EnumPayloadField`
/// emits `EnumPayloadFieldGet` (tag-gated, safe only when the
/// enclosing tag test has already fired). `StructField` emits
/// `FieldGet` (no tag, since the input is already a struct).
/// `TupleElement` emits `TupleGet` (index-addressed, no decl).
/// `UnionPayload` emits `UnionPayloadGet` (tag-gated extraction
/// against a tagged union member). `UnionNarrow` is the one step
/// that is not a single instruction. It re-tags a union of type
/// `source_type` into the sub-union that is the step's `output_type`,
/// through the tag switch of
/// [`emit_union_narrow`]. It binds a leaf, so it is never the prefix
/// of a nested pattern.
#[derive(Clone)]
pub(super) enum BindOp {
    EnumPayloadField {
        enum_symbol: IRSymbol,
        payload_index: u32,
        tag: IRVariantTag,
    },
    StructField {
        field_index: u32,
        struct_symbol: IRSymbol,
    },
    TupleElement {
        index: u32,
    },
    UnionNarrow {
        source_type: IRType,
    },
    UnionPayload {
        member_index: u8,
        member_type: IRType,
        union_type: IRType,
    },
}

/// Emit `step` in `block` with `source` as its input. Returns the
/// value that the step produces and the block that control continues
/// in. That block is `block` for every step that is one instruction.
/// A [`BindOp::UnionNarrow`] is a tag switch, so it returns its merge
/// block.
pub(super) fn emit_bind_step(
    step: &BindStep,
    source: ValueId,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> (ValueId, IRBlockId) {
    let output_type = &step.output_type;
    match &step.op {
        BindOp::EnumPayloadField {
            enum_symbol,
            payload_index,
            tag,
        } => emit_projection(output_type, ctx, block, |dest| {
            IRInstruction::EnumPayloadFieldGet {
                dest,
                field_type: output_type.clone(),
                payload_index: *payload_index,
                tag: *tag,
                ty: enum_symbol.clone(),
                value: source,
            }
        }),
        BindOp::StructField {
            field_index,
            struct_symbol,
        } => emit_projection(output_type, ctx, block, |dest| IRInstruction::FieldGet {
            base: source,
            dest,
            field_index: *field_index,
            field_type: output_type.clone(),
            struct_symbol: struct_symbol.clone(),
        }),
        BindOp::TupleElement { index } => {
            emit_projection(output_type, ctx, block, |dest| IRInstruction::TupleGet {
                base: source,
                dest,
                element_type: output_type.clone(),
                index: *index,
            })
        }
        BindOp::UnionNarrow { source_type } => {
            emit_union_narrow(source, source_type, output_type, ctx, block)
        }
        BindOp::UnionPayload {
            member_index,
            member_type,
            union_type,
        } => emit_projection(output_type, ctx, block, |dest| {
            IRInstruction::UnionPayloadGet {
                dest,
                member_index: *member_index,
                member_type: member_type.clone(),
                ty: union_type.clone(),
                value: source,
            }
        }),
    }
}

/// Emit the one instruction that `build` makes for a fresh value of
/// type `output_type`. Returns that value and `block`.
fn emit_projection(
    output_type: &IRType,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
    build: impl FnOnce(ValueId) -> IRInstruction,
) -> (ValueId, IRBlockId) {
    let dest = ctx.fresh_value(output_type.clone());
    ctx.cfg.append(block, build(dest));
    (dest, block)
}

pub(super) fn lower_pattern_check(
    pattern: &Pattern,
    inputs: PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(PatternCheck, IRBlockId), ()> {
    match pattern {
        Pattern::Binary { segments, .. } => {
            let cond =
                super::binary_match::lower_binary_pattern(segments, inputs.subject, ctx, block);
            Ok(single_test(cond, block))
        }
        Pattern::Binding { local_id, name, .. } => {
            lower_binding_check(*local_id, name, &inputs, ctx, block);
            Ok((PatternCheck::CatchAll { binds: Vec::new() }, block))
        }
        Pattern::EnumStruct {
            fields,
            type_path: _,
            variant,
            ..
        } => enums::lower_enum_struct_check(variant, fields, &inputs, ctx, block),
        Pattern::EnumTuple {
            elements,
            type_path: _,
            variant,
            ..
        } => enums::lower_enum_tuple_check(variant, elements, &inputs, ctx, block),
        Pattern::EnumUnit { variant, .. } => {
            let cond = enums::emit_enum_tag_eq(variant, &inputs, ctx, block);
            Ok(single_test(cond, block))
        }
        Pattern::Literal {
            literal_coercion,
            span,
            value,
        } => {
            let cond = emit_literal_eq(
                value,
                literal_coercion.as_ref(),
                *span,
                inputs.subject,
                ctx,
                block,
            )?;
            Ok(single_test(cond, block))
        }
        Pattern::Or { patterns, .. } => {
            Ok(or_pattern::lower_or_check(patterns, &inputs, ctx, block))
        }
        Pattern::Struct { fields, .. } => {
            Ok(structs::lower_struct_check(fields, &inputs, ctx, block))
        }
        Pattern::Tuple { elements, .. } => {
            Ok(tuples::lower_tuple_check(elements, &inputs, ctx, block))
        }
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            ..
        } => Ok(typed_binding::lower_typed_binding_check(
            *local_id,
            name,
            resolved_type,
            &inputs,
            ctx,
            block,
        )),
        Pattern::UnionMember {
            member_type,
            pattern,
            ..
        } => union_member::lower_union_member_check(member_type, pattern, &inputs, ctx, block),
        Pattern::Wildcard { .. } => Ok((PatternCheck::CatchAll { binds: Vec::new() }, block)),
        other => {
            ctx.output.diagnostics.push(Diagnostic::error(
                format!(
                    "IR does not yet lower match pattern `{}`",
                    pattern_kind_label(other),
                ),
                pattern_span(other),
            ));
            Err(())
        }
    }
}

fn lower_binding_check(
    local_id: Option<LocalId>,
    name: &Name,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) {
    let id = local_id.unwrap_or_else(|| {
        panic!(
            "IR lower: match binding `{name}` reaches lower without a stamped \
             LocalId (typecheck resolve invariant violation)",
        );
    });
    let ir_local = IRLocalId::from_local_id(id);
    // The bind writes the subject value without a `Clone`, so the
    // slot borrows and no drop site may free it.
    ctx.mark_slot_borrowed(ir_local);
    if !ctx.local_is_declared(ir_local) {
        let ty = lower_result_ty(inputs.subject_ty, ctx);
        let entry = ctx.entry_block();
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: ty.clone(),
            },
        );
        ctx.mark_local_declared(ir_local, ty);
    }
    ctx.cfg.append(
        block,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: inputs.subject,
        },
    );
}

/// The registry id behind a `Named` type that typecheck sealed to a
/// global. `what` names the pattern site in the panic ("struct
/// pattern subject", "enum subject").
pub(super) fn global_id_of(ty: &ResolvedType, what: &str) -> GlobalRegistryId {
    match ty {
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => *id,
        _ => panic!("IR lower: {what} has non-Global resolution after typecheck seal"),
    }
}

/// Pin a binding's `LocalId` invariant. Every pattern binding must
/// already carry a stamped id from the typecheck-resolve walk.
pub(super) fn require_local(local_id: Option<LocalId>, name: &Name) -> IRLocalId {
    let id = local_id.unwrap_or_else(|| {
        panic!(
            "IR lower: pattern binding `{name}` reaches lower without a \
             stamped LocalId (typecheck-resolve invariant violation)",
        );
    });
    IRLocalId::from_local_id(id)
}

/// Substitute the subject's type-args into a declared field type
/// and lower the result to its [`IRType`]. Shared by every per-
/// field bind helper so generic enum-payload / struct-field types
/// instantiate uniformly. Returns the substituted [`ResolvedType`]
/// alongside the [`IRType`] so callers that recurse into nested
/// patterns can hand the subject type to the inner resolver
/// without re-running the substitution.
pub(super) fn field_type_for(
    declared_ty: &ResolvedType,
    owner: GlobalRegistryId,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
) -> (ResolvedType, IRType) {
    let subject_args: &[ResolvedType] = match inputs.subject_ty {
        ResolvedType::Named { type_args, .. } => type_args,
        _ => &[],
    };
    let substituted = substitute(
        declared_ty,
        &Substitution::from_args(owner, subject_args),
        ctx.registry,
    );
    let ir_type = resolved_type_to_ir_type(&substituted, ctx.registry, ctx.output);
    (substituted, ir_type)
}

/// Hoist a payload-binding's `LocalDecl` to the function entry
/// block (idempotent per-local). Bind writes happen on the
/// success edge of the arm test, but seal expects every
/// `LocalWrite` to be dominated by exactly one `LocalDecl`, which
/// is always in entry.
///
/// Callers whose bind borrows the subject's payload storage (every
/// enum/struct/union pattern bind) must also
/// [`FnLowerCtx::mark_slot_borrowed`] the slot. A binary-match greedy
/// tail writes a freshly allocated block and must not.
pub(super) fn ensure_local_declared(local: IRLocalId, ty: &IRType, ctx: &mut FnLowerCtx<'_>) {
    if ctx.local_is_declared(local) {
        return;
    }
    let entry_block = ctx.entry_block();
    ctx.cfg.append(
        entry_block,
        IRInstruction::LocalDecl {
            local,
            ty: ty.clone(),
        },
    );
    ctx.mark_local_declared(local, ty.clone());
}

fn single_test(cond: ValueId, block: IRBlockId) -> (PatternCheck, IRBlockId) {
    (
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: Vec::new(),
            steps: vec![TestStep {
                cond,
                test_block: block,
            }],
        },
        block,
    )
}

/// The local that `pattern` binds to its whole subject with no test.
/// A plain binding always does. A typed binding does when
/// [`binds_whole_subject`] says so.
pub(super) fn whole_subject_local(
    pattern: &Pattern,
    subject_ir: &IRType,
    ctx: &mut FnLowerCtx<'_>,
) -> Option<IRLocalId> {
    match pattern {
        Pattern::Binding { local_id, name, .. } => Some(require_local(*local_id, name)),
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            ..
        } => {
            let bound_ir = typed_binding_ir_type(name, resolved_type, ctx);
            binds_whole_subject(&bound_ir, subject_ir).then(|| require_local(*local_id, name))
        }
        _ => None,
    }
}
