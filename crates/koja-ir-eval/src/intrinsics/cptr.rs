//! `CPtr<T>` family: `address`, `alloc`, `borrow`, `copy`, `free`,
//! `null`, `null?`, `offset`, `read`, `to_binary`, `write`.
//!
//! Eval now backs `CPtr<T>` with a real raw pointer ([`Value::CPtr`])
//! so the shell can exercise the same FFI paths the LLVM backend
//! emits. Each handler that needs element-width info reads `T` from
//! the calling [`IRFunction`]'s signature (return type for `alloc`
//! / `read`, first-param type for `offset` / `write`) and computes
//! `size_of::<T>()` via [`helpers::size_of_primitive`]. Non-primitive
//! pointee types surface
//! [`crate::error::RuntimeError::Unsupported`] with a pointer to
//! `--backend=llvm`.
//!
//! `to_binary` byte-copies `len` bytes into a fresh `Value::Binary`.

use std::ptr;
use std::slice;

use koja_ir::panics::CPTR_READ_NON_FINITE_MESSAGE;
use koja_ir::{CPtrMethod, IRFunction, IRType};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

pub(super) fn dispatch(
    method: CPtrMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        CPtrMethod::Address => address(args),
        CPtrMethod::Alloc => alloc(function, args),
        CPtrMethod::Borrow => borrow(args),
        CPtrMethod::Copy => copy(args),
        CPtrMethod::Free => free_(args),
        CPtrMethod::Null => null(),
        CPtrMethod::NullQ => null_q(args),
        CPtrMethod::Offset => offset(function, args),
        CPtrMethod::Read => read(function, args),
        CPtrMethod::ToBinary => to_binary(args),
        CPtrMethod::Write => write(function, args),
    }
}

fn address(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.address expects a single CPtr argument, got {args:?}"),
        });
    };
    Ok(Value::Int(*ptr as i64))
}

fn alloc(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(count)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.alloc expects a single Int argument, got {args:?}"),
        });
    };
    if *count < 0 {
        return Err(RuntimeError::Panicked {
            message: "CPtr.alloc count cannot be negative".to_string(),
        });
    }
    let element_size = pointee_size(&function.return_type, "CPtr.alloc")?;
    let count = *count as usize;
    let total = count
        .checked_mul(element_size)
        .ok_or_else(|| RuntimeError::Unsupported {
            detail: format!(
                "CPtr.alloc: count {count} * element_size {element_size} overflows usize"
            ),
        })?;
    let ptr = if total == 0 {
        ptr::null_mut()
    } else {
        unsafe { malloc(total) }
    };
    Ok(Value::CPtr(ptr))
}

fn borrow(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.borrow expects a single Binary argument, got {args:?}"),
        });
    };
    // The caller's frame local keeps the `Rc` alive through the
    // statement, matching the position check's in-statement contract.
    Ok(Value::CPtr(bytes.as_ptr() as *mut u8))
}

fn copy(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.copy expects a single Binary argument, got {args:?}"),
        });
    };
    if bytes.is_empty() {
        return Ok(Value::CPtr(ptr::null_mut()));
    }
    let ptr = unsafe { malloc(bytes.len()) };
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    Ok(Value::CPtr(ptr))
}

fn free_(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.free expects a single CPtr argument, got {args:?}"),
        });
    };
    if !ptr.is_null() {
        unsafe { free(*ptr) };
    }
    Ok(Value::Unit)
}

fn null() -> Result<Value, RuntimeError> {
    Ok(Value::CPtr(ptr::null_mut()))
}

fn null_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.null? expects a single CPtr argument, got {args:?}"),
        });
    };
    Ok(Value::Bool(ptr.is_null()))
}

fn offset(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr), Value::Int(n)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.offset expects (CPtr<T>, Int), got {args:?}"),
        });
    };
    let element_size = receiver_pointee_size(function, "CPtr.offset")?;
    let stepped = unsafe { ptr.offset((*n) as isize * element_size as isize) };
    Ok(Value::CPtr(stepped))
}

