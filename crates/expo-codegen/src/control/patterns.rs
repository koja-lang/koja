//! Pattern matching compilation: `match` expression walker and the public
//! `compile_pattern` entry point used by `receive` arms and the
//! `ExprKind::PatternMatch` (`is`) operator.
//!
//! Both paths route through [`expo_ir::Lowerer::lower_pattern_to_instructions`],
//! which produces a [`expo_ir::lower::patterns::LoweredPattern`] -- a pair
//! of instruction streams (`check_instructions` for the i1, `bind_instructions`
//! for binding setup) plus the [`expo_ir::values::IROperand`] referencing
//! the i1. Emission walks the streams through the standard
//! [`super::instructions::execute_instructions`] machinery; the LLVM
//! builder calls live in `instructions.rs` (one arm per pattern primitive).
//!
//! Slice 5b retired the codegen-side `emit_pattern` walker (and its
//! tag-check / bind / literal-eq / binary-pattern helpers); pattern
//! testing is now fully encoded as IR. The `fn_state.variables`
//! clone/restore around match arm bodies retired in lockstep -- bindings
//! emit only when the body block runs, never speculatively.

use std::collections::HashMap;

use expo_ast::ast::{Expr, MatchArm, Pattern, Statement};
use expo_ir::IRBlockId;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier, VariantIdentifier};
use expo_ir::lower::patterns::resolve_subject_ty;
use expo_ir::lower::types::monomorphize_type;
use expo_ir::resolved::match_expr::{IRMatch, IRMatchArm, ResolvedMatchType};
use expo_ir::values::{IROperand, IRValueId};
use expo_typecheck::types::{Type, TypeIdentifier, mangle_type};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::stmt::compile_union_wrap;

use super::compile_body_as_value;
use super::instructions::execute_instructions;
use super::terminator::{emit_terminator, materialize_operand};

/// Compiles a `match` expression. Patterns are tested sequentially; the first
/// matching arm executes. Bindings introduced by patterns are scoped to their
/// arm. Returns a phi value when all arms produce a value of the same type.
///
/// Today's ordering is: emit the subject first (so its post-emit Expo type is
/// available), then [`expo_ir::Lowerer::lower_match_expr`] resolves all arm
/// patterns into per-arm `check_instructions` + `bind_instructions` streams,
/// picks the result-type strategy from typecheck info, and mints the per-arm
/// IR block ids. [`emit_match_unified`] then walks the resulting [`IRMatch`]
/// through the same `execute_instructions` + `emit_terminator` machinery the
/// conditional walkers use.
///
/// ### Why pre-emit (and not pure lower-then-emit)
///
/// The lower/emit split would prefer lowering to run *before* any emission.
/// That requires `subject.resolved_type` (and every other expression's
/// `resolved_type`) to be populated by typecheck on the AST that codegen
/// monomorphizes from. It currently isn't: collect.rs clones impl blocks
/// into `ctx.generic_impl_asts` / `ctx.specialized_impl_asts` *before*
/// check.rs runs, and codegen reads those clones rather than the
/// typechecked AST in `module.items`. Until that AST-clone story is fixed
/// (see fix-generic-impl-typecheck plan, Stage 5), we lean on the
/// post-emit `subject_tv.expo_type`. The Ident/Self_ branch in
/// `resolve_subject_ty` is a final defensive fallback for residual gaps.
///
/// ### Behavior change vs the pre-IR implementation
///
/// The "Direct vs UnionWrap" strategy is a typecheck decision (taken by
/// [`lower_result_ty`] from arm-body `resolved_type` plus the surrounding
/// function's union return-type hint). If LLVM phi types disagree with that
/// decision at emission time, [`emit_match_unified`] returns an error rather
/// than silently returning `Ok(None)`. This surfaces typecheck/codegen
/// disagreements instead of swallowing them.
pub fn compile_match<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject: &Expr,
    arms: &[MatchArm],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let subject_tv =
        compile_expr(compiler, subject, function)?.ok_or("match subject produced no value")?;
    let subject_ty = resolve_subject_ty(
        &compiler.lower_ctx(),
        subject,
        &subject_tv.expo_type,
        |name| {
            compiler
                .fn_state
                .variables
                .get(name)
                .map(|(_, ty, _)| ty.clone())
        },
    );
    let result_ty = lower_result_ty(compiler, arms);
    let subject_alloca = compiler
        .builder
        .build_alloca(subject_tv.value.get_type(), "match_subject")
        .unwrap();
    compiler
        .builder
        .build_store(subject_alloca, subject_tv.value)
        .unwrap();
    let ir = compiler
        .lowerer()
        .lower_match_expr(subject_ty, arms, result_ty)?;
    emit_match_unified(compiler, &ir, subject_alloca, function)
}

