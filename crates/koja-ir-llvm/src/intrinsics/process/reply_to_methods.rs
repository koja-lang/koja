//! `ReplyTo<R>` `@intrinsic` emitters, the callee side of
//! `Ref.call`, answering through the caller's one-shot reply slot.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue};
use koja_ir::{IRFunction, IRSymbol, IRType};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::process::serialize_to_stack;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::{nth_param, nth_param_type};
use crate::runtime::declare_rt_reply_extern;
use crate::types::ir_basic_type;

use super::envelope::payload_drop_glue;
use super::{pid_from_self, token_from_self};

/// `ReplyTo.send(self, reply: R)`. Serialize bare `R` and route
/// through `koja_rt_reply` to the originating caller, stamping the
/// call's correlation token from `self`. The runtime parks the
/// envelope in the caller's one-shot reply slot, where
/// `koja_rt_call_receive` matches it by token in
/// [`super::ref_methods::emit_call`]. A `select` on the status picks
/// `Delivered` or `Expired`, both built straight-line.
pub(super) fn emit_reply_send<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let pid = pid_from_self(ctx, llvm_function, function)?;
    let token = token_from_self(ctx, llvm_function, function)?;
    let reply_value = nth_param(function, llvm_function, 1, "reply");
    let reply_ir_type = nth_param_type(function, 1);
    let reply_llvm = ir_basic_type(ctx, reply_ir_type)?;
    let (reply_ptr, reply_len) = serialize_to_stack(ctx, "reply_msg", reply_llvm, reply_value)?;
    let drop_glue = payload_drop_glue(ctx, reply_ir_type)?;

    let delivery_symbol = match &function.return_type {
        IRType::Enum(symbol) => symbol.clone(),
        other => panic!(
            "LLVM emit: `ReplyTo.send` returns `{other:?}`, expected the \
             `ReplyTo.Delivery` enum (IR seal invariant violation)",
        ),
    };

    let reply_fn = declare_rt_reply_extern(ctx);
    let status = ctx
        .call_basic(
            reply_fn,
            &[
                pid.into(),
                token.into(),
                reply_ptr.into(),
                reply_len.into(),
                drop_glue.into(),
            ],
            "reply_status",
        )?
        .into_int_value();

    let delivered_status = ctx.context.i64_type().const_int(0, false);
    let delivered = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, status, delivered_status, "reply_ok")
        .or_ice()?;
    let delivered_value = build_delivery(ctx, &delivery_symbol, "Delivered")?;
    let expired_value = build_delivery(ctx, &delivery_symbol, "Expired")?;
    let delivery = ctx
        .builder
        .build_select(delivered, delivered_value, expired_value, "reply_delivery")
        .or_ice()?;
    ctx.builder
        .build_return(Some(&delivery))
        .or_ice()
        .map(|_| ())
}

/// Build a nullary `ReplyTo.Delivery` variant (`Delivered` / `Expired`),
/// resolving its tag by name so the enum's declaration order is not
/// baked into codegen.
fn build_delivery<'ctx>(
    ctx: &EmitContext<'ctx>,
    delivery_symbol: &IRSymbol,
    variant: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let tag = ctx.layouts.enum_variant_tag(delivery_symbol, variant);
    build_enum_value(ctx, delivery_symbol, tag, &[])
}