fn read(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.read expects a single CPtr argument, got {args:?}"),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.read(null) is undefined behavior, refusing to dereference".to_string(),
        });
    }
    read_primitive(*ptr, &function.return_type, "CPtr.read")
}

fn write(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr), value] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.write expects (CPtr<T>, T), got {args:?}"),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.write(null, _) is undefined behavior, refusing to dereference"
                .to_string(),
        });
    }
    let pointee = receiver_pointee_ty(function, "CPtr.write")?;
    write_primitive(*ptr, pointee, value, "CPtr.write")?;
    Ok(Value::Unit)
}

fn to_binary(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr), Value::Int(len)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.to_binary expects (CPtr<UInt8>, Int), got {args:?}"),
        });
    };
    if *len < 0 {
        return Err(RuntimeError::Panicked {
            message: "CPtr.to_binary length cannot be negative".to_string(),
        });
    }
    let len = *len as usize;
    if len == 0 {
        return Ok(Value::binary(Vec::new()));
    }
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.to_binary(null, len > 0) is undefined behavior, refusing to copy"
                .to_string(),
        });
    }
    let bytes = unsafe { slice::from_raw_parts(*ptr as *const u8, len) }.to_vec();
    Ok(Value::binary(bytes))
}

fn pointee_size(return_type: &IRType, label: &str) -> Result<usize, RuntimeError> {
    let pointee = match return_type {
        IRType::CPtr(inner) => inner.as_ref(),
        other => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("{label} expected CPtr<T> return type, got `{other:?}`"),
            });
        }
    };
    helpers::size_of_primitive(pointee, label)
}

fn receiver_pointee_ty<'a>(
    function: &'a IRFunction,
    label: &str,
) -> Result<&'a IRType, RuntimeError> {
    let receiver = function
        .params
        .first()
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("{label} expected a self parameter (IR shape carries none)"),
        })?;
    match &receiver.ty {
        IRType::CPtr(inner) => Ok(inner.as_ref()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} expected CPtr<T> receiver, got `{other:?}`"),
        }),
    }
}

fn receiver_pointee_size(function: &IRFunction, label: &str) -> Result<usize, RuntimeError> {
    let pointee = receiver_pointee_ty(function, label)?;
    helpers::size_of_primitive(pointee, label)
}

/// Read a `T` from `ptr` without an alignment requirement.
///
/// # Safety
/// `ptr` must address `size_of::<T>()` readable bytes.
unsafe fn read_as<T: Copy>(ptr: *mut u8) -> T {
    unsafe { (ptr as *const T).read_unaligned() }
}

/// Write `value` at `ptr` without an alignment requirement.
///
/// # Safety
/// `ptr` must address `size_of::<T>()` writable bytes.
unsafe fn write_as<T>(ptr: *mut u8, value: T) {
    unsafe { (ptr as *mut T).write_unaligned(value) }
}

fn read_primitive(ptr: *mut u8, ty: &IRType, label: &str) -> Result<Value, RuntimeError> {
    // SAFETY: the caller checked `ptr` is non-null and the IR type
    // fixes the pointee width; foreign memory is trusted, as with FFI.
    let value = unsafe {
        match ty {
            IRType::Bool => Value::Bool(read_as::<u8>(ptr) != 0),
            IRType::CPtr(_) => Value::CPtr(read_as::<*mut u8>(ptr)),
            IRType::Float32 => {
                let v = read_as::<f32>(ptr);
                finite_float(v.is_finite(), Value::Float32(v))?
            }
            IRType::Float64 => {
                let v = read_as::<f64>(ptr);
                finite_float(v.is_finite(), Value::Float64(v))?
            }
            IRType::Int8 => Value::Int(read_as::<i8>(ptr) as i64),
            IRType::Int16 => Value::Int(read_as::<i16>(ptr) as i64),
            IRType::Int32 => Value::Int(read_as::<i32>(ptr) as i64),
            IRType::Int64 => Value::Int(read_as::<i64>(ptr)),
            IRType::UInt8 => Value::Int(read_as::<u8>(ptr) as i64),
            IRType::UInt16 => Value::Int(read_as::<u16>(ptr) as i64),
            IRType::UInt32 => Value::Int(read_as::<u32>(ptr) as i64),
            // `u64::MAX` round-trips through `Value::Int(i64)` as `-1`,
            // mirroring `materialize_const`'s `UInt64 -> Int64` cast
            // (eval doesn't carry a distinct unsigned variant).
            IRType::UInt64 => Value::Int(read_as::<u64>(ptr) as i64),
            other => {
                return Err(RuntimeError::Unsupported {
                    detail: format!("{label}: cannot read `T = {other:?}` (primitive types only)"),
                });
            }
        }
    };
    Ok(value)
}

