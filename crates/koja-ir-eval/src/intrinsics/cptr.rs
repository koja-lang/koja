//! `CPtr<T>` family — `alloc`, `free`, `null`, `null?`, `offset`,
//! `read`, `to_binary`, `to_string`, `write`.
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
//! `to_string` / `to_binary` are the receiver-typed methods on
//! `CPtr<UInt8>`: `to_string` reads the rc-prefixed Koja string ABI
//! (`[i64 rc][i64 bit_length][payload…]`, so `bit_length` at
//! `ptr - LENGTH_OFFSET` and the block base at `ptr - BLOCK_HEADER_SIZE`)
//! and frees the source block; `to_binary` byte-copies `len` bytes
//! into a fresh `Value::Binary` (the caller retains ownership of
//! the source pointer per the stdlib docstring).

use std::ptr;
use std::slice;

use koja_ir::{CPtrMethod, IRFunction, IRType};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

/// Distance in bytes from a Koja string/binary payload pointer back to
/// its block base (the `i64 rc` word). The rc header is
/// `[i64 rc][i64 bit_length]`, so the payload sits this far past the
/// base. API contract: MUST equal [`koja_runtime::util::BLOCK_HEADER_SIZE`].
const BLOCK_HEADER_SIZE: usize = 16;

/// Distance in bytes from a payload pointer back to its `i64
/// bit_length` word (the rc word sits a further `LENGTH_OFFSET` before
/// that). API contract: MUST equal [`koja_runtime::util::LENGTH_OFFSET`].
const LENGTH_OFFSET: usize = 8;

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
        CPtrMethod::Alloc => alloc(function, args),
        CPtrMethod::Free => free_(args),
        CPtrMethod::Null => null(),
        CPtrMethod::NullQ => null_q(args),
        CPtrMethod::Offset => offset(function, args),
        CPtrMethod::Read => read(function, args),
        CPtrMethod::ToBinary => to_binary(args),
        CPtrMethod::ToString => to_string(args),
        CPtrMethod::Write => write(function, args),
    }
}

fn alloc(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(count)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.alloc expects a single Int argument; got {args:?}"),
        });
    };
    let element_size = pointee_size(&function.return_type, "CPtr.alloc")?;
    let count = (*count).max(0) as usize;
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

fn free_(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.free expects a single CPtr argument; got {args:?}"),
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
            detail: format!("CPtr.null? expects a single CPtr argument; got {args:?}"),
        });
    };
    Ok(Value::Bool(ptr.is_null()))
}

fn offset(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr), Value::Int(n)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.offset expects (CPtr<T>, Int); got {args:?}"),
        });
    };
    let element_size = receiver_pointee_size(function, "CPtr.offset")?;
    let stepped = unsafe { ptr.offset((*n) as isize * element_size as isize) };
    Ok(Value::CPtr(stepped))
}

fn read(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.read expects a single CPtr argument; got {args:?}"),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.read(null) is undefined behavior; refusing to dereference".to_string(),
        });
    }
    read_primitive(*ptr, &function.return_type, "CPtr.read")
}

fn write(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr), value] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.write expects (CPtr<T>, T); got {args:?}"),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.write(null, _) is undefined behavior; refusing to dereference"
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
            detail: format!("CPtr.to_binary expects (CPtr<UInt8>, Int); got {args:?}"),
        });
    };
    let len = (*len).max(0) as usize;
    if len == 0 {
        return Ok(Value::Binary(Vec::new()));
    }
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.to_binary(null, len > 0) is undefined behavior; refusing to copy"
                .to_string(),
        });
    }
    let bytes = unsafe { slice::from_raw_parts(*ptr as *const u8, len) }.to_vec();
    Ok(Value::Binary(bytes))
}

fn to_string(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CPtr.to_string expects a single CPtr<UInt8> argument; got {args:?}"),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CPtr.to_string(null) is undefined behavior; refusing to dereference"
                .to_string(),
        });
    }
    let bit_length = unsafe { *(ptr.sub(LENGTH_OFFSET) as *const i64) };
    if bit_length < 0 {
        return Err(RuntimeError::Unsupported {
            detail: format!(
                "CPtr.to_string: source header carries negative bit_length {bit_length}; \
                 buffer does not look like an Koja string payload"
            ),
        });
    }
    let byte_length = (bit_length as usize) / 8;
    let bytes = unsafe { slice::from_raw_parts(*ptr as *const u8, byte_length) }.to_vec();
    unsafe { free(ptr.sub(BLOCK_HEADER_SIZE)) };
    Ok(Value::String(bytes))
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
            detail: format!("{label} expected a self parameter — IR shape carries none"),
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

