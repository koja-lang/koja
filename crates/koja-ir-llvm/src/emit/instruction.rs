//! Per-instruction dispatch. Every arm forwards to a sibling
//! `emit/*.rs` module that owns the concrete LLVM emission for that
//! instruction family and evaluates to the value the instruction
//! defines, or `None` for a pure side effect. The dispatcher binds
//! that value to [`IRInstruction::dest`] in one place, so no arm
//! touches the value map. Keeping this file dispatch-only makes the
//! [`IRInstruction`] coverage trivially auditable and keeps the
//! implementations focused.

use inkwell::values::BasicValueEnum;
use koja_ir::IRInstruction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::reductions::emit_yield_check;

use super::binary_construct::emit_binary_construct;
use super::binary_match;
use super::process::{SpawnArgs, emit_process_exit, emit_receive, emit_set_priority, emit_spawn};
use super::{
    ValueMap, calls, clone, closures, concat, constants, deep_copy, enums, indirect, locals,
    lookup, ops, structs, tuples, unions,
};

pub(crate) fn emit_instruction<'ctx>(
    ctx: &EmitContext<'ctx>,
    instr: &IRInstruction,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let result = emit_instruction_value(ctx, instr, values)?;
    match (instr.dest(), result) {
        (Some(dest), Some(value)) => {
            values.insert(dest, value);
        }
        (None, None) => {}
        // `Receive` ends its host block with the arm dispatch, and the
        // lowerer's `receive_merge` block param carries the result, so
        // the instruction's own `dest` is never read.
        (Some(_), None) if matches!(instr, IRInstruction::Receive { .. }) => {}
        (dest, value) => panic!(
            "LLVM emit: `{instr:?}` defines {dest:?} but its emitter produced {value:?} \
             (every arm must return a value exactly when the instruction has a `dest`)",
        ),
    }
    Ok(())
}

