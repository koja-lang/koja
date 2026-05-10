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
mod kernel;
mod list;
mod parse;
mod print;
mod string;

/// Run the registered intrinsic `id` against `args`. `function` is
/// the calling [`IRFunction`] — most handlers ignore it; list
/// intrinsics consult `function.return_type` to mint correctly-typed
/// `Option<T>` / `Pair<...>` values, and they reach into `resolver`
/// to look up Pair's struct decl (so the Option symbol comes from
/// the IR shape, not a string-mangled fabrication).
pub(crate) fn dispatch<R: CallResolver>(
    id: &IRIntrinsicId,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match *id {
        IRIntrinsicId::Binary(method) => binary::binary(method, args),
        IRIntrinsicId::Bits(BitsMethod::ToBinary) => binary::bits(BitsMethod::ToBinary, args),
        IRIntrinsicId::Bitwise { ty, op } => bitwise::dispatch(ty, op, args),
        IRIntrinsicId::CPtr(method) => cptr::dispatch(method, args),
        IRIntrinsicId::CString(_) => cstring::to_string(args),
        IRIntrinsicId::Equality(impl_) => equality::dispatch(impl_, args),
        IRIntrinsicId::Hash(impl_) => hash::dispatch(impl_, args),
        IRIntrinsicId::Kernel(KernelMethod::Panic) => kernel::panic(args),
        IRIntrinsicId::List(method) => list::dispatch(method, function, args, resolver),
        IRIntrinsicId::Parse(target) => parse::dispatch(target, args),
        IRIntrinsicId::Print => print::global_print(args),
        IRIntrinsicId::String(method) => string::dispatch(method, function, args),
    }
}
