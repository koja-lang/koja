//! Externs declared in `lib/global/src/system.koja`.
//!
//! Wrappers over `koja_runtime::system` for env-var, cwd, and
//! hostname queries. Each handler routes through the same C ABI
//! the LLVM backend uses, so eval observes the same OS-state-driven
//! values the native binary would.
//!
//! The pointer-returning calls (`koja_cwd`, `koja_get_env`,
//! `koja_hostname`) hand back a runtime-allocated payload that
//! `CPtr<UInt8>.to_cstring` walks. Eval just round-trips the raw
//! pointer through [`Value::CPtr`]; the runtime side owns the
//! storage layout.
//!
//! `koja_get_env` returns a null pointer when the requested variable
//! is unset, matching the runtime's contract — `System.get_env`'s
//! `ptr.null?()` check then yields `Option.None` without any
//! eval-side branching.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn koja_cwd() -> *const u8;
    fn koja_get_env(key_ptr: *const u8) -> *const u8;
    fn koja_hostname() -> *const u8;
    fn koja_set_env(key_ptr: *const u8, val_ptr: *const u8);
}

pub(super) fn cwd(args: &[Value]) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(type_mismatch("koja_cwd", "()", args));
    }
    let ptr = unsafe { koja_cwd() };
    Ok(Value::CPtr(ptr as *mut u8))
}

pub(super) fn get_env(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(key)] = args else {
        return Err(type_mismatch("koja_get_env", "(key: CPtr<UInt8>)", args));
    };
    let ptr = unsafe { koja_get_env(*key as *const u8) };
    Ok(Value::CPtr(ptr as *mut u8))
}

pub(super) fn hostname(args: &[Value]) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(type_mismatch("koja_hostname", "()", args));
    }
    let ptr = unsafe { koja_hostname() };
    Ok(Value::CPtr(ptr as *mut u8))
}

pub(super) fn set_env(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(key), Value::CPtr(val)] = args else {
        return Err(type_mismatch(
            "koja_set_env",
            "(key: CPtr<UInt8>, val: CPtr<UInt8>)",
            args,
        ));
    };
    unsafe { koja_set_env(*key as *const u8, *val as *const u8) };
    Ok(Value::Unit)
}

fn type_mismatch(name: &str, signature: &str, args: &[Value]) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!(
            "{name} expects {signature}; got {} arg(s): {args:?}",
            args.len(),
        ),
    }
}
