//! Per-backend dispatch table for `@extern "C"` function bodies on
//! the eval interpreter side. Mirrors [`crate::intrinsics`] in
//! shape: each registered extern is keyed by its C symbol name —
//! the same string the LLVM backend declares the function under
//! ([`koja_ir::IRExternAttrs::link_name`] when present, or
//! [`koja_ir::IRSymbol::last_segment`] otherwise) — and routed
//! to a hand-written handler that calls into `koja-runtime` (or
//! libc) over the same C ABI symbol the LLVM backend would.
//!
//! Modules in this folder mirror the stdlib source files that
//! declare the extern (`@extern "C"` shims live next to the methods
//! that call them in `lib/global/src/<name>.koja`), so a reader can
//! cross-reference one-to-one. Calling into the runtime via
//! `extern "C"` (rather than re-implementing the body in pure Rust)
//! keeps eval byte-equivalent with the LLVM backend by construction:
//! both backends execute the same machine code for the body.
//!
//! Adding a new extern: drop / extend the sibling `<name>.rs` module
//! matching the Koja source file, list the symbol in a
//! [`marshal::pass_through_externs!`] invocation (or hand-write the
//! handler when it needs more than arg/return marshaling), then
//! register `(c_symbol, handler)` in [`dispatch`]. Externs not in
//! the table fall through with `None` so the caller can surface
//! [`RuntimeError::ExternNotSupported`] with the mangled symbol
//! attached for the diagnostic.

use crate::error::RuntimeError;
use crate::value::Value;

mod cptr;
mod crypto;
mod fd;
mod kernel;
mod marshal;
mod net;
mod random;
mod runtime;
mod system;
mod time;
mod tls;