// ---------------------------------------------------------------------------
// Result-type strategy: AST arms -> ResolvedMatchType.
//
// Reads typecheck-supplied `resolved_type` from each arm's last expression,
// applying the surrounding function's monomorphization substitution. Decides
// the result-type strategy from typecheck data alone; no LLVM emission
// happens here. Stays in codegen because it consults
// `LLVMTypeCache::contains_monomorphized` for the union-wrap shortcut.
// ---------------------------------------------------------------------------

/// Decides the result-type strategy for a match expression from arm-body
/// typecheck data plus the surrounding function's union return-type hint.
///
/// "All arm types equal" -> [`ResolvedMatchType::Direct`]. "Arm types differ
/// but every value-producing arm is a member of the hinted union, and the
/// union itself is monomorphized" -> [`ResolvedMatchType::UnionWrap`].
/// Everything else falls back to `Direct` with the first contributing arm's
/// type; if emission later observes mismatched LLVM types under that
/// fallback, it errors.
fn lower_result_ty(compiler: &Compiler<'_>, arms: &[MatchArm]) -> ResolvedMatchType {
    let arm_types: Vec<Type> = arms
        .iter()
        .filter_map(|a| arm_value_type(a, compiler))
        .collect();

    if arm_types.is_empty() {
        return ResolvedMatchType::Direct { ty: Type::Unknown };
    }

    let first_mangled = mangle_type(&arm_types[0]);
    let all_eq = arm_types.iter().all(|t| mangle_type(t) == first_mangled);
    if all_eq {
        return ResolvedMatchType::Direct {
            ty: arm_types[0].clone(),
        };
    }

    if let Some(Type::Union(members)) = &compiler.fn_lower.return_type_hint {
        let target = Type::Union(members.clone());
        let target_mangled = mangle_type(&target);
        let all_members = arm_types.iter().all(|t| {
            matches!(t, Type::Union(_)) || members.iter().any(|m| mangle_type(m) == mangle_type(t))
        });
        if all_members
            && compiler
                .llvm_types
                .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&target_mangled))
        {
            return ResolvedMatchType::UnionWrap { target };
        }
    }

    ResolvedMatchType::Direct {
        ty: arm_types[0].clone(),
    }
}

/// The Expo type the arm body would yield to the match's phi, derived from
/// the last statement's expression. Returns `None` for arms whose last
/// statement is not an `Expr` (e.g. ends in a `let`); those arms contribute
/// no value to the phi at runtime.
fn arm_value_type(arm: &MatchArm, compiler: &Compiler<'_>) -> Option<Type> {
    let Statement::Expr(last) = arm.body.last()? else {
        return None;
    };
    last.resolved_type
        .as_ref()
        .map(|t| monomorphize_type(&compiler.lower_ctx(), t))
}

// ---------------------------------------------------------------------------
// Emission: IRMatch -> LLVM IR.
//
// Slice 5b made the per-arm checks self-contained `IRInstruction` streams
// dispatched through `execute_instructions`. Pattern primitives
// (`PatternTagEq` / `PatternLiteralEq` / `PatternProjectVariantField` /
// `PatternUnionPayloadPtr` / `PatternBindFromPtr` / `PatternBinaryMatch`)
// each have an arm in the executor that performs the LLVM builder calls.
// The arm body block runs `bind_instructions` (binding setup) before
// walking `body_stmts`; bindings exist only when the cond branch fires.
// ---------------------------------------------------------------------------

