//! Per-instruction dispatch. Every arm forwards to a sibling
//! `emit/*.rs` module that owns the concrete LLVM emission for that
//! instruction family. Keeping this file dispatch-only makes the
//! [`expo_alpha_ir::IRInstruction`] coverage trivially auditable and
//! keeps the implementations focused.

use expo_alpha_ir::IRInstruction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;

use super::binary_construct::emit_binary_construct;
use super::{ValueMap, calls, closures, concat, constants, enums, locals, lookup, ops, structs};

pub(super) fn emit_instruction<'ctx>(
    ctx: &EmitContext<'ctx>,
    instr: &IRInstruction,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match instr {
        IRInstruction::BinaryConstruct {
            dest,
            layout,
            segments,
        } => {
            let result = emit_binary_construct(ctx, *layout, segments, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(values, *lhs)?;
            let rhs_value = lookup(values, *rhs)?;
            let result = ops::emit_binary_op(ctx, *op, lhs_value, rhs_value)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, args } => {
            if let Some(result) = calls::emit_call(ctx, args, callee, values)? {
                values.insert(*dest, result);
            }
            Ok(())
        }
        IRInstruction::CallClosure {
            args,
            callee,
            dest,
            result_ty,
        } => {
            if let Some(result) =
                closures::emit_call_closure(ctx, *callee, args, result_ty, values)?
            {
                values.insert(*dest, result);
            }
            Ok(())
        }
        IRInstruction::Concat {
            dest,
            kind,
            lhs,
            rhs,
        } => {
            let lhs_value = lookup(values, *lhs)?;
            let rhs_value = lookup(values, *rhs)?;
            let result = concat::emit_concat(ctx, *kind, lhs_value, rhs_value)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            if let Some(constant) = constants::emit_const_instruction(ctx, value)? {
                values.insert(*dest, constant);
            }
            Ok(())
        }
        IRInstruction::DropLocal { local, ty } => locals::emit_drop_local(ctx, *local, ty),
        IRInstruction::EnumConstruct {
            dest,
            payload,
            tag,
            ty,
        } => {
            let result = enums::emit_enum_construct(ctx, payload, *tag, ty, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::EnumPayloadFieldGet {
            dest,
            field_type,
            payload_index,
            tag,
            ty,
            value,
        } => {
            let base = lookup(values, *value)?;
            let result = enums::emit_enum_payload_field_get(
                ctx,
                field_type,
                *payload_index,
                *tag,
                ty,
                base,
            )?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::EnumTagGet { dest, value, ty } => {
            let base = lookup(values, *value)?;
            let result = enums::emit_enum_tag_get(ctx, ty, base)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::FieldGet {
            base,
            dest,
            field_index,
            field_type,
            struct_symbol,
        } => {
            let base_value = lookup(values, *base)?;
            let result =
                structs::emit_field_get(ctx, base_value, *field_index, field_type, struct_symbol)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::LoadCapture {
            capture_index,
            dest,
            ty,
        } => {
            let value = closures::emit_load_capture(ctx, *capture_index, ty)?;
            values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LoadConst {
            dest,
            const_id,
            ty: _,
        } => {
            let value = constants::emit_load_const(ctx, const_id)?;
            values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalDecl { local, ty } => locals::emit_local_decl(ctx, *local, ty),
        IRInstruction::LocalRead { dest, local, ty } => {
            let value = locals::emit_local_read(ctx, *local, ty)?;
            values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalWrite {
            local,
            ownership: _,
            value,
        } => {
            let resolved = lookup(values, *value)?;
            locals::emit_local_write(ctx, *local, resolved)
        }
        IRInstruction::MakeClosure {
            body,
            captures,
            dest,
            ty: _,
        } => {
            let result = closures::emit_make_closure(ctx, body, captures, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::MoveOutLocal { dest, local, ty } => {
            let value = locals::emit_local_read(ctx, *local, ty)?;
            values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::StructInit { dest, fields, ty } => {
            let result = structs::emit_struct_init(ctx, fields, ty, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, operand } => {
            let operand_value = lookup(values, *operand)?;
            let result = ops::emit_unary_op(ctx, *op, operand_value)?;
            values.insert(*dest, result);
            Ok(())
        }
    }
}