/// Foreign memory is the other boundary (with extern returns) where
/// NaN or inf could enter a finite-only float type.
fn finite_float(is_finite: bool, value: Value) -> Result<Value, RuntimeError> {
    is_finite
        .then_some(value)
        .ok_or_else(|| RuntimeError::Panicked {
            message: CPTR_READ_NON_FINITE_MESSAGE.to_string(),
        })
}

fn write_primitive(
    ptr: *mut u8,
    ty: &IRType,
    value: &Value,
    label: &str,
) -> Result<(), RuntimeError> {
    // SAFETY: as in `read_primitive`.
    unsafe {
        match (ty, value) {
            (IRType::Bool, Value::Bool(b)) => write_as(ptr, u8::from(*b)),
            (IRType::CPtr(_), Value::CPtr(p)) => write_as(ptr, *p),
            (IRType::Float32, Value::Float32(v)) => write_as(ptr, *v),
            (IRType::Float32, Value::Float64(v)) => write_as(ptr, *v as f32),
            (IRType::Float64, Value::Float32(v)) => write_as(ptr, f64::from(*v)),
            (IRType::Float64, Value::Float64(v)) => write_as(ptr, *v),
            (IRType::Int8, Value::Int(v)) => write_as(ptr, *v as i8),
            (IRType::Int16, Value::Int(v)) => write_as(ptr, *v as i16),
            (IRType::Int32, Value::Int(v)) => write_as(ptr, *v as i32),
            (IRType::Int64, Value::Int(v)) => write_as(ptr, *v),
            (IRType::UInt8, Value::Int(v)) => write_as(ptr, *v as u8),
            (IRType::UInt16, Value::Int(v)) => write_as(ptr, *v as u16),
            (IRType::UInt32, Value::Int(v)) => write_as(ptr, *v as u32),
            (IRType::UInt64, Value::Int(v)) => write_as(ptr, *v as u64),
            (other_ty, other_v) => {
                return Err(RuntimeError::Unsupported {
                    detail: format!(
                        "{label}: cannot write `{other_v}` as `T = {other_ty:?}` (primitive \
                         types only)",
                    ),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_f64(bits: u64) -> Result<Value, RuntimeError> {
        let mut bytes = bits.to_ne_bytes();
        read_primitive(bytes.as_mut_ptr(), &IRType::Float64, "test")
    }

    fn read_f32(bits: u32) -> Result<Value, RuntimeError> {
        let mut bytes = bits.to_ne_bytes();
        read_primitive(bytes.as_mut_ptr(), &IRType::Float32, "test")
    }

    #[test]
    fn finite_float_reads_pass_through() {
        assert!(matches!(read_f64(1.5f64.to_bits()), Ok(Value::Float64(v)) if v == 1.5));
        assert!(matches!(read_f32(2.5f32.to_bits()), Ok(Value::Float32(v)) if v == 2.5));
    }

    #[test]
    fn non_finite_float_reads_trap() {
        for bits in [
            f64::NAN.to_bits(),
            f64::INFINITY.to_bits(),
            f64::NEG_INFINITY.to_bits(),
        ] {
            let Err(RuntimeError::Panicked { message }) = read_f64(bits) else {
                panic!("expected a panic for bits {bits:#x}");
            };
            assert_eq!(message, CPTR_READ_NON_FINITE_MESSAGE);
        }
        assert!(matches!(
            read_f32(f32::NAN.to_bits()),
            Err(RuntimeError::Panicked { .. })
        ));
    }
}
