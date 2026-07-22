//! Pattern lowering: walk a [`Pattern`] against a subject `ValueId`
//! and produce a [`PatternCheck`] describing whether the arm fires
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
//! - [`literals`]: the `subject == const(value)` emission shared
//!   between the dispatcher's `Pattern::Literal` arm and the
//!   or-pattern literal-alternative arm.

mod enums;
mod literals;
mod or_pattern;
mod structs;
mod tuples;

use koja_ast::ast::{Diagnostic, Pattern};
use koja_ast::identifier::{GlobalRegistryId, LocalId, ResolvedType};
use koja_ast::labels::{pattern_kind_label, pattern_span};
use koja_typecheck::GlobalRegistry;

use crate::types::{ConstValue, IRBinOp};

use super::arms::lower_result_ty;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::package::resolved_type_to_ir_type;
use crate::enum_decl::IRVariantTag;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::substitute_resolved_type;
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

use literals::emit_literal_eq;

/// Read-only inputs threaded through every recursive helper.
/// Bundling them keeps `lower_pattern_check` and its per-shape
/// helpers under the clippy `too_many_arguments` threshold.
pub(super) struct PatternInputs<'a> {
    pub(super) registry: &'a GlobalRegistry,
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
/// `then`/`else` choice in [`super::match_expr::wire_test_chain`].
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
/// against a tagged union member).
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
    UnionPayload {
        member_index: u8,
        member_type: IRType,
        union_type: IRType,
    },
}

/// Emit `UnionTagGet(subject) == const(member_index)` and return
/// the resulting `Bool` value. Counterpart of
/// [`enums::emit_enum_tag_eq`] for the union family.
fn emit_union_tag_eq(
    subject: ValueId,
    subject_ir: &IRType,
    member_index: u8,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let tag_value = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::UnionTagGet {
            dest: tag_value,
            ty: subject_ir.clone(),
            value: subject,
        },
    );
    let const_dest = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: const_dest,
            value: ConstValue::Int8(member_index as i8),
        },
    );
    let cond = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest: cond,
            lhs: tag_value,
            op: IRBinOp::Eq,
            operand_ty: IRType::Int8,
            rhs: const_dest,
        },
    );
    cond
}

pub(super) fn lower_pattern_check(
    pattern: &Pattern,
    inputs: PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    match pattern {
        Pattern::Binary { segments, .. } => {
            let cond = super::binary_match::lower_binary_pattern(
                segments,
                inputs.subject,
                ctx,
                block,
                inputs.registry,
                output,
            );
            Ok(single_test(cond, block))
        }
        Pattern::Binding { local_id, name, .. } => {
            lower_binding_check(*local_id, name, &inputs, ctx, block, output);
            Ok((PatternCheck::CatchAll { binds: Vec::new() }, block))
        }
        Pattern::EnumStruct {
            fields,
            type_path: _,
            variant,
            ..
        } => enums::lower_enum_struct_check(variant, fields, &inputs, ctx, block, output),
        Pattern::EnumTuple {
            elements,
            type_path: _,
            variant,
            ..
        } => enums::lower_enum_tuple_check(variant, elements, &inputs, ctx, block, output),
        Pattern::EnumUnit { variant, .. } => {
            let cond = enums::emit_enum_tag_eq(variant, &inputs, ctx, block, output);
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
                &mut output.diagnostics,
            )?;
            Ok(single_test(cond, block))
        }
        Pattern::Or { patterns, .. } => Ok(or_pattern::lower_or_check(
            patterns, &inputs, ctx, block, output,
        )),
        Pattern::Struct { fields, .. } => Ok(structs::lower_struct_check(
            fields, &inputs, ctx, block, output,
        )),
        Pattern::Tuple { elements, .. } => Ok(tuples::lower_tuple_check(
            elements, &inputs, ctx, block, output,
        )),
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            ..
        } => {
            let resolved = resolved_type.as_ref().unwrap_or_else(|| {
                panic!(
                    "IR lower: typed-binding pattern `{name}` reaches lower without a \
                     resolved_type (typecheck-resolve invariant violation)",
                );
            });
            let member_ir =
                resolved_type_to_ir_type(resolved, inputs.registry, &mut output.instantiations);
            let subject_ir = ctx.type_of(inputs.subject).clone();
            let IRType::Union { members, .. } = &subject_ir else {
                panic!(
                    "IR lower: typed-binding pattern `{name}` reaches lower with \
                     non-Union subject `{subject_ir:?}`, typecheck-resolve invariant \
                     violation",
                );
            };
            let member_index = members
                .iter()
                .position(|m| m == &member_ir)
                .unwrap_or_else(|| {
                    panic!(
                        "IR lower: typed-binding pattern `{name}` member \
                         `{member_ir:?}` is not in subject union `{subject_ir:?}` \
                         (typecheck-resolve invariant violation)",
                    )
                }) as u8;
            let cond = emit_union_tag_eq(inputs.subject, &subject_ir, member_index, ctx, block);
            let local = require_local(*local_id, name);
            ensure_local_declared(local, &member_ir, ctx);
            ctx.mark_slot_borrowed(local);
            let bind = PayloadBind {
                local,
                chain: vec![BindStep {
                    output_type: member_ir.clone(),
                    op: BindOp::UnionPayload {
                        member_index,
                        member_type: member_ir,
                        union_type: subject_ir,
                    },
                }],
            };
            Ok((
                PatternCheck::Tests {
                    chain_mode: ChainMode::And,
                    payload_binds: vec![bind],
                    steps: vec![TestStep {
                        cond,
                        test_block: block,
                    }],
                },
                block,
            ))
        }
        Pattern::Wildcard { .. } => Ok((PatternCheck::CatchAll { binds: Vec::new() }, block)),
        other => {
            output.diagnostics.push(Diagnostic::error(
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
    name: &str,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
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
        let ty = lower_result_ty(inputs.subject_ty, inputs.registry, output);
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

/// Pin a binding's `LocalId` invariant: every pattern binding must
/// already carry a stamped id from the typecheck-resolve walk.
pub(super) fn require_local(local_id: Option<LocalId>, name: &str) -> IRLocalId {
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
    output: &mut LowerOutput,
) -> (ResolvedType, IRType) {
    let subject_args: &[ResolvedType] = match inputs.subject_ty {
        ResolvedType::Named { type_args, .. } => type_args,
        _ => &[],
    };
    let substituted = substitute_resolved_type(declared_ty, subject_args, owner);
    let ir_type =
        resolved_type_to_ir_type(&substituted, inputs.registry, &mut output.instantiations);
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
pub(super) fn ensure_local_declared(local: IRLocalId, ty: &IRType, ctx: &mut FnLowerCtx) {
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
