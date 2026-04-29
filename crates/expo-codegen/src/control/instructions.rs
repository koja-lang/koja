//! Shared emission for [`IRInstruction`] sequences within a basic block.
//!
//! Every conditional construct's `emit_*` walker calls
//! [`execute_instructions`] before dispatching the block's terminator.
//! The walker materializes each instruction in order, registers the
//! produced LLVM value under the instruction's SSA destination, and
//! returns the populated `value_map` for the surrounding emitter to
//! pass to [`super::emit_terminator`] when resolving operand
//! references.
//!
//! Instruction emission is mechanical -- all decision logic lives in
//! the resolved op variants ([`ResolvedBinaryOp`], [`ResolvedUnaryOp`])
//! attached at lowering time. Each match arm here maps a resolved
//! variant to the corresponding LLVM builder call(s) with no further
//! choice points.
//!
//! The transitional [`IRInstruction::Stub`] variant defers to
//! `compile_expr`. Its retirement is the long-running migration the
//! IR vocabulary expansion serves: each new typed instruction here
//! retires `Stub` for one [`expo_ast::ast::ExprKind`].

use std::collections::HashMap;

use expo_ast::ast::BinarySegment;
use expo_ir::IRBlockId;
use expo_ir::identity::FunctionIdentifier;
use expo_ir::resolved::fields::ResolvedChain;
use expo_ir::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};
use expo_ir::resolved::patterns::ResolvedLiteral;
use expo_ir::values::{IRInstruction, IROperand, IRValueId};
use expo_typecheck::types::Type;
use inkwell::AddressSpace;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PhiValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::binary::patterns::compile_binary_pattern;
use crate::compiler::Compiler;
use crate::control::patterns::{lookup_enum_struct_type, match_values, resolve_payload_info};
use crate::drop::Ownership;
use crate::expr::compile_expr;
use crate::ops::truncate_to_common_width;
use crate::stmt::coerce_numeric;
use crate::structs::{emit_chain_field_access, emit_field_load, load_maybe_indirect};
use crate::types::to_llvm_type;

use super::terminator::materialize_operand;

/// Lift the lift-helper output `(operand, return_type)` to a
/// [`crate::compiler::TypedValue`], handling void-returning callees
/// gracefully. Returns `None` when the operand's destination wasn't
/// inserted into the value map (the call's result was void), matching
/// the legacy `compile_call` / `compile_method_call` behavior of
/// returning `Ok(None)` for `Type::Unit` returns.
pub(crate) fn maybe_typed_value<'ctx>(
    compiler: &Compiler<'ctx>,
    operand: &IROperand,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
    return_type: Type,
) -> Result<Option<crate::compiler::TypedValue<'ctx>>, String> {
    if let IROperand::Local(id) = operand
        && !value_map.contains_key(id)
    {
        return Ok(None);
    }
    let value = super::terminator::materialize_operand(compiler, operand, value_map)?;
    Ok(Some(crate::compiler::TypedValue::new(value, return_type)))
}