/// Outcome of walking a single match arm's body.
enum ArmEmission<'ctx> {
    /// Body ran to completion without a trailing-expression value. The
    /// body's `Branch(merge)` terminator has already been dispatched.
    /// Drives the legacy "partial production" decision: if any arm
    /// reaches this state and another arm produced a value, the merge
    /// phi is dropped silently.
    NoValue,
    /// Body self-terminated (early `return` / `panic`). No body
    /// terminator dispatched because the block already has one. Does
    /// not contribute an incoming to the merge phi and does not block
    /// phi construction either -- the legacy contract is that
    /// terminated arms are invisible to the value-merge decision.
    Terminated,
    /// Body produced a value at the captured end block. The body
    /// terminator is deferred to [`collect_match_incoming`], which
    /// applies the lowered result strategy (UnionWrap if needed) and
    /// branches to merge.
    Value {
        end_bb: BasicBlock<'ctx>,
        tv: TypedValue<'ctx>,
    },
}

/// Walks an [`IRMatch`] into LLVM IR. N-arm generalization of
/// [`super::conditionals::emit_cond`] with native pattern + guard
/// instruction streams driving the per-arm cond branch and a
/// strategy-applying merge-phi assembly that mirrors legacy semantics
/// exactly.
///
/// Allocates LLVM blocks for every `arm.check_block`, every
/// `arm.body_block`, the all-patterns-failed `fallthrough_block`, and
/// the shared `merge_block`. Branches into `arms[0].check_block` from
/// the call-site builder position (which holds the subject alloca). A
/// single `value_map` is threaded across all arms with `subject_alloca`
/// pre-registered under `ir.subject_value`, so every per-arm check
/// stream's pattern primitives resolve their `subject_ptr` operand
/// against the same storage.
///
/// For each arm: position at `check_block`, run [`emit_match_arm_check`]
/// (executes `check_instructions` against the shared `value_map`,
/// dispatches `check_terminator`); position at `body_block`, run
/// [`emit_match_arm_body`] (executes `bind_instructions` to register
/// bindings, walks `body_stmts` via [`compile_body_as_value`]).
///
/// After every arm: hand the value-producing arms to
/// [`assemble_match_phi`], which applies the lowered result strategy
/// (Direct vs UnionWrap), branches each value-producing arm to merge,
/// terminates the fallthrough block, and synthesizes the final phi at
/// `merge_block`.
fn emit_match_unified<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRMatch,
    subject_alloca: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let block_map = build_match_block_map(compiler, ir, function);
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    value_map.insert(ir.subject_value, subject_alloca.into());

    let first_check_bb = block_map[&ir.arms[0].check_block];
    compiler
        .builder
        .build_unconditional_branch(first_check_bb)
        .unwrap();

    let mut pending: Vec<(TypedValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
    let mut any_no_value = false;
    for ir_arm in &ir.arms {
        // Wrap each arm (check + body) in a `fn_state.variables`
        // clone/restore so per-arm pattern bindings (registered by
        // `PatternBindFromPtr` in `check_instructions`) and any
        // `let`-bindings inside the body don't leak into subsequent
        // arms or shadow outer-scope variables past the arm. Slice 5b
        // lifted the binding *setup* into IR; per-arm *scoping* still
        // lives here because the variables map carries LLVM-typed
        // allocas that are not part of the IR surface.
        let saved_vars = compiler.fn_state.variables.clone();
        emit_match_arm_check(compiler, ir_arm, &block_map, &mut value_map, function)?;
        let outcome = emit_match_arm_body(compiler, ir_arm, &block_map, &mut value_map, function)?;
        compiler.fn_state.variables = saved_vars;
        match outcome {
            ArmEmission::NoValue => any_no_value = true,
            ArmEmission::Terminated => {}
            ArmEmission::Value { tv, end_bb } => pending.push((tv, end_bb)),
        }
    }

    let fallthrough_bb = block_map[&ir.fallthrough_block];
    let merge_bb = block_map[&ir.merge_block];
    assemble_match_phi(
        compiler,
        ir,
        pending,
        any_no_value,
        fallthrough_bb,
        merge_bb,
    )
}

