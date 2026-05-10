//! Eval handlers for the 48-cell `Bitwise` intrinsic family.
//!
//! Eval collapses every integer width (`Int{8,16,32,64}` /
//! `UInt{8,16,32,64}`) to [`Value::Int(i64)`](crate::Value::Int),
//! so the AND/OR/XOR/NOT/SHL ops are width-agnostic at this layer
//! and operate directly on i64. Right shift is the only op where
//! the receiver type matters: signed types use arithmetic shift
//! (sign-extend, native `i64 >> n`), unsigned types use logical
//! shift (cast through `u64 >> n`). The receiver type is encoded
//! in the dispatch id's prefix (`Int.bsr` vs `UInt8.bsr`) so we
//! parse it from there rather than threading the [`IRFunction`]
//! through the dispatch surface.

use crate::error::RuntimeError;
use crate::value::Value;

/// One of the six bitwise ops parsed from the id's trailing
/// segment. Matches the LLVM emitter's `Op` shape so the two
/// dispatch tables stay in lockstep.
#[derive(Clone, Copy)]
pub(super) enum Op {
    Band,
    Bnot,
    Bor,
    Bsl,
    Bsr,
    Bxor,
}

/// Split a bitwise intrinsic id (`"Int.band"`, `"UInt8.bsr"`) into
/// `(receiver_type, op)`. Returns `None` for non-bitwise ids so
/// the parent dispatch can fall through; the receiver-type slice
/// stays a `&str` to avoid an allocation per call.
pub(super) fn parse_id(id: &str) -> Option<(&str, Op)> {
    let (ty, suffix) = id.rsplit_once('.')?;
    let op = match suffix {
        "band" => Op::Band,
        "bnot" => Op::Bnot,
        "bor" => Op::Bor,
        "bsl" => Op::Bsl,
        "bsr" => Op::Bsr,
        "bxor" => Op::Bxor,
        _ => return None,
    };
    Some((ty, op))
}

fn is_signed_type(ty: &str) -> bool {
    matches!(ty, "Int" | "Int8" | "Int16" | "Int32" | "Int64")
}

/// Run a parsed bitwise intrinsic. `id` is the dispatch key
/// (`"Int.band"` etc.) — used only for error messages and to
/// route right-shift signedness; the parsed `(ty, op)` is what
/// drives execution.
pub(super) fn dispatch(id: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    let Some((ty, op)) = parse_id(id) else {
        panic!(
            "bitwise dispatch invoked with non-bitwise id `{id}`; \
             dispatch table and parse_id must stay in sync",
        );
    };
    let lhs = arg_int(args, 0, id)?;
    let result = match op {
        Op::Band => lhs & arg_int(args, 1, id)?,
        Op::Bor => lhs | arg_int(args, 1, id)?,
        Op::Bxor => lhs ^ arg_int(args, 1, id)?,
        Op::Bnot => !lhs,
        Op::Bsl => {
            let n = arg_int(args, 1, id)?;
            // Cast to u32 for the Rust shift; out-of-range counts
            // are undefined in LLVM and saturate-to-zero in this
            // interpreter (panics worse than mismatched semantics).
            lhs.wrapping_shl(n as u32)
        }
        Op::Bsr => {
            let n = arg_int(args, 1, id)?;
            if is_signed_type(ty) {
                lhs.wrapping_shr(n as u32)
            } else {
                ((lhs as u64).wrapping_shr(n as u32)) as i64
            }
        }
    };
    Ok(Value::Int(result))
}

fn arg_int(args: &[Value], index: usize, id: &str) -> Result<i64, RuntimeError> {
    match args.get(index) {
        Some(Value::Int(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeMismatch {
            detail: format!("`{id}` arg #{index}: expected Int, got {other:?}"),
        }),
        None => Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{id}` arity: expected at least {expected} args, got {got}",
                expected = index + 1,
                got = args.len(),
            ),
        }),
    }
}