/// Walk `instructions` in order, emitting LLVM IR for each and
/// recording the produced value into `value_map` under the
/// instruction's SSA destination. The caller owns the map's
/// lifetime so multi-block constructs (e.g. ternary's
/// `then` -> `else` -> `merge` chain) can share SSA values across
/// successive invocations -- the `IRInstruction::Phi` at the merge
/// references operands minted inside the arms, and a per-call
/// fresh map would lose them.
///
/// `block_map` is required to walk
/// [`IRInstruction::Phi`] -- its `add_incoming` calls need the LLVM
/// [`BasicBlock`] handles for each predecessor [`IRBlockId`].
/// Constructs that don't emit Phi (`unless`, `if`-no-else, single-block
/// instruction sequences in `compile_call` / struct construction) pass
/// `None` and the executor errors out cleanly if a Phi turns up
/// unexpectedly.
pub(crate) fn execute_instructions<'ctx>(
    compiler: &mut Compiler<'ctx>,
    instructions: &[IRInstruction],
    function: FunctionValue<'ctx>,
    block_map: Option<&HashMap<IRBlockId, BasicBlock<'ctx>>>,
    value_map: &mut HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<(), String> {
    for instruction in instructions {
        let entry = match instruction {
            IRInstruction::BinaryOp { dest, op, lhs, rhs } => {
                let l = materialize_operand(compiler, lhs, value_map)?;
                let r = materialize_operand(compiler, rhs, value_map)?;
                let value = emit_binary_op(compiler, op, l, r)?;
                Some((*dest, value))
            }
            IRInstruction::Call {
                dest,
                mangled,
                args,
                param_types,
                return_type: _,
            } => emit_call(compiler, *dest, mangled, args, param_types, value_map)?,
            IRInstruction::FieldChain {
                dest,
                base_name,
                base_type,
                steps,
            } => {
                let value = emit_field_chain(compiler, base_name, base_type, steps)?;
                Some((*dest, value))
            }
            IRInstruction::FieldLoad { dest, base, step } => {
                let base_value = materialize_operand(compiler, base, value_map)?;
                if !base_value.is_struct_value() {
                    return Err(
                        "IRInstruction::FieldLoad: base operand is not a struct value".to_string(),
                    );
                }
                let value = emit_field_load(compiler, base_value.into_struct_value(), step)?;
                Some((*dest, value))
            }
            IRInstruction::LoadConst { dest, name, ty: _ } => {
                let value = *compiler.constants.get(name).ok_or_else(|| {
                    format!("IRInstruction::LoadConst: unregistered constant `{name}`")
                })?;
                Some((*dest, value))
            }
            IRInstruction::LoadLocal { dest, name, ty } => {
                let value = emit_load_local(compiler, name, ty)?;
                Some((*dest, value))
            }
            IRInstruction::MakeFnRef {
                dest,
                name,
                fn_type: _,
            } => {
                let value = emit_make_fn_ref(compiler, name)?;
                Some((*dest, value))
            }
            IRInstruction::MethodCall {
                dest,
                mangled,
                receiver,
                receiver_name,
                is_move,
                args,
                param_types,
                return_type: _,
            } => emit_method_call(
                compiler,
                *dest,
                mangled,
                receiver,
                receiver_name.as_deref(),
                *is_move,
                args,
                param_types,
                value_map,
            )?,
            IRInstruction::PatternBinaryMatch {
                dest,
                subject_ptr,
                segments,
            } => {
                let value = emit_pattern_binary_match(
                    compiler,
                    subject_ptr,
                    segments,
                    function,
                    value_map,
                )?;
                Some((*dest, value.into()))
            }
            IRInstruction::PatternBindFromPtr {
                name,
                ty,
                source_ptr,
                strict_llvm,
            } => {
                emit_pattern_bind_from_ptr(
                    compiler,
                    name,
                    ty,
                    source_ptr,
                    *strict_llvm,
                    value_map,
                )?;
                None
            }
            IRInstruction::PatternLiteralEq {
                dest,
                subject_ptr,
                subject_ty,
                lit,
            } => {
                let value =
                    emit_pattern_literal_eq(compiler, subject_ptr, subject_ty, lit, value_map)?;
                Some((*dest, value.into()))
            }
            IRInstruction::PatternProjectVariantField {
                dest,
                subject_ptr,
                enum_key,
                variant,
                field_index,
                field_ty,
                name_hint,
            } => {
                let value = emit_pattern_project_variant_field(
                    compiler,
                    subject_ptr,
                    enum_key,
                    variant,
                    *field_index,
                    field_ty,
                    name_hint,
                    value_map,
                )?;
                Some((*dest, value.into()))
            }
            IRInstruction::PatternTagEq {
                dest,
                subject_ptr,
                enum_key,
                tag,
            } => {
                let value = emit_pattern_tag_eq(compiler, subject_ptr, enum_key, *tag, value_map)?;
                Some((*dest, value.into()))
            }
            IRInstruction::PatternUnionPayloadPtr {
                dest,
                subject_ptr,
                union_mangled,
            } => {
                let value = emit_pattern_union_payload_ptr(
                    compiler,
                    subject_ptr,
                    union_mangled,
                    value_map,
                )?;
                Some((*dest, value.into()))
            }
            IRInstruction::Phi {
                dest,
                incomings,
                ty,
            } => {
                let blocks = block_map.ok_or(
                    "IRInstruction::Phi: block_map required to resolve incoming predecessors",
                )?;
                let value = emit_phi(compiler, incomings, ty, blocks, value_map)?;
                Some((*dest, value))
            }
            IRInstruction::Stub { dest, expr } => {
                let value = compile_expr(compiler, expr, function)?
                    .ok_or("instruction stub expression produced no value")?
                    .value;
                Some((*dest, value))
            }
            IRInstruction::UnaryOp { dest, op, operand } => {
                let v = materialize_operand(compiler, operand, value_map)?;
                let value = emit_unary_op(compiler, op, v)?;
                Some((*dest, value))
            }
        };
        if let Some((dest, value)) = entry {
            value_map.insert(dest, value);
        }
    }
    Ok(())
}