fn read_primitive(ptr: *mut u8, ty: &IRType, label: &str) -> Result<Value, RuntimeError> {
    let value = match ty {
        IRType::Bool => Value::Bool(unsafe { *ptr } != 0),
        IRType::CPtr(_) => {
            let p = unsafe { *(ptr as *const *mut u8) };
            Value::CPtr(p)
        }
        IRType::Float32 => Value::Float32(unsafe { (ptr as *const f32).read_unaligned() }),
        IRType::Float64 => Value::Float64(unsafe { (ptr as *const f64).read_unaligned() }),
        IRType::Int8 => Value::Int(unsafe { *(ptr as *const i8) } as i64),
        IRType::Int16 => Value::Int(unsafe { (ptr as *const i16).read_unaligned() } as i64),
        IRType::Int32 => Value::Int(unsafe { (ptr as *const i32).read_unaligned() } as i64),
        IRType::Int64 => Value::Int(unsafe { (ptr as *const i64).read_unaligned() }),
        IRType::UInt8 => Value::Int(unsafe { *ptr } as i64),
        IRType::UInt16 => Value::Int(unsafe { (ptr as *const u16).read_unaligned() } as i64),
        IRType::UInt32 => Value::Int(unsafe { (ptr as *const u32).read_unaligned() } as i64),
        IRType::UInt64 => {
            // `u64::MAX` round-trips through `Value::Int(i64)` as `-1`;
            // this mirrors `materialize_const`'s `UInt64 -> Int64`
            // cast (eval doesn't carry a distinct unsigned variant).
            let v = unsafe { (ptr as *const u64).read_unaligned() };
            Value::Int(v as i64)
        }
        other => {
            return Err(RuntimeError::Unsupported {
                detail: format!("{label}: cannot read `T = {other:?}` — primitive types only",),
            });
        }
    };
    Ok(value)
}

fn write_primitive(
    ptr: *mut u8,
    ty: &IRType,
    value: &Value,
    label: &str,
) -> Result<(), RuntimeError> {
    match (ty, value) {
        (IRType::Bool, Value::Bool(b)) => unsafe { *ptr = u8::from(*b) },
        (IRType::CPtr(_), Value::CPtr(p)) => unsafe { (ptr as *mut *mut u8).write_unaligned(*p) },
        (IRType::Float32, Value::Float32(v)) => unsafe {
            (ptr as *mut f32).write_unaligned(*v);
        },
        (IRType::Float32, Value::Float64(v)) => unsafe {
            (ptr as *mut f32).write_unaligned(*v as f32);
        },
        (IRType::Float64, Value::Float32(v)) => unsafe {
            (ptr as *mut f64).write_unaligned(f64::from(*v));
        },
        (IRType::Float64, Value::Float64(v)) => unsafe {
            (ptr as *mut f64).write_unaligned(*v);
        },
        (IRType::Int8, Value::Int(v)) => unsafe { *(ptr as *mut i8) = *v as i8 },
        (IRType::Int16, Value::Int(v)) => unsafe { (ptr as *mut i16).write_unaligned(*v as i16) },
        (IRType::Int32, Value::Int(v)) => unsafe { (ptr as *mut i32).write_unaligned(*v as i32) },
        (IRType::Int64, Value::Int(v)) => unsafe { (ptr as *mut i64).write_unaligned(*v) },
        (IRType::UInt8, Value::Int(v)) => unsafe { *ptr = *v as u8 },
        (IRType::UInt16, Value::Int(v)) => unsafe { (ptr as *mut u16).write_unaligned(*v as u16) },
        (IRType::UInt32, Value::Int(v)) => unsafe { (ptr as *mut u32).write_unaligned(*v as u32) },
        (IRType::UInt64, Value::Int(v)) => unsafe { (ptr as *mut u64).write_unaligned(*v as u64) },
        (other_ty, other_v) => {
            return Err(RuntimeError::Unsupported {
                detail: format!(
                    "{label}: cannot write `{other_v}` as `T = {other_ty:?}` — primitive \
                     types only",
                ),
            });
        }
    }
    Ok(())
}
