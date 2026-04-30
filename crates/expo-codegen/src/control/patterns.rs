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
use expo_ir::CFGBuilder;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier, VariantIdentifier};
use expo_ir::lower::patterns::resolve_subject_ty;
use expo_ir::lower::types::monomorphize_type;
use expo_ir::resolved::match_expr::ResolvedMatchType;
use expo_ir::values::{IROperand, IRValueId};
use expo_typecheck::types::{Type, TypeIdentifier, mangle_type};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;

use super::instructions::execute_instructions;
use super::terminator::materialize_operand;
use super::walk_function_blocks_seeded;

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
    let result_expo_ty = match &result_ty {
        ResolvedMatchType::Direct { ty } => ty.clone(),
        ResolvedMatchType::UnionWrap { target } => target.clone(),
    };
    let subject_alloca = compiler
        .builder
        .build_alloca(subject_tv.value.get_type(), "match_subject")
        .unwrap();
    compiler
        .builder
        .build_store(subject_alloca, subject_tv.value)
        .unwrap();

    let mut builder = CFGBuilder::new();
    let entry = compiler.fn_lower.next_block_id();
    builder.add_block(entry, "match_entry");
    let subject_id = compiler.fn_lower.next_value_id();
    let (_open, operand) = {
        let mut lowerer = compiler.lowerer();
        lowerer.lower_match_expr(
            &mut builder,
            entry,
            IROperand::Local(subject_id),
            subject_ty,
            arms,
            result_ty,
        )?
    };
    let (blocks, closed) = builder.into_blocks_with_closed();
    let result = walk_function_blocks_seeded(
        compiler,
        &blocks,
        &closed,
        function,
        if matches!(operand, IROperand::Unit) {
            None
        } else {
            Some(&operand)
        },
        &[(subject_id, subject_alloca.into())],
    )?;
    Ok(result.map(|v| TypedValue::new(v, result_expo_ty)))
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