/// Emit an [`IRInstruction::Call`]: materialize args, coerce against
/// the resolved parameter types, look up the LLVM `FunctionValue` in
/// `c.functions` (Wave 16 invariant guarantees presence for any
/// mangled symbol registered in [`expo_ir::program::IRProgram`]), and
/// build the LLVM call. Returns `None` for void-returning callees so
/// the caller skips the value-map insert; non-void returns produce
/// `Some((dest, value))`.
fn emit_call<'ctx>(
    c: &mut Compiler<'ctx>,
    dest: IRValueId,
    mangled: &FunctionIdentifier,
    args: &[IROperand],
    param_types: &[Type],
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<Option<(IRValueId, BasicValueEnum<'ctx>)>, String> {
    let callee = *c
        .functions
        .get(mangled)
        .ok_or_else(|| format!("IRInstruction::Call: unregistered callee `{mangled}`"))?;

    let llvm_args = build_call_args(c, args, param_types, value_map)?;
    let result = c.call(callee, &llvm_args, &format!("{mangled}_ret"));
    Ok(result.map(|v| (dest, v)))
}

/// Emit an [`IRInstruction::MethodCall`]: materialize the receiver as
/// the implicit `self` argument (no coercion -- receiver type is
/// concrete after resolution), materialize and coerce the remaining
/// args against `param_types[1..]`, build the LLVM call, then mirror
/// the legacy `compile_method_call` ownership update by marking the
/// receiver variable [`Ownership::Unowned`] when `is_move` and
/// `receiver_name` is set.
#[allow(clippy::too_many_arguments)]
fn emit_method_call<'ctx>(
    c: &mut Compiler<'ctx>,
    dest: IRValueId,
    mangled: &FunctionIdentifier,
    receiver: &IROperand,
    receiver_name: Option<&str>,
    is_move: bool,
    args: &[IROperand],
    param_types: &[Type],
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<Option<(IRValueId, BasicValueEnum<'ctx>)>, String> {
    let callee = *c
        .functions
        .get(mangled)
        .ok_or_else(|| format!("IRInstruction::MethodCall: unregistered callee `{mangled}`"))?;

    let recv_value = materialize_operand(c, receiver, value_map)?;
    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len() + 1);
    llvm_args.push(recv_value.into());
    // `param_types` for MethodCall excludes the implicit `self`
    // receiver (see `ResolvedMethodCall::param_types`), so the args
    // line up at index 0.
    let coerced = build_call_args(c, args, param_types, value_map)?;
    llvm_args.extend(coerced);

    let result = c.call(callee, &llvm_args, &format!("{mangled}_ret"));

    if is_move
        && let Some(name) = receiver_name
        && let Some((ptr, ty, _)) = c.fn_state.variables.get(name)
    {
        let entry = (*ptr, ty.clone(), Ownership::Unowned);
        c.fn_state.variables.insert(name.to_string(), entry);
    }

    Ok(result.map(|v| (dest, v)))
}