/// Allocate LLVM basic blocks for every [`IRBlockId`] referenced by an
/// [`IRMatch`] and return the map. Block labels mirror the legacy
/// `emit_match` naming so `.ll` output stays diff-friendly.
fn build_match_block_map<'ctx>(
    compiler: &Compiler<'ctx>,
    ir: &IRMatch,
    function: FunctionValue<'ctx>,
) -> HashMap<IRBlockId, BasicBlock<'ctx>> {
    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    for (i, arm) in ir.arms.iter().enumerate() {
        let label = if i == 0 {
            "match_test_0".to_string()
        } else {
            format!("match_test_{i}")
        };
        let bb = compiler.context.append_basic_block(function, &label);
        block_map.insert(arm.check_block, bb);
    }
    for (i, arm) in ir.arms.iter().enumerate() {
        let bb = compiler
            .context
            .append_basic_block(function, &format!("match_body_{i}"));
        block_map.insert(arm.body_block, bb);
    }
    let fallthrough_bb = compiler.context.append_basic_block(function, "match_none");
    block_map.insert(ir.fallthrough_block, fallthrough_bb);
    let merge_bb = compiler.context.append_basic_block(function, "match_end");
    block_map.insert(ir.merge_block, merge_bb);
    block_map
}

/// Walks one arm's check block. Runs `check_instructions` (pattern
/// primitives + `BoolAnd`/`BoolOr` fusion + lifted guard operand
/// stream) against the shared `value_map`, then dispatches
/// `check_terminator` (`CondBranch { cond: <pattern+guard operand>,
/// then: body_block, otherwise: <next_check or fallthrough> }`).
fn emit_match_arm_check<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir_arm: &IRMatchArm,
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    value_map: &mut HashMap<IRValueId, BasicValueEnum<'ctx>>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let check_bb = block_map[&ir_arm.check_block];
    compiler.builder.position_at_end(check_bb);
    execute_instructions(
        compiler,
        &ir_arm.check_instructions,
        function,
        Some(block_map),
        value_map,
    )?;
    emit_terminator(
        compiler,
        &ir_arm.check_terminator,
        block_map,
        value_map,
        function,
    )
}

/// Walks one arm's body block: walks the AST stub body via
/// [`compile_body_as_value`]. Pattern bindings already exist in
/// `Compiler.fn_state.variables` (registered by `PatternBindFromPtr`
/// instructions during [`emit_match_arm_check`]); their per-arm
/// scoping is enforced by the surrounding [`emit_match_unified`] loop.
/// Returns the captured trailing-expression value (when present) and
/// the actual end block (which may differ from `body_block` when the
/// body contains nested control flow).
fn emit_match_arm_body<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir_arm: &IRMatchArm,
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    value_map: &mut HashMap<IRValueId, BasicValueEnum<'ctx>>,
    function: FunctionValue<'ctx>,
) -> Result<ArmEmission<'ctx>, String> {
    let body_bb = block_map[&ir_arm.body_block];
    compiler.builder.position_at_end(body_bb);
    let arm_tv = compile_body_as_value(compiler, &ir_arm.body_stmts, function)?;
    let terminated = compiler.current_block_terminated();
    let end_bb = compiler.builder.get_insert_block().unwrap();

    if terminated {
        return Ok(ArmEmission::Terminated);
    }
    if let Some(tv) = arm_tv {
        return Ok(ArmEmission::Value { end_bb, tv });
    }

    emit_terminator(
        compiler,
        &ir_arm.body_terminator,
        block_map,
        value_map,
        function,
    )?;
    Ok(ArmEmission::NoValue)
}

