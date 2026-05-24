//! `Ref<M, R>` and `ReplyTo<R>` `@intrinsic` methods. The eval
//! interpreter has no scheduler, mailbox, or signal plumbing — those
//! live in the LLVM backend's `koja_rt_*` runtime ABI — so every
//! method here returns [`RuntimeError::Unsupported`]. The dispatch
//! still goes through an exhaustive match on
//! [`koja_ir::RefMethod`] / [`koja_ir::ReplyToMethod`]
//! so adding a new mailbox primitive forces a touch here even
//! while the bodies stay stubs.

use koja_ir::{IRFunction, RefMethod, ReplyToMethod};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn ref_dispatch(
    method: RefMethod,
    function: &IRFunction,
) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: format!(
            "`Ref.{}` (called via `{}`) is not supported under the interpreter; \
             process scheduling lives in the LLVM runtime",
            method_name(method),
            function.symbol,
        ),
    })
}

pub(super) fn reply_to_dispatch(
    method: ReplyToMethod,
    function: &IRFunction,
) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: format!(
            "`ReplyTo.{}` (called via `{}`) is not supported under the interpreter; \
             reply delivery lives in the LLVM runtime",
            reply_method_name(method),
            function.symbol,
        ),
    })
}

fn method_name(method: RefMethod) -> &'static str {
    match method {
        RefMethod::AliveQ => "alive?",
        RefMethod::Call => "call",
        RefMethod::Cast => "cast",
        RefMethod::Kill => "kill",
        RefMethod::SelfRef => "self_ref",
        RefMethod::SendAfter => "send_after",
        RefMethod::Signal => "signal",
    }
}

fn reply_method_name(method: ReplyToMethod) -> &'static str {
    match method {
        ReplyToMethod::Send => "send",
    }
}