/// Emit an [`IRInstruction::FieldChain`]: rebuild a [`ResolvedChain`]
/// from the instruction's fields and dispatch to
/// [`emit_chain_field_access`], which walks the binding's storage
/// pointer with one GEP chain plus a final load. Restores the
/// static-chain GEP optimization at the IR level for chains rooted
/// at a named local (`a.b.c`, `self.origin.x`).
fn emit_field_chain<'ctx>(
    c: &mut Compiler<'ctx>,
    base_name: &str,
    base_type: &Type,
    steps: &[expo_ir::resolved::fields::ResolvedFieldStep],
) -> Result<BasicValueEnum<'ctx>, String> {
    let chain = ResolvedChain {
        base_name: base_name.to_string(),
        base_type: base_type.clone(),
        steps: steps.to_vec(),
    };
    let label = format!("chain_{base_name}");
    let result = emit_chain_field_access(c, &chain, &label).ok_or_else(|| {
        format!(
            "IRInstruction::FieldChain: cannot resolve LLVM type for chain rooted at `{base_name}`"
        )
    })?;
    let typed = result?.ok_or("IRInstruction::FieldChain: chain produced no value")?;
    Ok(typed.value)
}

/// Emit an [`IRInstruction::LoadLocal`]: look up the binding's
/// storage pointer in `Compiler.fn_state.variables` and emit a load
/// of `ty`'s LLVM representation. Mirrors the local-binding branch
/// of `compile_expr`'s `Ident` arm.
fn emit_load_local<'ctx>(
    c: &Compiler<'ctx>,
    name: &str,
    ty: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    let (ptr, _, _) = c.fn_state.variables.get(name).ok_or_else(|| {
        format!("IRInstruction::LoadLocal: binding `{name}` not in fn_state.variables")
    })?;
    let llvm_ty = to_llvm_type(ty, c.context, &c.llvm_types).ok_or_else(|| {
        format!("IRInstruction::LoadLocal: unsupported type for binding `{name}`: {ty:?}")
    })?;
    Ok(c.builder.build_load(llvm_ty, *ptr, name).unwrap())
}

/// Emit an [`IRInstruction::MakeFnRef`]: build (or reuse) a thunk
/// for the named top-level function and pair it with a null
/// environment pointer to produce a closure-compatible fat pointer.
/// Mirrors the function-as-value branch of `compile_expr`'s `Ident`
/// arm.
fn emit_make_fn_ref<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
) -> Result<BasicValueEnum<'ctx>, String> {
    let thunk = c.get_or_create_thunk(name)?;
    let ptr_ty = c.context.ptr_type(AddressSpace::default());
    let closure_struct_ty = c
        .context
        .struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
    let thunk_ptr = thunk.as_global_value().as_pointer_value();
    let null_env = ptr_ty.const_null();
    let mut fat_ptr = closure_struct_ty.get_undef();
    fat_ptr = c
        .builder
        .build_insert_value(fat_ptr, thunk_ptr, 0, "insert_fn")
        .unwrap()
        .into_struct_value();
    fat_ptr = c
        .builder
        .build_insert_value(fat_ptr, null_env, 1, "insert_env")
        .unwrap()
        .into_struct_value();
    Ok(fat_ptr.into())
}