/// Assembles the final merge phi for an [`IRMatch`]. Applies the lowered
/// result strategy (Direct vs UnionWrap) to each value-producing arm,
/// branches each to merge, terminates the fallthrough block, then
/// constructs the phi at `merge_block` with one incoming per
/// value-producing arm plus an `undef` from the fallthrough.
///
/// Mirrors the legacy `emit_match` semantics exactly:
/// - zero value-producing arms => `Ok(None)` (structural void).
/// - some-but-not-all => `Ok(None)` (no unified phi shape).
/// - all produced with matching LLVM types => `Ok(Some(TypedValue))`.
/// - all produced but LLVM types disagree under the strategy => `Err`.
fn assemble_match_phi<'ctx>(
    compiler: &mut Compiler<'ctx>,
    ir: &IRMatch,
    pending: Vec<(TypedValue<'ctx>, BasicBlock<'ctx>)>,
    any_no_value: bool,
    fallthrough_bb: BasicBlock<'ctx>,
    merge_bb: BasicBlock<'ctx>,
) -> ExprResult<'ctx> {
    compiler.builder.position_at_end(fallthrough_bb);
    compiler
        .builder
        .build_unconditional_branch(merge_bb)
        .unwrap();

    if pending.is_empty() {
        compiler.builder.position_at_end(merge_bb);
        return Ok(None);
    }

    if any_no_value {
        // Some arms produced a value while others ran to completion
        // without one -- no unified phi shape, drop the value silently
        // (matches legacy `emit_match` partial-production behavior).
        // Self-terminated arms (early `return` / `panic`) are
        // intentionally invisible to this decision.
        compiler.builder.position_at_end(merge_bb);
        return Ok(None);
    }

    let incoming = collect_match_incoming(compiler, &ir.result_ty, &pending, merge_bb)?;
    compiler.builder.position_at_end(merge_bb);

    let first_ty = incoming[0].0.get_type();
    if !incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
        return Err(format!(
            "match arms produced incompatible LLVM types under lowered strategy `{}`",
            describe_match_strategy(&ir.result_ty),
        ));
    }
    let undef = first_ty.const_zero();
    let phi = compiler.builder.build_phi(first_ty, "matchval").unwrap();
    for (value, bb) in &incoming {
        phi.add_incoming(&[(value, *bb)]);
    }
    phi.add_incoming(&[(&undef, fallthrough_bb)]);

    let result_type = match &ir.result_ty {
        ResolvedMatchType::UnionWrap { target } => target.clone(),
        ResolvedMatchType::Direct { ty } if !matches!(ty, Type::Unknown) => ty.clone(),
        // Fallback: lowering had no typecheck-derived result type (e.g.
        // an arm body whose `resolved_type` didn't propagate into the
        // cached impl-AST clone). Trust the post-emit Expo type of the
        // first value-producing arm instead. Once the AST-clone story
        // is fixed this branch can go away.
        ResolvedMatchType::Direct { .. } => pending[0].0.expo_type.clone(),
    };
    Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)))
}

/// Apply the lowered result strategy to each value-producing arm and
/// emit its `Branch(merge)` terminator. Returns one `(value, end_bb)`
/// per arm in iteration order. UnionWrap arms whose value already has
/// the union shape pass through unwrapped; Direct arms always pass
/// through untouched.
fn collect_match_incoming<'ctx>(
    compiler: &mut Compiler<'ctx>,
    result_ty: &ResolvedMatchType,
    pending: &[(TypedValue<'ctx>, BasicBlock<'ctx>)],
    merge_bb: BasicBlock<'ctx>,
) -> Result<Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)>, String> {
    let mut incoming = Vec::with_capacity(pending.len());
    for (tv, bb) in pending {
        compiler.builder.position_at_end(*bb);
        let final_val = match result_ty {
            ResolvedMatchType::UnionWrap { target } => {
                if matches!(tv.expo_type, Type::Union(_)) {
                    tv.value
                } else {
                    compile_union_wrap(compiler, tv.value, &tv.expo_type, target)?
                }
            }
            ResolvedMatchType::Direct { .. } => tv.value,
        };
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
        let end_bb = compiler.builder.get_insert_block().unwrap();
        incoming.push((final_val, end_bb));
    }
    Ok(incoming)
}

fn describe_match_strategy(strategy: &ResolvedMatchType) -> String {
    match strategy {
        ResolvedMatchType::Direct { ty } => format!("Direct({})", ty.display()),
        ResolvedMatchType::UnionWrap { target } => format!("UnionWrap({})", target.display()),
    }
}

/// Compiles a single pattern test against a subject pointer. Emits the
/// pattern's instruction stream (tests + binding setup, in source
/// order) at the current builder position and returns the resulting
/// i1. Pattern bindings are registered into
/// `Compiler.fn_state.variables` as a side effect; callers wrap their
/// arm dispatch in a `fn_state.variables` clone/restore to scope them.
///
/// Routes through [`expo_ir::Lowerer::lower_pattern_to_instructions`]
/// to produce the same instruction stream that `match` arms use, so
/// both paths share the codegen-side pattern primitives in
/// [`super::instructions::execute_instructions`].
pub(crate) fn compile_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    subject_ptr: PointerValue<'ctx>,
    subject_type: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let subject_id = compiler.lowerer().next_value_id();
    let lowered = compiler.lowerer().lower_pattern_to_instructions(
        pattern,
        subject_type,
        IROperand::Local(subject_id),
    )?;
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    value_map.insert(subject_id, subject_ptr.into());
    execute_instructions(
        compiler,
        &lowered.instructions,
        function,
        None,
        &mut value_map,
    )?;
    let result = materialize_operand(compiler, &lowered.check_result, &value_map)?;
    Ok(result.into_int_value())
}