/// Run the registered extern under C symbol `link_name` against
/// `args`. Returns `None` when no handler is registered so the
/// caller can surface [`RuntimeError::ExternNotSupported`].
///
/// `async` because the cooperative I/O externs suspend: `koja_io_block`
/// and the fd / socket read-write wrappers park the process (or, in
/// function mode, block the thread) on fd readiness via the reactor before
/// the syscall. Every other extern resolves synchronously.
pub(crate) async fn dispatch(
    link_name: &str,
    args: &[Value],
) -> Option<Result<Value, RuntimeError>> {
    match link_name {
        "BIO_free" => Some(tls::bio_free(args)),
        "BIO_new_mem_buf" => Some(tls::bio_new_mem_buf(args)),
        "ERR_clear_error" => Some(tls::err_clear_error(args)),
        "EVP_DigestFinal_ex" => Some(crypto::evp_digest_final_ex(args)),
        "EVP_DigestInit_ex" => Some(crypto::evp_digest_init_ex(args)),
        "EVP_DigestUpdate" => Some(crypto::evp_digest_update(args)),
        "EVP_MD_CTX_free" => Some(crypto::evp_md_ctx_free(args)),
        "EVP_MD_CTX_new" => Some(crypto::evp_md_ctx_new(args)),
        "EVP_PKEY_free" => Some(tls::evp_pkey_free(args)),
        "EVP_sha1" => Some(crypto::evp_sha1(args)),
        "EVP_sha256" => Some(crypto::evp_sha256(args)),
        "EVP_sha384" => Some(crypto::evp_sha384(args)),
        "EVP_sha512" => Some(crypto::evp_sha512(args)),
        "HMAC" => Some(crypto::hmac(args)),
        "PEM_read_bio_PrivateKey" => Some(tls::pem_read_bio_private_key(args)),
        "PEM_read_bio_X509" => Some(tls::pem_read_bio_x509(args)),
        "SHA1" => Some(crypto::sha1(args)),
        "SHA256" => Some(crypto::sha256(args)),
        "SHA384" => Some(crypto::sha384(args)),
        "SHA512" => Some(crypto::sha512(args)),
        "SSL_CTX_add1_chain_cert" => Some(tls::ssl_ctx_add1_chain_cert(args)),
        "SSL_CTX_free" => Some(tls::ssl_ctx_free(args)),
        "SSL_CTX_get_cert_store" => Some(tls::ssl_ctx_get_cert_store(args)),
        "SSL_CTX_load_verify_locations" => Some(tls::ssl_ctx_load_verify_locations(args)),
        "SSL_CTX_new" => Some(tls::ssl_ctx_new(args)),
        "SSL_CTX_set_default_verify_paths" => Some(tls::ssl_ctx_set_default_verify_paths(args)),
        "SSL_CTX_set_verify" => Some(tls::ssl_ctx_set_verify(args)),
        "SSL_CTX_use_PrivateKey" => Some(tls::ssl_ctx_use_private_key(args)),
        "SSL_CTX_use_certificate" => Some(tls::ssl_ctx_use_certificate(args)),
        "SSL_accept" => Some(tls::ssl_accept(args)),
        "SSL_connect" => Some(tls::ssl_connect(args)),
        "SSL_free" => Some(tls::ssl_free(args)),
        "SSL_get_error" => Some(tls::ssl_get_error(args)),
        "SSL_get_verify_result" => Some(tls::ssl_get_verify_result(args)),
        "SSL_new" => Some(tls::ssl_new(args)),
        "SSL_read" => Some(tls::ssl_read(args)),
        "SSL_set1_host" => Some(tls::ssl_set1_host(args)),
        "SSL_set_fd" => Some(tls::ssl_set_fd(args)),
        "SSL_set_tlsext_host_name" => Some(tls::ssl_set_tlsext_host_name(args)),
        "SSL_shutdown" => Some(tls::ssl_shutdown(args)),
        "SSL_write" => Some(tls::ssl_write(args)),
        "TLS_method" => Some(tls::tls_method(args)),
        "X509_STORE_add_cert" => Some(tls::x509_store_add_cert(args)),
        "X509_free" => Some(tls::x509_free(args)),
        "X509_verify_cert_error_string" => Some(tls::x509_verify_cert_error_string(args)),
        "koja_cwd" => Some(system::cwd(args)),
        "koja_errno_code" => Some(net::errno_code(args)),
        "koja_fd_close" => Some(fd::fd_close(args)),
        "koja_fd_read" => Some(fd::fd_read(args).await),
        "koja_fd_write" => Some(fd::fd_write(args).await),
        "koja_file_delete" => Some(fd::file_delete(args)),
        "koja_file_exists" => Some(fd::file_exists(args)),
        "koja_file_open" => Some(fd::file_open(args)),
        "koja_file_read_all" => Some(fd::file_read_all(args)),
        "koja_file_rename" => Some(fd::file_rename(args)),
        "koja_file_write_all" => Some(fd::file_write_all(args)),
        "koja_get_env" => Some(system::get_env(args)),
        "koja_hostname" => Some(system::hostname(args)),
        "koja_io_block" => Some(fd::io_block(args).await),
        "koja_kernel_exit" => Some(kernel::exit(args)),
        "koja_last_error" => Some(net::last_error(args)),
        "koja_last_error_code" => Some(net::last_error_code(args)),
        "koja_random_bytes" => Some(random::bytes(args)),
        "koja_random_int" => Some(random::int(args)),
        "koja_rt_live_blocks" => Some(runtime::live_blocks(args)),
        "koja_rt_sched_violations" => Some(runtime::sched_violations(args)),
        "koja_rt_unwatch_fd" => Some(fd::rt_unwatch_fd(args)),
        "koja_rt_watch_fd" => Some(fd::rt_watch_fd(args)),
        "koja_set_env" => Some(system::set_env(args)),
        "koja_socket_accept" => Some(net::socket_accept(args).await),
        "koja_socket_bind" => Some(net::socket_bind(args)),
        "koja_socket_connect" => Some(net::socket_connect(args)),
        "koja_socket_create" => Some(net::socket_create(args)),
        "koja_socket_listen" => Some(net::socket_listen(args)),
        "koja_socket_send_to" => Some(net::socket_send_to(args).await),
        "koja_socket_setsockopt_reuse" => Some(net::socket_setsockopt_reuse(args)),
        "koja_socket_try_accept" => Some(net::socket_try_accept(args)),
        "koja_time_now_millis" => Some(time::now_millis(args)),
        "strlen" => Some(cptr::strlen_(args)),
        _ => None,
    }
}