/// Emit an [`IRInstruction::Phi`]: build an LLVM phi node and
/// register one incoming per `(IRBlockId, IROperand)` pair. Each
/// incoming operand is materialized through the same `value_map`
/// the surrounding executor uses, so values from `then` / `else`
/// arms threaded as `IROperand::Local` resolve correctly when the
/// merge block runs after the arms.
///
/// The phi's LLVM type is derived from the first materialized
/// incoming value's runtime type, which is always concrete --
/// `to_llvm_type(ty, ...)` is consulted only as a fallback (e.g.
/// for diagnostic logs) because Expo's `Type::Named` carrying
/// inferred-as-`Unknown` type args (common in stdlib Result
/// pipelines) fails the structural lookup, while the LLVM-side
/// type is fully resolved by the time the values land here.
fn emit_phi<'ctx>(
    c: &Compiler<'ctx>,
    incomings: &[(IRBlockId, IROperand)],
    ty: &Type,
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let mut materialized: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> =
        Vec::with_capacity(incomings.len());
    for (block_id, operand) in incomings {
        let llvm_block = *block_map.get(block_id).ok_or_else(|| {
            format!("IRInstruction::Phi: incoming block {block_id:?} not in block_map")
        })?;
        let value = materialize_operand(c, operand, value_map)?;
        materialized.push((value, llvm_block));
    }

    let llvm_ty = materialized
        .first()
        .map(|(value, _)| value.get_type())
        .or_else(|| to_llvm_type(ty, c.context, &c.llvm_types))
        .ok_or_else(|| format!("IRInstruction::Phi: no incomings and no fallback type ({ty:?})"))?;

    let phi: PhiValue<'ctx> = c.builder.build_phi(llvm_ty, "phi").unwrap();
    for (value, llvm_block) in &materialized {
        phi.add_incoming(&[(value, *llvm_block)]);
    }
    Ok(phi.as_basic_value())
}

/// Materialize each operand and coerce it against the matching
/// `param_types[i]`. Out-of-range arguments (variadic tail) pass
/// through without coercion. Mirrors the per-argument coercion loop in
/// the legacy `compile_call` / `compile_method_call` paths.
fn build_call_args<'ctx>(
    c: &mut Compiler<'ctx>,
    args: &[IROperand],
    param_types: &[Type],
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<Vec<BasicMetadataValueEnum<'ctx>>, String> {
    let mut out = Vec::with_capacity(args.len());
    for (i, operand) in args.iter().enumerate() {
        let value = materialize_operand(c, operand, value_map)?;
        let coerced = match param_types.get(i) {
            Some(target) => coerce_numeric(c, value, target),
            None => value,
        };
        out.push(coerced.into());
    }
    Ok(out)
}

/// Map a [`ResolvedBinaryOp`] to its LLVM builder call. Mirrors the
/// per-variant dispatch in `expo-codegen`'s `compile_binary` but
/// operates on already-materialized [`BasicValueEnum`] operands and
/// returns just the produced value (no [`crate::compiler::TypedValue`]
/// wrapping -- the per-block value map carries values only).
fn emit_binary_op<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &ResolvedBinaryOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    Ok(match op {
        ResolvedBinaryOp::BoolAnd => {
            let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
            c.builder.build_and(l, r, "and").unwrap().into()
        }
        ResolvedBinaryOp::BoolOr => {
            let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
            c.builder.build_or(l, r, "or").unwrap().into()
        }
        ResolvedBinaryOp::EnumStructEqual { .. } => {
            return Err(
                "EnumStructEqual is excluded from IRInstruction::BinaryOp; lowering should fall back to Stub"
                    .to_string(),
            );
        }
        ResolvedBinaryOp::FloatAdd => c
            .builder
            .build_float_add(lhs.into_float_value(), rhs.into_float_value(), "fadd")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatDiv => c
            .builder
            .build_float_div(lhs.into_float_value(), rhs.into_float_value(), "fdiv")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::OEQ, "feq"),
        ResolvedBinaryOp::FloatGreater => emit_float_cmp(c, lhs, rhs, FloatPredicate::OGT, "fgt"),
        ResolvedBinaryOp::FloatGreaterEqual => {
            emit_float_cmp(c, lhs, rhs, FloatPredicate::OGE, "fge")
        }
        ResolvedBinaryOp::FloatLess => emit_float_cmp(c, lhs, rhs, FloatPredicate::OLT, "flt"),
        ResolvedBinaryOp::FloatLessEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::OLE, "fle"),
        ResolvedBinaryOp::FloatMul => c
            .builder
            .build_float_mul(lhs.into_float_value(), rhs.into_float_value(), "fmul")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatNotEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::ONE, "fne"),
        ResolvedBinaryOp::FloatRem => c
            .builder
            .build_float_rem(lhs.into_float_value(), rhs.into_float_value(), "frem")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatSub => c
            .builder
            .build_float_sub(lhs.into_float_value(), rhs.into_float_value(), "fsub")
            .unwrap()
            .into(),
        ResolvedBinaryOp::IntAdd => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_add(l, r, "add").unwrap().into()
        }),
        ResolvedBinaryOp::IntDiv => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_signed_div(l, r, "sdiv").unwrap().into()
        }),
        ResolvedBinaryOp::IntEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::EQ, "eq"),
        ResolvedBinaryOp::IntGreater => emit_int_cmp(c, lhs, rhs, IntPredicate::SGT, "sgt"),
        ResolvedBinaryOp::IntGreaterEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::SGE, "sge"),
        ResolvedBinaryOp::IntLess => emit_int_cmp(c, lhs, rhs, IntPredicate::SLT, "slt"),
        ResolvedBinaryOp::IntLessEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::SLE, "sle"),
        ResolvedBinaryOp::IntMul => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_mul(l, r, "mul").unwrap().into()
        }),
        ResolvedBinaryOp::IntNotEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::NE, "ne"),
        ResolvedBinaryOp::IntRem => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_signed_rem(l, r, "srem").unwrap().into()
        }),
        ResolvedBinaryOp::IntSub => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_sub(l, r, "sub").unwrap().into()
        }),
        ResolvedBinaryOp::StringEqual => emit_string_cmp(c, lhs, rhs, IntPredicate::EQ)?,
        ResolvedBinaryOp::StringNotEqual => emit_string_cmp(c, lhs, rhs, IntPredicate::NE)?,
    })
}

