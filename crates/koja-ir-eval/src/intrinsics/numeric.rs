//! Explicit numeric conversions out of the hub types, the eval
//! mirror of the LLVM backend's `intrinsics/numeric.rs`.
//!
//! All sized integers live as canonical `Value::Int(i64)` here, so
//! a successful narrowing is just a bounds check (no representation
//! change). `Float.to_float32` converts the variant and requires
//! the rounded result to stay finite (the finite-only `Float`
//! invariant). The checked conversions mint
//! `NumericConversionError.OutOfRange` on failure, recovering the
//! error enum's symbol from the `Result` return type's `Err`
//! variant payload via the resolver.

use koja_ir::{IntNarrowTarget, NumericConvert};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn dispatch<R: CallResolver>(
    convert: NumericConvert,
    function: &koja_ir::IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    if matches!(convert, NumericConvert::FloatToFloat32) {
        let [Value::Float64(v)] = args else {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("Float.to_float32 expects a single Float receiver, got {args:?}"),
            });
        };
        let result_symbol = helpers::enum_return_symbol(function, "Float.to_float32")?;
        let narrowed = *v as f32;
        let converted = if narrowed.is_finite() {
            Ok(Value::Float32(narrowed))
        } else {
            Err(helpers::err_variant_value(
                &result_symbol,
                resolver,
                "OutOfRange",
            )?)
        };
        return Ok(helpers::result_value(result_symbol, converted));
    }

    let [Value::Int(v)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("{convert:?} expects a single integer receiver, got {args:?}"),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, &format!("{convert:?}"))?;
    let (min, max) = checked_bounds(convert);
    let converted = if (min..=max).contains(v) {
        Ok(Value::Int(*v))
    } else {
        Err(helpers::err_variant_value(
            &result_symbol,
            resolver,
            "OutOfRange",
        )?)
    };
    Ok(helpers::result_value(result_symbol, converted))
}

/// Inclusive bounds the receiver must satisfy. Mirrors the LLVM
/// emitter's `checked_bounds`.
fn checked_bounds(convert: NumericConvert) -> (i64, i64) {
    match convert {
        NumericConvert::FloatToFloat32 => unreachable!("float path handled separately"),
        NumericConvert::IntNarrow(target) => match target {
            IntNarrowTarget::Int8 => (i64::from(i8::MIN), i64::from(i8::MAX)),
            IntNarrowTarget::Int16 => (i64::from(i16::MIN), i64::from(i16::MAX)),
            IntNarrowTarget::Int32 => (i64::from(i32::MIN), i64::from(i32::MAX)),
            IntNarrowTarget::UInt8 => (0, i64::from(u8::MAX)),
            IntNarrowTarget::UInt16 => (0, i64::from(u16::MAX)),
            IntNarrowTarget::UInt32 => (0, i64::from(u32::MAX)),
            // Every non-negative `Int` fits `UInt64`.
            IntNarrowTarget::UInt64 => (0, i64::MAX),
        },
        // A `UInt64` bit pattern fits `Int` iff it is at most
        // `i64::MAX`, i.e. non-negative under the signed view.
        NumericConvert::UInt64ToInt => (0, i64::MAX),
    }
}
