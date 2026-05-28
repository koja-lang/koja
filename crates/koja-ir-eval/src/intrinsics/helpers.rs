//! Cross-intrinsic helpers — shared shapes that several `intrinsics/`
//! handlers reach for. Lifted out to keep `option_value` /
//! `result_value` / `size_of_primitive` from drifting across
//! sibling modules.

use std::cell::RefCell;
use std::rc::Rc;

use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag};

use crate::error::RuntimeError;
use crate::value::{EnumPayload, Value};

/// `Option<T>::Some` carries the only tuple-payload variant; v1
/// monotonically assigns tag `0`. The `option_value` helper bakes
/// that in so callers don't have to fish through the enum decl for
/// each invocation.
const SOME_TAG: IRVariantTag = IRVariantTag(0);
const NONE_TAG: IRVariantTag = IRVariantTag(1);

/// `Result<T, E>::Ok` carries the value; `::Err` carries the error.
/// Same v1 convention as `Option`'s `Some` / `None`.
const OK_TAG: IRVariantTag = IRVariantTag(0);
const ERR_TAG: IRVariantTag = IRVariantTag(1);

/// Construct an `Option<T>` value over `symbol`. `Some(value)` lands
/// as a tuple-payload variant; `None` is a unit variant.
pub(super) fn option_value(symbol: IRSymbol, value: Option<Value>) -> Value {
    match value {
        Some(v) => Value::Enum {
            name: "Some".into(),
            payload: EnumPayload::Tuple(vec![v]),
            symbol,
            tag: SOME_TAG,
        },
        None => Value::Enum {
            name: "None".into(),
            payload: EnumPayload::Unit,
            symbol,
            tag: NONE_TAG,
        },
    }
}

/// Construct a `Result<T, E>` value over `symbol`. Both arms carry
/// a single-element tuple payload.
pub(super) fn result_value(symbol: IRSymbol, value: Result<Value, Value>) -> Value {
    match value {
        Ok(v) => Value::Enum {
            name: "Ok".into(),
            payload: EnumPayload::Tuple(vec![v]),
            symbol,
            tag: OK_TAG,
        },
        Err(v) => Value::Enum {
            name: "Err".into(),
            payload: EnumPayload::Tuple(vec![v]),
            symbol,
            tag: ERR_TAG,
        },
    }
}

/// Read the receiver enum's [`IRSymbol`] off `function.return_type`,
/// erroring when the return shape isn't an enum (a typecheck /
/// lower invariant violation that we surface rather than panic
/// because the intrinsic dispatch seam can't rely on seal).
pub(super) fn enum_return_symbol(
    function: &IRFunction,
    label: &str,
) -> Result<IRSymbol, RuntimeError> {
    match &function.return_type {
        IRType::Enum(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} expected Enum return type, got `{other:?}`"),
        }),
    }
}

/// Recursive deep clone of a [`Value`]. Plain `Value::clone()` is a
/// shallow `Rc::clone` for `List` / `Map` / `Set` storage, which
/// would leave the clone aliased with the original — observable as
/// soon as either side mutates. This helper walks the tree and mints
/// fresh `Rc<RefCell<...>>` for every container layer so the result
/// is fully independent.
///
/// Used by `Map.clone` / `Set.clone` (and reused by any future
/// container-clone intrinsic) — non-container variants fall back to
/// `Value::clone`, which already deep-copies their owned payload
/// (e.g. `Vec<u8>` for `Value::String` / `Value::Binary`).
pub(super) fn deep_clone_value(value: &Value) -> Value {
    match value {
        Value::List(entries) => {
            let cloned: Vec<Value> = entries.borrow().iter().map(deep_clone_value).collect();
            Value::List(Rc::new(RefCell::new(cloned)))
        }
        Value::Map(entries) => {
            let cloned: Vec<(Value, Value)> = entries
                .borrow()
                .iter()
                .map(|(k, v)| (deep_clone_value(k), deep_clone_value(v)))
                .collect();
            Value::Map(Rc::new(RefCell::new(cloned)))
        }
        Value::Set(entries) => {
            let cloned: Vec<Value> = entries.borrow().iter().map(deep_clone_value).collect();
            Value::Set(Rc::new(RefCell::new(cloned)))
        }
        Value::Enum {
            name,
            payload,
            symbol,
            tag,
        } => Value::Enum {
            name: name.clone(),
            payload: deep_clone_payload(payload),
            symbol: symbol.clone(),
            tag: *tag,
        },
        Value::Struct { symbol, fields } => Value::Struct {
            symbol: symbol.clone(),
            fields: fields.iter().map(deep_clone_value).collect(),
        },
        Value::Union {
            payload,
            symbol,
            tag,
        } => Value::Union {
            payload: Box::new(deep_clone_value(payload)),
            symbol: symbol.clone(),
            tag: *tag,
        },
        other => other.clone(),
    }
}

fn deep_clone_payload(payload: &EnumPayload) -> EnumPayload {
    match payload {
        EnumPayload::Struct(fields) => EnumPayload::Struct(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), deep_clone_value(value)))
                .collect(),
        ),
        EnumPayload::Tuple(values) => {
            EnumPayload::Tuple(values.iter().map(deep_clone_value).collect())
        }
        EnumPayload::Unit => EnumPayload::Unit,
    }
}

/// Byte size of a primitive [`IRType`]. Used by `CPtr.alloc`,
/// `CPtr.offset`, `CPtr.read`, `CPtr.write` to compute element-
/// width offsets. Returns [`RuntimeError::Unsupported`] for non-
/// primitive element types — eval can't allocate / step over a
/// struct or list without a full size-and-align computation, and
/// the LLVM backend covers those cases on `--backend=llvm`.
pub(super) fn size_of_primitive(ty: &IRType, label: &str) -> Result<usize, RuntimeError> {
    match ty {
        IRType::Bool | IRType::Int8 | IRType::UInt8 => Ok(1),
        IRType::CPtr(_) => Ok(std::mem::size_of::<*mut u8>()),
        IRType::Float32 | IRType::Int32 | IRType::UInt32 => Ok(4),
        IRType::Float64 | IRType::Int64 | IRType::UInt64 => Ok(8),
        IRType::Int16 | IRType::UInt16 => Ok(2),
        other => Err(RuntimeError::Unsupported {
            detail: format!(
                "{label}: eval can only allocate / offset / read / write \
                 primitive `CPtr<T>` element types; got `T = {other:?}`. \
                 Use `--backend=llvm` for non-primitive pointee types.",
            ),
        }),
    }
}