fn emit_unary_op<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &ResolvedUnaryOp,
    operand: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    Ok(match op {
        ResolvedUnaryOp::FloatNeg => c
            .builder
            .build_float_neg(operand.into_float_value(), "fneg")
            .unwrap()
            .into(),
        ResolvedUnaryOp::IntNeg => c
            .builder
            .build_int_neg(operand.into_int_value(), "neg")
            .unwrap()
            .into(),
        ResolvedUnaryOp::IntNot => c
            .builder
            .build_not(operand.into_int_value(), "not")
            .unwrap()
            .into(),
    })
}

fn emit_int_arith_simple<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    build: impl FnOnce(
        &inkwell::builder::Builder<'ctx>,
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx>,
) -> BasicValueEnum<'ctx> {
    let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
    build(&c.builder, l, r)
}

fn emit_int_cmp<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: IntPredicate,
    name: &str,
) -> BasicValueEnum<'ctx> {
    let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
    c.builder
        .build_int_compare(pred, l, r, name)
        .unwrap()
        .into()
}

fn emit_float_cmp<'ctx>(
    c: &Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: FloatPredicate,
    name: &str,
) -> BasicValueEnum<'ctx> {
    c.builder
        .build_float_compare(pred, lhs.into_float_value(), rhs.into_float_value(), name)
        .unwrap()
        .into()
}

fn emit_string_cmp<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: IntPredicate,
) -> Result<BasicValueEnum<'ctx>, String> {
    let strcmp = *c
        .functions
        .get(&FunctionIdentifier::new("strcmp"))
        .ok_or("strcmp not declared")?;
    let cmp_result = c
        .call(
            strcmp,
            &[
                lhs.into_pointer_value().into(),
                rhs.into_pointer_value().into(),
            ],
            "strcmp_result",
        )
        .ok_or("strcmp did not return a value")?
        .into_int_value();
    let zero = c.context.i32_type().const_int(0, false);
    Ok(c.builder
        .build_int_compare(pred, cmp_result, zero, "str_cmp")
        .unwrap()
        .into())
}

/// Materialize an [`IROperand`] expected to resolve to a pointer value.
/// Used by every pattern primitive that consumes a subject / source
/// pointer; centralizes the diagnostic for the common error of feeding
/// a non-pointer operand into a pattern slot.
fn materialize_ptr_operand<'ctx>(
    c: &Compiler<'ctx>,
    operand: &IROperand,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
    instruction: &str,
) -> Result<inkwell::values::PointerValue<'ctx>, String> {
    let value = materialize_operand(c, operand, value_map)?;
    if !value.is_pointer_value() {
        return Err(format!("{instruction}: expected pointer operand"));
    }
    Ok(value.into_pointer_value())
}

