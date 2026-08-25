//! Cross-intrinsic helpers: shared shapes that several `intrinsics/`
//! handlers reach for. Lifted out to keep `option_value` /
//! `result_value` / `size_of_primitive` from drifting across
//! sibling modules.

use koja_ir::{IREnumVariant, IRFunction, IRSymbol, IRType, IRVariantPayload};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::value::{EnumPayload, Value};

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
/// `Result<T, ErrEnum>` intrinsic. The error enum's symbol comes from the
/// `Result` decl's `Err` payload and the variant tag is resolved by name,
/// so neither the stdlib's mangling scheme nor any enum's declaration
/// order is baked in here. Shared by the checked-narrowing / parse
/// intrinsics (`NumericConversionError`) and `Ref.call` (`CallError`).
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

/// Build a unit-variant value `<enum>.<variant>` directly, resolving the tag
/// by name so the enum's declaration order isn't baked in. Used where an
/// intrinsic returns a bare enum (e.g. `ReplyTo.send -> ReplyTo.Delivery`).
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
