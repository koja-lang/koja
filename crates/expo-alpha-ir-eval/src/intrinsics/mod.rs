//! Per-backend dispatch table for `@intrinsic` function bodies on
//! the eval interpreter side. Mirrors the LLVM backend's
//! `intrinsics/` shape — each registered intrinsic is keyed by its
//! [`expo_alpha_ir::FunctionKind::Intrinsic`] payload (an
//! [`IRIntrinsicId`] -- a typed enum the lift pass mints from the
//! function's identifier path) and routed via an exhaustive `match`
//! to a hand-written handler.
//!
//! Adding a new intrinsic: extend [`IRIntrinsicId`] in
//! `expo-alpha-ir`, drop a sibling `<name>.rs` module exporting
//! `pub(super) fn <handler>`, and wire its arm in [`dispatch`]. The
//! exhaustive match makes the wiring step compiler-checked.

use expo_alpha_ir::{BitsMethod, IRFunction, IRIntrinsicId, KernelMethod};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::value::Value;

mod binary;
mod bitwise;
mod cptr;
mod cstring;
mod equality;
mod hash;
mod helpers;
mod kernel;
mod list;
mod map;
mod parse;
mod print;
mod set;
mod string;

/// Run the registered intrinsic `id` against `args`. `function` is
/// the calling [`IRFunction`] — handlers that mint typed return
/// values (`Option<T>`, `Result<T, E>`, `Pair<...>`) read the
/// receiver symbol from `function.return_type`; pointer-typed
/// intrinsics (`CPtr.alloc`, `CPtr.offset`, …) read the element
/// type from `function.params[0].ty` / `function.return_type` to
/// compute `size_of::<T>()`. `resolver` is consulted when a
/// handler needs sibling decl info (e.g. Pair's `first` field type
/// for `List.pop`) so neither path fabricates an `IRSymbol` from a
/// string.
pub(crate) fn dispatch<R: CallResolver>(
    id: &IRIntrinsicId,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match *id {
        IRIntrinsicId::Binary(method) => binary::binary(method, function, args),
        IRIntrinsicId::Bits(BitsMethod::ToBinary) => {
            binary::bits(BitsMethod::ToBinary, function, args)
        }
        IRIntrinsicId::Bitwise { ty, op } => bitwise::dispatch(ty, op, args),
        IRIntrinsicId::CPtr(method) => cptr::dispatch(method, function, args),
        IRIntrinsicId::CString(_) => cstring::to_string(args),
        IRIntrinsicId::Equality(impl_) => equality::dispatch(impl_, args),
        IRIntrinsicId::Hash(impl_) => hash::dispatch(impl_, args),
        IRIntrinsicId::Kernel(KernelMethod::Panic) => kernel::panic(args),
        IRIntrinsicId::List(method) => list::dispatch(method, function, args, resolver),
        IRIntrinsicId::Map(method) => map::dispatch(method, function, args),
        IRIntrinsicId::Parse(target) => parse::dispatch(target, function, args),
        IRIntrinsicId::Print => print::global_print(args),
        IRIntrinsicId::Set(method) => set::dispatch(method, function, args),
        IRIntrinsicId::String(method) => string::dispatch(method, function, args),
    }
}