/// Emit an [`IRInstruction::PatternTagEq`]: GEP the subject's tag slot
/// (struct index 0), load the i8 tag, and compare against `tag`.
/// Mirrors the legacy `emit_tag_check` body; `enum_key` is either an
/// enum cache key or a union mangled name (both expose the same
/// `lookup_enum_struct_type` registry slot).
fn emit_pattern_tag_eq<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: &IROperand,
    enum_key: &str,
    tag: u8,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<IntValue<'ctx>, String> {
    let subject = materialize_ptr_operand(c, subject_ptr, value_map, "PatternTagEq")?;
    let enum_type = lookup_enum_struct_type(c, enum_key)?;
    let tag_ptr = c
        .builder
        .build_struct_gep(enum_type, subject, 0, "tag_ptr")
        .unwrap();
    let tag_val = c
        .builder
        .build_load(c.context.i8_type(), tag_ptr, "tag")
        .unwrap()
        .into_int_value();
    let expected = c.context.i8_type().const_int(u64::from(tag), false);
    Ok(c.builder
        .build_int_compare(IntPredicate::EQ, tag_val, expected, "tag_eq")
        .unwrap())
}

/// Emit an [`IRInstruction::PatternLiteralEq`]: load the subject as
/// `subject_ty`, materialize the literal as an LLVM constant, and
/// dispatch into `match_values` (which handles the int width / float /
/// string-via-strcmp comparison details).
fn emit_pattern_literal_eq<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: &IROperand,
    subject_ty: &Type,
    lit: &ResolvedLiteral,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<IntValue<'ctx>, String> {
    let subject = materialize_ptr_operand(c, subject_ptr, value_map, "PatternLiteralEq")?;
    let llvm_ty = to_llvm_type(subject_ty, c.context, &c.llvm_types)
        .ok_or("PatternLiteralEq: cannot load subject for literal comparison")?;
    let subject_val = c.builder.build_load(llvm_ty, subject, "lit_subj").unwrap();
    let lit_val = materialize_pattern_literal(c, lit);
    match_values(c, &subject_val, &lit_val)
}

/// Materialize a [`ResolvedLiteral`] to a backend constant for use in
/// [`IRInstruction::PatternLiteralEq`]. String literals become global
/// `i8*` pointers; numeric / bool literals become inline constants.
fn materialize_pattern_literal<'ctx>(
    c: &Compiler<'ctx>,
    lit: &ResolvedLiteral,
) -> BasicValueEnum<'ctx> {
    match lit {
        ResolvedLiteral::Bool(b) => c.context.bool_type().const_int(u64::from(*b), false).into(),
        ResolvedLiteral::Float(v) => c.context.f64_type().const_float(*v).into(),
        ResolvedLiteral::Int(v) => c.context.i64_type().const_int(*v as u64, true).into(),
        ResolvedLiteral::String(s) => c
            .builder
            .build_global_string_ptr(s, "str_pat")
            .unwrap()
            .as_pointer_value()
            .into(),
    }
}

