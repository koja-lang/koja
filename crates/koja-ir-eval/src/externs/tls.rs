//! Externs declared in `lib/net/src/tls.koja` (`@link "ssl:..."`).
//!
//! Direct calls into BoringSSL's libssl C ABI — the same archives
//! [`crate::externs::crypto`] links via `boring-sys`. Every symbol
//! is a pure pass-through, so the whole surface rides
//! [`super::marshal::pass_through_externs!`].
//!
//! Argument shapes mirror the Koja declarations one-for-one (see
//! `externs/crypto.rs` for the width policy: Koja-shaped `Int64` /
//! `CPtr` even where the C signature is narrower or
//! `const`-qualified, matching the call shape the LLVM backend
//! emits off the same `@extern "C"` declarations).
//!
//! Blocking-fd note: eval sockets are blocking (see
//! [`crate::externs::net`]), so `SSL_connect` / `SSL_accept` /
//! `SSL_read` / `SSL_write` block until the operation completes and
//! never surface `SSL_ERROR_WANT_READ` / `WANT_WRITE` — the
//! stdlib's `fd.block(...)` retry path simply never runs under
//! eval.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    bio_free => fn BIO_free(bio: CPtr) -> Int32;
    bio_new_mem_buf => fn BIO_new_mem_buf(buf: CPtr, len: Int64) -> CPtr;
    err_clear_error => fn ERR_clear_error() -> ();
    evp_pkey_free => fn EVP_PKEY_free(key: CPtr) -> ();
    pem_read_bio_private_key => fn PEM_read_bio_PrivateKey(
        bio: CPtr,
        out: CPtr,
        callback: CPtr,
        userdata: CPtr,
    ) -> CPtr;
    pem_read_bio_x509 => fn PEM_read_bio_X509(
        bio: CPtr,
        out: CPtr,
        callback: CPtr,
        userdata: CPtr,
    ) -> CPtr;
    ssl_accept => fn SSL_accept(ssl: CPtr) -> Int32;
    ssl_connect => fn SSL_connect(ssl: CPtr) -> Int32;
    ssl_ctx_add1_chain_cert => fn SSL_CTX_add1_chain_cert(ctx: CPtr, cert: CPtr) -> Int32;
    ssl_ctx_free => fn SSL_CTX_free(ctx: CPtr) -> ();
    ssl_ctx_get_cert_store => fn SSL_CTX_get_cert_store(ctx: CPtr) -> CPtr;
    ssl_ctx_load_verify_locations => fn SSL_CTX_load_verify_locations(
        ctx: CPtr,
        file: CPtr,
        dir: CPtr,
    ) -> Int32;
    ssl_ctx_new => fn SSL_CTX_new(method: CPtr) -> CPtr;
    ssl_ctx_set_default_verify_paths => fn SSL_CTX_set_default_verify_paths(ctx: CPtr) -> Int32;
    ssl_ctx_set_verify => fn SSL_CTX_set_verify(ctx: CPtr, mode: Int64, callback: CPtr) -> ();
    ssl_ctx_use_certificate => fn SSL_CTX_use_certificate(ctx: CPtr, cert: CPtr) -> Int32;
    ssl_ctx_use_private_key => fn SSL_CTX_use_PrivateKey(ctx: CPtr, key: CPtr) -> Int32;
    ssl_free => fn SSL_free(ssl: CPtr) -> ();
    ssl_get_error => fn SSL_get_error(ssl: CPtr, ret: Int64) -> Int32;
    ssl_get_verify_result => fn SSL_get_verify_result(ssl: CPtr) -> Int64;
    ssl_new => fn SSL_new(ctx: CPtr) -> CPtr;
    ssl_read => fn SSL_read(ssl: CPtr, buf: CPtr, num: Int64) -> Int32;
    ssl_set1_host => fn SSL_set1_host(ssl: CPtr, name: CPtr) -> Int32;
    ssl_set_fd => fn SSL_set_fd(ssl: CPtr, fd: Int32) -> Int32;
    ssl_set_tlsext_host_name => fn SSL_set_tlsext_host_name(ssl: CPtr, name: CPtr) -> Int32;
    ssl_shutdown => fn SSL_shutdown(ssl: CPtr) -> Int32;
    ssl_write => fn SSL_write(ssl: CPtr, buf: CPtr, num: Int64) -> Int32;
    tls_method => fn TLS_method() -> CPtr;
    x509_free => fn X509_free(cert: CPtr) -> ();
    x509_store_add_cert => fn X509_STORE_add_cert(store: CPtr, cert: CPtr) -> Int32;
    x509_verify_cert_error_string => fn X509_verify_cert_error_string(code: Int64) -> CPtr;
}
