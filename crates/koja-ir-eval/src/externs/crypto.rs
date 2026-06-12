//! Externs declared in `lib/crypto/src/{sha1,sha256,sha384,sha512,hmac}.koja`.
//!
//! Direct calls into BoringSSL's libcrypto C ABI. The static
//! archives are pulled into the link graph by the `boring-sys` dep
//! on this crate (its `#[link]` attributes do the work).
//!
//! Argument shapes mirror the Koja declarations one-for-one (every
//! Koja-side `Int64` arrives here as `i64`; every Koja-side
//! `CPtr<UInt8>` as `*mut u8`). Where the actual C signature uses a
//! narrower or differently-qualified type (`int` returns, `*mut u32`
//! out-params, `const` pointers) we declare the extern with the
//! wider Koja-shaped type to keep eval ABI-equivalent with the LLVM
//! backend, which emits the same call shape directly off the
//! `@extern "C"` declaration.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    evp_digest_final_ex => fn EVP_DigestFinal_ex(ctx: CPtr, out: CPtr, out_len: CPtr) -> Int64;
    evp_digest_init_ex => fn EVP_DigestInit_ex(ctx: CPtr, md: CPtr, engine: CPtr) -> Int64;
    evp_digest_update => fn EVP_DigestUpdate(ctx: CPtr, data: CPtr, len: Int64) -> Int64;
    evp_md_ctx_free => fn EVP_MD_CTX_free(ctx: CPtr) -> ();
    evp_md_ctx_new => fn EVP_MD_CTX_new() -> CPtr;
    evp_sha1 => fn EVP_sha1() -> CPtr;
    evp_sha256 => fn EVP_sha256() -> CPtr;
    evp_sha384 => fn EVP_sha384() -> CPtr;
    evp_sha512 => fn EVP_sha512() -> CPtr;
    hmac => fn HMAC(
        evp_md: CPtr,
        key: CPtr,
        key_len: Int64,
        data: CPtr,
        data_len: Int64,
        out: CPtr,
        out_len: CPtr,
    ) -> CPtr;
    sha1 => fn SHA1(data: CPtr, len: Int64, out: CPtr) -> CPtr;
    sha256 => fn SHA256(data: CPtr, len: Int64, out: CPtr) -> CPtr;
    sha384 => fn SHA384(data: CPtr, len: Int64, out: CPtr) -> CPtr;
    sha512 => fn SHA512(data: CPtr, len: Int64, out: CPtr) -> CPtr;
}
