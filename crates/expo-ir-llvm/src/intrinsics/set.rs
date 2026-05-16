//! `Set<T>` family — heap-backed open-addressed hash table over
//! unique elements. Shares its struct layout with `Map` (see
//! [`crate::types::hashtable_value_type`]); each `Entry` is a single
//! `T` rather than a `(K, V)` pair, but the probe / resize / state
//! machinery is identical.

use expo_ir::{IRFunction, IRType, SetMethod};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::intrinsics::hashtable;
use inkwell::values::FunctionValue;

pub(super) fn emit_set<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: SetMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let element = element(method, function)?;
    let element_size = hashtable::ir_byte_size(ctx, element)?;
    let layout = hashtable::HashtableLayout {
        entry_size: element_size,
        key_size: element_size,
        key_ty: element,
        value_ty: None,
    };

    match method {
        SetMethod::EmptyQ => hashtable::emit_empty_q(ctx, function, llvm_function),
        SetMethod::FromList => hashtable::emit_set_from_list(ctx, function, llvm_function, &layout),
        SetMethod::HasQ => hashtable::emit_has_q(ctx, function, llvm_function, &layout),
        SetMethod::Insert => hashtable::emit_set_insert(ctx, function, llvm_function, &layout),
        SetMethod::Length => hashtable::emit_length(ctx, function, llvm_function),
        SetMethod::New => hashtable::emit_new(ctx, function, element_size),
        SetMethod::Remove => hashtable::emit_remove(ctx, function, llvm_function, &layout),
    }
}

/// Resolve the element `T` for a `Set<T>` intrinsic. `new` carries
/// it on the return type; every other method has `self: Set<T>` as
/// `params[0]`.
fn element(method: SetMethod, function: &IRFunction) -> Result<&IRType, LlvmError> {
    let candidate = match method {
        SetMethod::New => &function.return_type,
        SetMethod::FromList => &function.return_type,
        _ => &function.params[0].ty,
    };
    match candidate {
        IRType::Set(inner) => Ok(inner),
        other => Err(LlvmError::Codegen(format!(
            "Set.{method:?} expected a `Set<T>` slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}
