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

use expo_ir::identity::FunctionIdentifier;
use expo_ir::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};
use expo_ir::values::{IRInstruction, IROperand, IRValueId};
use expo_typecheck::types::Type;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::compiler::Compiler;
use crate::drop::Ownership;
use crate::expr::compile_expr;
use crate::ops::truncate_to_common_width;
use crate::stmt::coerce_numeric;
use crate::structs::emit_field_load;

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
/// recording the produced value under the instruction's SSA
/// destination. Returns the populated value map for the caller to
/// thread into [`super::emit_terminator`].
pub(crate) fn execute_instructions<'ctx>(
    compiler: &mut Compiler<'ctx>,
    instructions: &[IRInstruction],
    function: FunctionValue<'ctx>,
) -> Result<HashMap<IRValueId, BasicValueEnum<'ctx>>, String> {
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    for instruction in instructions {
        let entry = match instruction {
            IRInstruction::BinaryOp { dest, op, lhs, rhs } => {
                let l = materialize_operand(compiler, lhs, &value_map)?;
                let r = materialize_operand(compiler, rhs, &value_map)?;
                let value = emit_binary_op(compiler, op, l, r)?;
                Some((*dest, value))
            }
            IRInstruction::Call {
                dest,
                mangled,
                args,
                param_types,
                return_type: _,
            } => emit_call(compiler, *dest, mangled, args, param_types, &value_map)?,
            IRInstruction::FieldLoad { dest, base, step } => {
                let base_value = materialize_operand(compiler, base, &value_map)?;
                if !base_value.is_struct_value() {
                    return Err(
                        "IRInstruction::FieldLoad: base operand is not a struct value".to_string(),
                    );
                }
                let value = emit_field_load(compiler, base_value.into_struct_value(), step)?;
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
                &value_map,
            )?,
            IRInstruction::Stub { dest, expr } => {
                let value = compile_expr(compiler, expr, function)?
                    .ok_or("instruction stub expression produced no value")?
                    .value;
                Some((*dest, value))
            }
            IRInstruction::UnaryOp { dest, op, operand } => {
                let v = materialize_operand(compiler, operand, &value_map)?;
                let value = emit_unary_op(compiler, op, v)?;
                Some((*dest, value))
            }
        };
        if let Some((dest, value)) = entry {
            value_map.insert(dest, value);
        }
    }
    Ok(value_map)
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
