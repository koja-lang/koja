//! Cross-intrinsic helpers: shared shapes that several `intrinsics/`
//! handlers reach for. Lifted out to keep the `arg_*` readers,
//! `option_value` / `result_value`, and `size_of_primitive` from
//! drifting across sibling modules.

use std::cell::RefCell;
use std::rc::Rc;
use std::str;

use koja_ir::{IREnumVariant, IRFunction, IRSymbol, IRType, IRVariantPayload};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::value::{EnumPayload, MapEntries, SetEntries, Value};

/// The `index`-th argument. A missing argument is a
/// [`RuntimeError::TypeMismatch`] like every other shape violation
/// at the intrinsic seam.
pub(super) fn arg<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError::TypeMismatch {
        detail: format!("{label} missing arg #{index} (got {} args)", args.len()),
    })
}

/// Read the `index`-th argument as an `Int`.
pub(super) fn arg_int(args: &[Value], index: usize, label: &str) -> Result<i64, RuntimeError> {
    match arg(args, index, label)? {
        Value::Int(value) => Ok(*value),
        other => Err(arg_mismatch(label, index, "Int", other)),
    }
}

/// Share the `index`-th argument's `List` storage.
pub(super) fn arg_list(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Rc<RefCell<Vec<Value>>>, RuntimeError> {
    match arg(args, index, label)? {
        Value::List(items) => Ok(items.clone()),
        other => Err(arg_mismatch(label, index, "List", other)),
    }
}

/// Share the `index`-th argument's `Map` storage.
pub(super) fn arg_map(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<MapEntries, RuntimeError> {
    match arg(args, index, label)? {
        Value::Map(entries) => Ok(entries.clone()),
        other => Err(arg_mismatch(label, index, "Map", other)),
    }
}

/// The shape error for an argument of the wrong kind.
fn arg_mismatch(label: &str, index: usize, expected: &str, other: &Value) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!("{label} arg #{index} expected {expected}, got `{other}`"),
    }
}

/// Read the `index`-th argument as a `Range { start, stop }` pair.
/// Typecheck pins the `Range` shape (two `Int` fields in source
/// order) before an intrinsic runs.
pub(super) fn arg_range(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<(i64, i64), RuntimeError> {
    let value = arg(args, index, label)?;
    if let Value::Struct { fields, .. } = value
        && let [Value::Int(start), Value::Int(stop)] = fields.as_slice()
    {
        return Ok((*start, *stop));
    }
    Err(arg_mismatch(label, index, "Range struct", value))
}

/// Share the `index`-th argument's `Set` storage.
pub(super) fn arg_set(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<SetEntries, RuntimeError> {
    match arg(args, index, label)? {
        Value::Set(items) => Ok(items.clone()),
        other => Err(arg_mismatch(label, index, "Set", other)),
    }
}

/// Borrow the `index`-th argument's `String` bytes.
pub(super) fn arg_string_bytes<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a [u8], RuntimeError> {
    match arg(args, index, label)? {
        Value::String(bytes) => Ok(bytes.as_slice()),
        other => Err(arg_mismatch(label, index, "String", other)),
    }
}

/// Borrow the `index`-th argument as `&str`. Surfaces a clean
/// [`RuntimeError::Unsupported`] when the payload is not valid
/// UTF-8: codepoint-walking methods (`length`, `get`, `slice`)
/// need it. Byte-oriented methods read raw bytes through
/// [`arg_string_bytes`] instead.
pub(super) fn arg_string_utf8<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, RuntimeError> {
    let bytes = arg_string_bytes(args, index, label)?;
    str::from_utf8(bytes).map_err(|err| RuntimeError::Unsupported {
        detail: format!(
            "{label} arg #{index}: String contents are not valid UTF-8 \
             (invalid at byte {}): {err}",
            err.valid_up_to(),
        ),
    })
}

/// Find `variant_name` on `symbol`'s decl. Every helper below resolves
/// variant tags through here, by name, so no stdlib declaration order
/// is baked into eval-constructed values.
fn named_variant<'d, R: CallResolver>(
    symbol: &IRSymbol,
    resolver: &'d R,
    variant_name: &str,
) -> Result<&'d IREnumVariant, RuntimeError> {
    let decl = resolver
        .enum_decl(symbol.mangled())
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("enum decl `{symbol}` not found in program"),
        })?;
    decl.variants
        .iter()
        .find(|variant| variant.name == variant_name)
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("enum `{symbol}` has no variant named `{variant_name}`"),
        })
}