/// The value `instr` defines, or `None` for a pure side effect.
fn emit_instruction_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    instr: &IRInstruction,
    values: &ValueMap<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
    Ok(match instr {
        IRInstruction::BinaryConstruct {
            layout, segments, ..
        } => Some(emit_binary_construct(ctx, *layout, segments, values)?),
        IRInstruction::BinaryMatch {
            layout,
            segments,
            subject,
            ..
        } => {
            Some(binary_match::emit_binary_match(ctx, *layout, segments, *subject, values)?.into())
        }
        IRInstruction::BinaryOp {
            lhs,
            op,
            operand_ty,
            rhs,
            ..
        } => {
            let lhs_value = lookup(values, *lhs);
            let rhs_value = lookup(values, *rhs);
            let result = ops::emit_binary_op(ctx, *op, operand_ty, lhs_value, rhs_value)?;
            Some(result)
        }
        IRInstruction::Call { callee, args, .. } => {
            Some(calls::emit_call(ctx, args, callee, values)?)
        }
        IRInstruction::CallClosure {
            args,
            callee,
            param_types,
            result_ty,
            ..
        } => Some(closures::emit_call_closure(
            ctx,
            *callee,
            args,
            param_types,
            result_ty,
            values,
        )?),
        IRInstruction::Clone { source, ty, .. } => {
            Some(clone::emit_clone(ctx, *source, ty, values)?)
        }
        IRInstruction::ClosureEquals { lhs, rhs, ty, .. } => {
            Some(closures::emit_closure_equals(ctx, *lhs, *rhs, ty, values)?)
        }
        IRInstruction::Concat {
            consumes_lhs,
            kind,
            lhs,
            rhs,
            ..
        } => {
            let lhs_value = lookup(values, *lhs);
            let rhs_value = lookup(values, *rhs);
            let result = concat::emit_concat(ctx, *kind, *consumes_lhs, lhs_value, rhs_value)?;
            Some(result)
        }
        IRInstruction::Const { value, .. } => Some(constants::emit_const(ctx, value)),
        IRInstruction::ConsumeLocal { .. } => None,
        IRInstruction::DeepCopy { source, ty, .. } => {
            Some(deep_copy::emit_deep_copy(ctx, *source, ty, values)?)
        }
        IRInstruction::DropLocal { local, ty } => {
            locals::emit_drop_local(ctx, *local, ty)?;
            None
        }
        IRInstruction::DropValue { value, ty } => {
            locals::emit_drop_value(ctx, *value, ty, values)?;
            None
        }
        IRInstruction::EnumConstruct {
            payload, tag, ty, ..
        } => Some(enums::emit_enum_construct(ctx, payload, *tag, ty, values)?),
        IRInstruction::EnumPayloadFieldGet {
            field_type,
            payload_index,
            tag,
            ty,
            value,
            ..
        } => {
            let base = lookup(values, *value);
            Some(enums::emit_enum_payload_field_get(
                ctx,
                field_type,
                *payload_index,
                *tag,
                ty,
                base,
            )?)
        }
        IRInstruction::EnumTagGet { value, ty, .. } => {
            let base = lookup(values, *value);
            Some(enums::emit_enum_tag_get(ctx, ty, base)?)
        }
        IRInstruction::FieldGet {
            base,
            field_index,
            field_type,
            struct_symbol,
            ..
        } => {
            let base_value = lookup(values, *base);
            Some(structs::emit_field_get(
                ctx,
                base_value,
                *field_index,
                field_type,
                struct_symbol,
            )?)
        }
        IRInstruction::FieldSet {
            base,
            field_index,
            struct_symbol,
            value,
            ..
        } => {
            let base_value = lookup(values, *base);
            let new_field = lookup(values, *value);
            Some(structs::emit_field_set(
                ctx,
                base_value,
                *field_index,
                struct_symbol,
                new_field,
            )?)
        }
        IRInstruction::IndirectPresent { base, slot, .. } => {
            let base = lookup(values, *base);
            Some(indirect::emit_indirect_present(ctx, base, slot)?)
        }
        IRInstruction::LoadCapture {
            capture_index, ty, ..
        } => Some(closures::emit_load_capture(ctx, *capture_index, ty)?),
        IRInstruction::LoadCaptureOf {
            capture_index,
            closure,
            ty,
            ..
        } => Some(closures::emit_load_capture_of(
            ctx,
            *closure,
            *capture_index,
            ty,
            values,
        )?),
        IRInstruction::LoadConst { const_id, .. } => {
            Some(constants::emit_load_const(ctx, const_id)?)
        }
        IRInstruction::LocalDecl { local, ty } => {
            locals::emit_local_decl(ctx, *local, ty)?;
            None
        }
        IRInstruction::LocalRead { local, ty, .. } => {
            Some(locals::emit_local_read(ctx, *local, ty)?)
        }
        IRInstruction::LocalWrite { local, value } => {
            let resolved = lookup(values, *value);
            locals::emit_local_write(ctx, *local, resolved)?;
            None
        }
        IRInstruction::MakeClosure { body, captures, .. } => {
            Some(closures::emit_make_closure(ctx, body, captures, values)?)
        }
        IRInstruction::NumericWiden {
            from, to, value, ..
        } => {
            let source = lookup(values, *value);
            Some(ops::emit_numeric_widen(ctx, from, to, source)?)
        }
        IRInstruction::ProcessExit { reason } => {
            emit_process_exit(ctx, *reason, values)?;
            None
        }
        IRInstruction::Receive { after, arms, .. } => {
            emit_receive(ctx, after.as_ref(), arms, values)?;
            None
        }
        IRInstruction::SetPriority { tag } => {
            emit_set_priority(ctx, *tag, values)?;
            None
        }
        IRInstruction::Spawn {
            config,
            config_type,
            ref_type,
            wrapper,
            ..
        } => {
            let args = SpawnArgs {
                config: *config,
                config_type,
                ref_type,
                wrapper,
            };
            Some(emit_spawn(ctx, args, values)?)
        }
        IRInstruction::StructInit { fields, ty, .. } => {
            Some(structs::emit_struct_init(ctx, fields, ty, values)?)
        }
        IRInstruction::TupleGet { base, index, .. } => {
            let base_value = lookup(values, *base);
            Some(tuples::emit_tuple_get(ctx, base_value, *index)?)
        }
        IRInstruction::TupleInit { elements, ty, .. } => {
            Some(tuples::emit_tuple_init(ctx, elements, ty, values)?)
        }
        IRInstruction::UnaryOp {
            op,
            operand,
            operand_ty,
            ..
        } => {
            let operand_value = lookup(values, *operand);
            Some(ops::emit_unary_op(ctx, *op, operand_ty, operand_value)?)
        }
        IRInstruction::UnionPayloadGet {
            member_type,
            ty,
            value,
            ..
        } => {
            let base = lookup(values, *value);
            Some(unions::emit_union_payload_get(ctx, member_type, ty, base)?)
        }
        IRInstruction::UnionTagGet { ty, value, .. } => {
            let base = lookup(values, *value);
            Some(unions::emit_union_tag_get(ctx, ty, base)?)
        }
        IRInstruction::UnionWrap {
            member_index,
            member_type,
            ty,
            value,
            ..
        } => {
            let payload = lookup(values, *value);
            Some(unions::emit_union_wrap(
                ctx,
                *member_index,
                member_type,
                ty,
                payload,
            )?)
        }
        IRInstruction::YieldCheck => {
            emit_yield_check(ctx)?;
            None
        }
    })
}
