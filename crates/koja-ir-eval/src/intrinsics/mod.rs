//! Per-backend dispatch table for `@intrinsic` function bodies on
//! the eval interpreter side. Mirrors the LLVM backend's
//! `intrinsics/` shape: each registered intrinsic is keyed by its
//! [`koja_ir::FunctionKind::Intrinsic`] payload (an
//! [`IRIntrinsicId`], a typed enum the lift pass mints from the
//! function's identifier path) and routed via an exhaustive `match`
//! to a hand-written handler.
//!
//! Adding a new intrinsic: extend [`IRIntrinsicId`] in
//! `koja-ir`, drop a sibling `<name>.rs` module exporting
//! `pub(super) fn <handler>`, and wire its arm in [`dispatch`]. The
//! exhaustive match makes the wiring step compiler-checked.

use koja_ir::{IRFunction, IRIntrinsicId, KernelMethod};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::value::Value;

mod binary;
mod bitwise;
mod consuming;
mod cptr;
mod cstring;
mod debug;
mod equality;
mod hash;
mod helpers;
mod kernel;
mod list;
mod map;
mod numeric;
mod parse;
mod process;
mod runtime_block;
mod set;
mod socket;
mod string;

pub(crate) use process::build_business_payload;

/// The bundle every resolver-aware intrinsic handler receives. `function`
/// is the calling [`IRFunction`] (handlers read receiver symbols off its
/// return type), `args` are the evaluated arguments (receiver first), and
/// `resolver` looks up sibling declarations. Bundled so the next
/// cross-cutting dependency is a new field here, not a parameter threaded
/// through every module.
pub(super) struct IntrinsicCall<'a, R: CallResolver> {
    pub(super) args: &'a [Value],
    pub(super) function: &'a IRFunction,
    pub(super) resolver: &'a R,
}

/// Run the registered intrinsic `id` for the calling `function`.
/// Handlers that mint typed return values (`Option<T>`,
/// `Result<T, E>`, tuples) read the receiver symbol from
/// `function.return_type`, and pointer-typed intrinsics
/// (`CPtr.alloc`, `CPtr.offset`, …) read the element type from
/// `function.params[0].ty` / `function.return_type` to compute
/// `size_of::<T>()`. `resolver` is consulted when a handler needs
/// sibling declaration information, so no path fabricates an
/// `IRSymbol` from a string.
///
/// `async` because the process intrinsics suspend: `Ref.call` parks on
/// the caller's reply slot and yields to the driver until the reply lands
/// (or the timeout fires). Every other intrinsic resolves synchronously
/// and just returns its value through the state machine.
pub(crate) async fn dispatch<R: CallResolver>(
    id: &IRIntrinsicId,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let call = IntrinsicCall {
        args,
        function,
        resolver,
    };
    match *id {
        IRIntrinsicId::Binary(method) => binary::binary(method, call),
        IRIntrinsicId::Bits(method) => binary::bits(method, call),
        IRIntrinsicId::Bitwise { ty, op } => bitwise::dispatch(ty, op, args),
        IRIntrinsicId::CPtr(method) => cptr::dispatch(method, function, args),
        IRIntrinsicId::CString(_) => cstring::to_string(call),
        IRIntrinsicId::Consuming(method) => consuming::dispatch(method, args),
        IRIntrinsicId::Debug(impl_) => debug::dispatch(impl_, args),
        IRIntrinsicId::Equality(impl_) => equality::dispatch(impl_, args),
        IRIntrinsicId::Hash(impl_) => hash::dispatch(impl_, args),
        IRIntrinsicId::Kernel(KernelMethod::Panic) => kernel::panic(args),
        IRIntrinsicId::List(method) => list::dispatch(method, call),
        IRIntrinsicId::Map(method) => map::dispatch(method, call),
        IRIntrinsicId::NumericConvert(convert) => numeric::dispatch(convert, call),
        IRIntrinsicId::Parse(target) => parse::dispatch(target, call),
        IRIntrinsicId::Process(method) => process::process_dispatch(method, call),
        IRIntrinsicId::Ref(method) => process::ref_dispatch(method, call).await,
        IRIntrinsicId::ReplyTo(method) => process::reply_to_dispatch(method, call).await,
        IRIntrinsicId::RuntimeBlock(method) => runtime_block::dispatch(method, args),
        IRIntrinsicId::Set(method) => set::dispatch(method, call),
        IRIntrinsicId::Socket(method) => socket::dispatch(method, call).await,
        IRIntrinsicId::String(method) => string::dispatch(method, call),
    }
}