/// Construct an `Option<T>` value over `symbol`. `Some(value)` lands
/// as a tuple-payload variant. `None` is a unit variant.
pub(super) fn option_value<R: CallResolver>(
    symbol: IRSymbol,
    resolver: &R,
    value: Option<Value>,
) -> Result<Value, RuntimeError> {
    let (name, payload) = match value {
        Some(v) => ("Some", EnumPayload::tuple(vec![v])),
        None => ("None", EnumPayload::Unit),
    };
    let tag = named_variant(&symbol, resolver, name)?.tag;
    Ok(Value::Enum {
        name: name.into(),
        payload,
        symbol,
        tag,
    })
}

/// Construct a `Result<T, E>` value over `symbol`. Both arms carry
/// a single-element tuple payload.
pub(super) fn result_value<R: CallResolver>(
    symbol: IRSymbol,
    resolver: &R,
    value: Result<Value, Value>,
) -> Result<Value, RuntimeError> {
    let (name, payload) = match value {
        Ok(v) => ("Ok", EnumPayload::tuple(vec![v])),
        Err(v) => ("Err", EnumPayload::tuple(vec![v])),
    };
    let tag = named_variant(&symbol, resolver, name)?.tag;
    Ok(Value::Enum {
        name: name.into(),
        payload,
        symbol,
        tag,
    })
}

/// Build the `<ErrEnum>.<variant>` value for a unit-variant error of a
/// `Result<T, ErrEnum>` intrinsic. The error enum's symbol comes from
/// the `Result` decl's `Err` payload.
pub(super) fn err_variant_value<R: CallResolver>(
    result_symbol: &IRSymbol,
    resolver: &R,
    variant_name: &str,
) -> Result<Value, RuntimeError> {
    let err_variant = named_variant(result_symbol, resolver, "Err")?;
    let IRVariantPayload::Tuple(types) = &err_variant.payload else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("`{result_symbol}`'s Err variant payload is not a tuple"),
        });
    };
    let [IRType::Enum(error_symbol)] = types.as_slice() else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{result_symbol}`'s Err payload should be a single error enum, got `{types:?}`",
            ),
        });
    };
    unit_variant_value(error_symbol, resolver, variant_name)
}

/// Build a unit-variant value `<enum>.<variant>` directly. Used where
/// an intrinsic returns a bare enum (e.g. `ReplyTo.send ->
/// ReplyTo.Delivery`).
pub(super) fn unit_variant_value<R: CallResolver>(
    enum_symbol: &IRSymbol,
    resolver: &R,
    variant_name: &str,
) -> Result<Value, RuntimeError> {
    let tag = named_variant(enum_symbol, resolver, variant_name)?.tag;
    Ok(Value::Enum {
        name: variant_name.into(),
        payload: EnumPayload::Unit,
        symbol: enum_symbol.clone(),
        tag,
    })
}

/// The single `Ok` payload type of a `Result` enum decl. The IR
/// seal pins `Result.Ok` to exactly one tuple field. Shape
/// violations surface as errors (not panics) because the intrinsic
/// dispatch seam can't rely on seal.
pub(super) fn single_ok_payload<R: CallResolver>(
    result_symbol: &IRSymbol,
    resolver: &R,
    label: &str,
) -> Result<IRType, RuntimeError> {
    let ok_variant = named_variant(result_symbol, resolver, "Ok")?;
    match &ok_variant.payload {
        IRVariantPayload::Tuple(types) if types.len() == 1 => Ok(types[0].clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!(
                "{label}: `{result_symbol}` Ok variant has unexpected payload `{other:?}` \
                 (expected a single tuple field)",
            ),
        }),
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

/// Byte size of a primitive [`IRType`]. Used by `CPtr.alloc`,
/// `CPtr.offset`, `CPtr.read`, `CPtr.write` to compute element-
/// width offsets. Returns [`RuntimeError::Unsupported`] for non-
/// primitive element types. Eval can't allocate / step over a
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
                 primitive `CPtr<T>` element types, got `T = {other:?}`. \
                 Use `--backend=llvm` for non-primitive pointee types.",
            ),
        }),
    }
}