// ---------------------------------------------------------------------------
// Shared helpers used by codegen-side pattern primitives in
// `instructions.rs` plus other codegen modules (`enums.rs` consumes
// `get_payload_ptr` and `match_values` for enum equality compilation).
// ---------------------------------------------------------------------------

pub(crate) fn lookup_enum_struct_type<'ctx>(
    compiler: &Compiler<'ctx>,
    enum_key: &str,
) -> Result<StructType<'ctx>, String> {
    compiler
        .llvm_types
        .get_concrete(&TypeIdentifier::from_qualified_name(enum_key))
        .or_else(|| {
            compiler
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(enum_key))
        })
        .ok_or_else(|| format!("unknown enum: {enum_key}"))
}

/// Resolved payload metadata for an enum variant.
pub(crate) struct ResolvedPayloadInfo<'ctx> {
    pub enum_type: StructType<'ctx>,
    pub payload_type: StructType<'ctx>,
}

/// Emission-only LLVM cache lookup; no semantic decision. Pulls the payload
/// and enum `StructType<'ctx>` for a variant straight from the LLVM type
/// registry so the surrounding GEP emitter can index into them.
pub(crate) fn resolve_payload_info<'ctx>(
    compiler: &Compiler<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<ResolvedPayloadInfo<'ctx>, String> {
    let id = VariantIdentifier::new(enum_name, variant);
    let payload_type = compiler
        .llvm_types
        .variant_payload(&id)
        .ok_or_else(|| format!("no payload type for {enum_name}.{variant}"))?;
    let enum_type = lookup_enum_struct_type(compiler, enum_name)?;
    Ok(ResolvedPayloadInfo {
        enum_type,
        payload_type,
    })
}

pub(crate) fn get_payload_ptr<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<(StructType<'ctx>, PointerValue<'ctx>), String> {
    let resolved = resolve_payload_info(compiler, enum_name, variant)?;
    let payload_ptr = compiler
        .builder
        .build_struct_gep(resolved.enum_type, subject_ptr, 1, "payload_ptr")
        .unwrap();
    Ok((resolved.payload_type, payload_ptr))
}

pub(crate) fn match_values<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject: &BasicValueEnum<'ctx>,
    lit: &BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    if subject.is_int_value() && lit.is_int_value() {
        let subj_iv = subject.into_int_value();
        let mut lit_iv = lit.into_int_value();
        let subj_bits = subj_iv.get_type().get_bit_width();
        let lit_bits = lit_iv.get_type().get_bit_width();
        if subj_bits != lit_bits {
            let target_ty = compiler.context.custom_width_int_type(subj_bits);
            lit_iv = if subj_bits < lit_bits {
                compiler
                    .builder
                    .build_int_truncate(lit_iv, target_ty, "lit_trunc")
                    .unwrap()
            } else {
                compiler
                    .builder
                    .build_int_s_extend(lit_iv, target_ty, "lit_sext")
                    .unwrap()
            };
        }
        Ok(compiler
            .builder
            .build_int_compare(IntPredicate::EQ, subj_iv, lit_iv, "lit_eq")
            .unwrap())
    } else if subject.is_float_value() && lit.is_float_value() {
        Ok(compiler
            .builder
            .build_float_compare(
                FloatPredicate::OEQ,
                subject.into_float_value(),
                lit.into_float_value(),
                "lit_feq",
            )
            .unwrap())
    } else if subject.is_pointer_value() && lit.is_pointer_value() {
        let strcmp = *compiler
            .functions
            .get(&FunctionIdentifier::new("strcmp"))
            .ok_or("strcmp not declared")?;
        let cmp_result = compiler
            .call(
                strcmp,
                &[
                    subject.into_pointer_value().into(),
                    lit.into_pointer_value().into(),
                ],
                "strcmp_result",
            )
            .ok_or("strcmp did not return a value")?
            .into_int_value();
        let zero = compiler.context.i32_type().const_int(0, false);
        Ok(compiler
            .builder
            .build_int_compare(IntPredicate::EQ, cmp_result, zero, "str_eq")
            .unwrap())
    } else {
        Err("unsupported literal pattern comparison".to_string())
    }
}
