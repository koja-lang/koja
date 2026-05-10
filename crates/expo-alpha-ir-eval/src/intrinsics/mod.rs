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

use expo_alpha_ir::{BitsMethod, IRIntrinsicId, KernelMethod};

use crate::error::RuntimeError;
use crate::value::Value;

mod binary;
mod bitwise;
mod cptr;
mod cstring;
mod equality;
mod hash;
mod kernel;
mod parse;
mod print;

/// Run the registered intrinsic `id` against `args`.
pub(crate) fn dispatch(id: &IRIntrinsicId, args: &[Value]) -> Result<Value, RuntimeError> {
    match *id {
        IRIntrinsicId::Binary(method) => binary::binary(method, args),
        IRIntrinsicId::Bits(BitsMethod::ToBinary) => binary::bits(BitsMethod::ToBinary, args),
        IRIntrinsicId::Bitwise { ty, op } => bitwise::dispatch(ty, op, args),
        IRIntrinsicId::CPtr(method) => cptr::dispatch(method, args),
        IRIntrinsicId::CString(_) => cstring::to_string(args),
        IRIntrinsicId::Equality(impl_) => equality::dispatch(impl_, args),
        IRIntrinsicId::Hash(impl_) => hash::dispatch(impl_, args),
        IRIntrinsicId::Kernel(KernelMethod::Panic) => kernel::panic(args),
        IRIntrinsicId::Parse(target) => parse::dispatch(target, args),
        IRIntrinsicId::Print => print::global_print(args),
    }
}
