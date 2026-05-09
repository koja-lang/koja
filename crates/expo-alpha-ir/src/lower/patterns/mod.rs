//! Pattern lowering: walk a [`Pattern`] against a subject `ValueId`
//! and produce a [`PatternCheck`] describing whether the arm fires
//! unconditionally, after one or more chained predicates, and what
//! field binds (`EnumPayloadFieldGet` / `FieldGet` + `LocalWrite`)
//! the success edge needs to perform before the arm body runs. The
//! match driver in [`super::match_expr`] consumes the result to
//! wire the gating `CondBranch`(es) and the per-arm body block.
//!
//! Admits leaves (wildcard / binding / literal), `EnumUnit`,
//! `EnumTuple` / `EnumStruct` (one-level — payload elements / fields
//! restricted to wildcard / binding), `Struct` (same restriction —
//! always a `CatchAll` carrying field binds), and `Or` (alternatives
//! restricted to literal / EnumUnit, no bindings). Every other
//! shape is a feature gap diagnosed in typecheck and is unreachable
//! on the success path.
//!
//! # Module layout
//!
//! - [`enums`] — `EnumUnit` / `EnumTuple` / `EnumStruct` shapes,
//!   `emit_enum_tag_eq`, and the shared enum-payload metadata
//!   resolver.
//! - [`structs`] — plain-struct destructure (always `CatchAll` with
//!   per-field [`BindSource::StructField`] binds).
//! - [`or_pattern`] — `A | B | C` chained through fresh
//!   `match_or_alt_<n>` blocks, one [`TestStep`] per alternative.
//! - [`literals`] — the `subject == const(value)` emission shared
//!   between the dispatcher's `Pattern::Literal` arm and the
//!   or-pattern literal-alternative arm.

mod enums;
mod literals;
mod or_pattern;
mod structs;

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Diagnostic, Pattern};
use expo_ast::identifier::{GlobalRegistryId, LocalId, ResolvedType};
use expo_ast::labels::{pattern_kind_label, pattern_span};

use super::arms::lower_result_ty;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::package::resolved_type_to_ir_type;
use crate::enum_decl::IRVariantTag;
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::substitute_resolved_type;
use crate::local::IRLocalId;
use crate::ownership::Ownership;
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
    /// destructure). `binds` carries any field-extraction binds the
    /// driver must emit at the head of the success block before
    /// running the guard / body. Wildcard and binding always have
    /// empty binds; struct destructure carries one entry per named
    /// `Pattern::Binding` field.
    CatchAll { binds: Vec<PayloadBind> },
    /// One or more chained predicates. Length 1 for a single
    /// `Literal` / `EnumUnit` / `EnumTuple` / `EnumStruct` pattern;
    /// length n for an `Or` of n alternatives. The driver wires
    /// every step's success edge to the same success block, every
    /// interior step's failure edge to the next step's
    /// `test_block`, and the last step's failure edge to the
    /// caller-supplied fall-through.
    Tests {
        payload_binds: Vec<PayloadBind>,
        steps: Vec<TestStep>,
    },
}

/// One predicate gating arm execution. The test instructions are
/// already emitted into `test_block`; the driver sets that block's
/// terminator to a `CondBranch` keyed on `cond`.
pub(super) struct TestStep {
    pub(super) cond: ValueId,
    pub(super) test_block: IRBlockId,
}

/// One field binding emitted on the success edge. The driver
/// appends the right `*FieldGet` + `LocalWrite` pair to the head
/// of the success block before the arm body runs. Source variant
/// drives instruction selection.
pub(super) struct PayloadBind {
    pub(super) field_type: IRType,
    pub(super) local: IRLocalId,
    pub(super) source: BindSource,
}

/// Where a [`PayloadBind`] reads its value from. `EnumPayload`
/// emits `EnumPayloadFieldGet` (tag-gated extraction); `StructField`
/// emits `FieldGet` (no tag — the subject is already a struct).
pub(super) enum BindSource {
    EnumPayload {
        enum_symbol: IRSymbol,
        payload_index: u32,
        tag: IRVariantTag,
    },
    StructField {
        field_index: u32,
        struct_symbol: IRSymbol,
    },
}

pub(super) fn lower_pattern_check(
    pattern: &Pattern,
    inputs: PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    match pattern {
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
        Pattern::Literal { span, value } => {
            let cond = emit_literal_eq(
                value,
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
        Pattern::Struct { fields, .. } => Ok((
            structs::lower_struct_check(fields, &inputs, ctx, output),
            block,
        )),
        Pattern::Wildcard { .. } => Ok((PatternCheck::CatchAll { binds: Vec::new() }, block)),
        other => {
            output.diagnostics.push(Diagnostic::error(
                format!(
                    "alpha IR does not yet lower match pattern `{}`",
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
            "alpha IR lower: match binding `{name}` reaches lower without a stamped \
             LocalId — typecheck resolve invariant violation",
        );
    });
    let ir_local = IRLocalId::from_local_id(id);
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
            ownership: Ownership::Unowned,
            value: inputs.subject,
        },
    );
    ctx.mark_local_written(ir_local, Ownership::Unowned);
}

/// Pin a binding's `LocalId` invariant: every pattern binding must
/// already carry a stamped id from the typecheck-resolve walk.
pub(super) fn require_local(local_id: Option<LocalId>, name: &str) -> IRLocalId {
    let id = local_id.unwrap_or_else(|| {
        panic!(
            "alpha IR lower: pattern binding `{name}` reaches lower without a \
             stamped LocalId — typecheck-resolve invariant violation",
        );
    });
    IRLocalId::from_local_id(id)
}

/// Substitute the subject's type-args into a declared field type
/// and lower the result to its [`IRType`]. Shared by every per-
/// field bind helper so generic enum-payload / struct-field types
/// instantiate uniformly.
pub(super) fn field_type_for(
    declared_ty: &ResolvedType,
    owner: GlobalRegistryId,
    inputs: &PatternInputs<'_>,
    output: &mut LowerOutput,
) -> IRType {
    let subject_args: &[ResolvedType] = match inputs.subject_ty {
        ResolvedType::Named { type_args, .. } => type_args,
        _ => &[],
    };
    let substituted = substitute_resolved_type(declared_ty, subject_args, owner);
    resolved_type_to_ir_type(&substituted, inputs.registry, &mut output.instantiations)
}

/// Hoist a payload-binding's `LocalDecl` to the function entry
/// block (idempotent per-local). Bind writes happen on the
/// success edge of the arm test, but seal expects every
/// `LocalWrite` to be dominated by exactly one `LocalDecl`, which
/// is always in entry.
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
            payload_binds: Vec::new(),
            steps: vec![TestStep {
                cond,
                test_block: block,
            }],
        },
        block,
    )
}
