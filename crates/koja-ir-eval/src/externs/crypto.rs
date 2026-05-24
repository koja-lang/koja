//! Externs declared in `lib/crypto/src/{sha1,sha256,sha384,sha512,hmac}.koja`.
//!
//! Direct calls into BoringSSL's libcrypto C ABI. The static
//! archives are pulled into the link graph by the `boring-sys` dep
//! on this crate (its `#[link]` attributes do the work).
//!
//! Argument shapes mirror the Koja declarations one-for-one (every
//! Koja-side `Int64` arrives here as `i64`; every Koja-side
//! `CPtr<UInt8>` as `*mut u8` / `*const u8`). Where the actual C
//! signature uses a narrower type (`int` for `EVP_DigestInit_ex` /
//! `EVP_DigestUpdate` / `EVP_DigestFinal_ex` returns and HMAC's
//! `key_len`) we declare the extern with the wider Koja-shaped type
//! to keep eval ABI-equivalent with the LLVM backend, which emits
//! the same call shape directly off the `@extern "C"` declaration.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn EVP_DigestFinal_ex(ctx: *mut u8, out: *mut u8, out_len: *mut u32) -> i64;
    fn EVP_DigestInit_ex(ctx: *mut u8, md: *const u8, engine: *mut u8) -> i64;
    fn EVP_DigestUpdate(ctx: *mut u8, data: *const u8, len: i64) -> i64;
    fn EVP_MD_CTX_free(ctx: *mut u8);
    fn EVP_MD_CTX_new() -> *mut u8;
    fn EVP_sha1() -> *const u8;
    fn EVP_sha256() -> *const u8;
    fn EVP_sha384() -> *const u8;
    fn EVP_sha512() -> *const u8;
    fn HMAC(
        evp_md: *const u8,
        key: *const u8,
        key_len: i64,
        data: *const u8,
        data_len: i64,
        out: *mut u8,
        out_len: *mut u32,
    ) -> *mut u8;
    fn SHA1(data: *const u8, len: i64, out: *mut u8) -> *mut u8;
    fn SHA256(data: *const u8, len: i64, out: *mut u8) -> *mut u8;
    fn SHA384(data: *const u8, len: i64, out: *mut u8) -> *mut u8;
    fn SHA512(data: *const u8, len: i64, out: *mut u8) -> *mut u8;
}

pub(super) fn sha1(args: &[Value]) -> Result<Value, RuntimeError> {
    sha_one_shot("SHA1", args, |d, l, o| unsafe { SHA1(d, l, o) })
}

pub(super) fn sha256(args: &[Value]) -> Result<Value, RuntimeError> {
    sha_one_shot("SHA256", args, |d, l, o| unsafe { SHA256(d, l, o) })
}

pub(super) fn sha384(args: &[Value]) -> Result<Value, RuntimeError> {
    sha_one_shot("SHA384", args, |d, l, o| unsafe { SHA384(d, l, o) })
}

pub(super) fn sha512(args: &[Value]) -> Result<Value, RuntimeError> {
    sha_one_shot("SHA512", args, |d, l, o| unsafe { SHA512(d, l, o) })
}

pub(super) fn evp_sha1(args: &[Value]) -> Result<Value, RuntimeError> {
    evp_md("EVP_sha1", args, || unsafe { EVP_sha1() })
}

pub(super) fn evp_sha256(args: &[Value]) -> Result<Value, RuntimeError> {
    evp_md("EVP_sha256", args, || unsafe { EVP_sha256() })
}

pub(super) fn evp_sha384(args: &[Value]) -> Result<Value, RuntimeError> {
    evp_md("EVP_sha384", args, || unsafe { EVP_sha384() })
}

pub(super) fn evp_sha512(args: &[Value]) -> Result<Value, RuntimeError> {
    evp_md("EVP_sha512", args, || unsafe { EVP_sha512() })
}

pub(super) fn evp_md_ctx_new(args: &[Value]) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(type_mismatch("EVP_MD_CTX_new", "()", args));
    }
    Ok(Value::CPtr(unsafe { EVP_MD_CTX_new() }))
}

pub(super) fn evp_md_ctx_free(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ctx)] = args else {
        return Err(type_mismatch("EVP_MD_CTX_free", "(ctx: CPtr<UInt8>)", args));
    };
    unsafe { EVP_MD_CTX_free(*ctx) };
    Ok(Value::Unit)
}

pub(super) fn evp_digest_init_ex(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ctx), Value::CPtr(md), Value::CPtr(engine)] = args else {
        return Err(type_mismatch(
            "EVP_DigestInit_ex",
            "(ctx: CPtr<UInt8>, md: CPtr<UInt8>, engine: CPtr<UInt8>)",
            args,
        ));
    };
    let rc = unsafe { EVP_DigestInit_ex(*ctx, *md as *const u8, *engine) };
    Ok(Value::Int(rc))
}

pub(super) fn evp_digest_update(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ctx), Value::CPtr(data), Value::Int(len)] = args else {
        return Err(type_mismatch(
            "EVP_DigestUpdate",
            "(ctx: CPtr<UInt8>, data: CPtr<UInt8>, len: Int64)",
            args,
        ));
    };
    let rc = unsafe { EVP_DigestUpdate(*ctx, *data as *const u8, *len) };
    Ok(Value::Int(rc))
}

pub(super) fn evp_digest_final_ex(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ctx), Value::CPtr(out), Value::CPtr(out_len)] = args else {
        return Err(type_mismatch(
            "EVP_DigestFinal_ex",
            "(ctx: CPtr<UInt8>, out: CPtr<UInt8>, out_len: CPtr<UInt8>)",
            args,
        ));
    };
    let rc = unsafe { EVP_DigestFinal_ex(*ctx, *out, *out_len as *mut u32) };
    Ok(Value::Int(rc))
}

pub(super) fn hmac(args: &[Value]) -> Result<Value, RuntimeError> {
    let [
        Value::CPtr(evp_md),
        Value::CPtr(key),
        Value::Int(key_len),
        Value::CPtr(data),
        Value::Int(data_len),
        Value::CPtr(out),
        Value::CPtr(out_len),
    ] = args
    else {
        return Err(type_mismatch(
            "HMAC",
            "(evp_md: CPtr<UInt8>, key: CPtr<UInt8>, key_len: Int64, data: CPtr<UInt8>, \
             data_len: Int64, out: CPtr<UInt8>, out_len: CPtr<UInt8>)",
            args,
        ));
    };
    let result = unsafe {
        HMAC(
            *evp_md as *const u8,
            *key as *const u8,
            *key_len,
            *data as *const u8,
            *data_len,
            *out,
            *out_len as *mut u32,
        )
    };
    Ok(Value::CPtr(result))
}

fn sha_one_shot(
    label: &str,
    args: &[Value],
    call: impl FnOnce(*const u8, i64, *mut u8) -> *mut u8,
) -> Result<Value, RuntimeError> {
    let [Value::CPtr(data), Value::Int(len), Value::CPtr(out)] = args else {
        return Err(type_mismatch(
            label,
            "(data: CPtr<UInt8>, len: Int64, out: CPtr<UInt8>)",
            args,
        ));
    };
    Ok(Value::CPtr(call(*data as *const u8, *len, *out)))
}

fn evp_md(
    label: &str,
    args: &[Value],
    call: impl FnOnce() -> *const u8,
) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(type_mismatch(label, "()", args));
    }
    Ok(Value::CPtr(call() as *mut u8))
}

fn type_mismatch(name: &str, signature: &str, args: &[Value]) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!(
            "{name} expects {signature}; got {} arg(s): {args:?}",
            args.len(),
        ),
    }
}