/// Emit an [`IRInstruction::PatternProjectVariantField`]: GEP into the
/// variant's payload (struct index 1 of the enum), GEP into the field
/// at `field_index`, `load_maybe_indirect` the field value, then
/// alloca + store to give the result a stable pointer (the new alloca
/// is returned). Used as the subject pointer for a recursive
/// sub-pattern or as the source pointer for a
/// [`IRInstruction::PatternBindFromPtr`].
#[allow(clippy::too_many_arguments)]
fn emit_pattern_project_variant_field<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: &IROperand,
    enum_key: &str,
    variant: &str,
    field_index: u32,
    field_ty: &Type,
    name_hint: &str,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<inkwell::values::PointerValue<'ctx>, String> {
    let subject = materialize_ptr_operand(c, subject_ptr, value_map, "PatternProjectVariantField")?;
    let info = resolve_payload_info(c, enum_key, variant)?;
    let payload_ptr = c
        .builder
        .build_struct_gep(info.enum_type, subject, 1, "payload_ptr")
        .unwrap();
    let field_ptr = c
        .builder
        .build_struct_gep(
            info.payload_type,
            payload_ptr,
            field_index,
            &format!("{name_hint}_ptr"),
        )
        .unwrap();
    let field_val = load_maybe_indirect(c, field_ptr, field_ty, &format!("{name_hint}_val"));
    // ZST fields use an i8 placeholder when `to_llvm_type` is `None`
    // (e.g. `()`), so the alloca shape stays in sync with the
    // monomorphized enum payload layout.
    let inner = expo_typecheck::types::unwrap_indirect(field_ty);
    let inner_llvm_ty =
        to_llvm_type(inner, c.context, &c.llvm_types).unwrap_or_else(|| c.context.i8_type().into());
    let alloca = c
        .builder
        .build_alloca(inner_llvm_ty, &format!("{name_hint}_tmp"))
        .unwrap();
    c.builder.build_store(alloca, field_val).unwrap();
    Ok(alloca)
}

/// Emit an [`IRInstruction::PatternUnionPayloadPtr`]: GEP into the
/// union's payload field (struct index 1). Mirrors the legacy
/// `get_union_payload_ptr`; the diagnostic on layout failure points to
/// the underlying "union body sized to tag-only" condition.
fn emit_pattern_union_payload_ptr<'ctx>(
    c: &Compiler<'ctx>,
    subject_ptr: &IROperand,
    union_mangled: &str,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<inkwell::values::PointerValue<'ctx>, String> {
    let subject = materialize_ptr_operand(c, subject_ptr, value_map, "PatternUnionPayloadPtr")?;
    let union_type = lookup_enum_struct_type(c, union_mangled)?;
    c.builder
        .build_struct_gep(union_type, subject, 1, "payload_ptr")
        .map_err(|_| {
            format!(
                "union `{union_mangled}` has no payload field at index 1; \
                 its body was sized to tag-only, likely because member \
                 bodies were not yet defined when the union was laid out"
            )
        })
}

/// Emit an [`IRInstruction::PatternBindFromPtr`]: load the bound value
/// from `source_ptr` as `ty`, alloca + store, register the binding in
/// `Compiler.fn_state.variables`. Side effect only -- no SSA value is
/// produced. Mirrors the legacy `emit_bind`.
fn emit_pattern_bind_from_ptr<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    ty: &Type,
    source_ptr: &IROperand,
    strict_llvm: bool,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<(), String> {
    let source = materialize_ptr_operand(c, source_ptr, value_map, "PatternBindFromPtr")?;
    let llvm_ty = if strict_llvm {
        to_llvm_type(ty, c.context, &c.llvm_types)
            .ok_or_else(|| format!("PatternBindFromPtr: unsupported type for `{name}`: {ty:?}"))?
    } else {
        to_llvm_type(ty, c.context, &c.llvm_types).unwrap_or_else(|| c.context.i8_type().into())
    };
    let val = c.builder.build_load(llvm_ty, source, name).unwrap();
    let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
    c.builder.build_store(alloca, val).unwrap();
    c.fn_state
        .variables
        .insert(name.to_string(), (alloca, ty.clone(), Ownership::Unowned));
    Ok(())
}

/// Emit an [`IRInstruction::PatternBinaryMatch`]: thin wrapper around
/// `compile_binary_pattern` (which is multi-block and has its own
/// internal control flow, so it stays as a single instruction at the
/// IR seam rather than being decomposed).
fn emit_pattern_binary_match<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: &IROperand,
    segments: &[BinarySegment],
    function: FunctionValue<'ctx>,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<IntValue<'ctx>, String> {
    let subject = materialize_ptr_operand(c, subject_ptr, value_map, "PatternBinaryMatch")?;
    compile_binary_pattern(c, segments, subject, function)
}
