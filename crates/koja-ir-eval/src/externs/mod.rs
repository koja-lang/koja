//! Per-backend dispatch table for `@extern "C"` function bodies on
//! the eval interpreter side. Mirrors [`crate::intrinsics`] in
//! shape: each registered extern is keyed by its C symbol name,
//! the same string the LLVM backend declares the function under
//! ([`koja_ir::IRExternAttrs::link_name`] when present, or
//! [`koja_ir::IRSymbol::last_segment`] otherwise), and routed
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
//! add a `"c_symbol" => handler(args)` row to the [`extern_table!`]
//! invocation, keeping ASCII order. Externs not in the table fall
//! through with `None` so the caller can surface
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

/// Expand one `symbol => handler` table into two views: the
/// `dispatch` match and the sorted [`SUPPORTED_EXTERNS`] list the
/// driver consults before choosing a backend. Keeping both behind
/// one macro means a new extern cannot land in one and not the
/// other. The parameter names come from the invocation so the
/// handler expressions can refer to them across the macro boundary.
macro_rules! extern_table {
    (
        $(#[$meta:meta])*
        async fn $dispatch:ident($link_name:ident, $args:ident) {
            $($symbol:literal => $handler:expr,)*
        }
    ) => {
        /// Every C symbol `dispatch` can run, in ASCII order.
        pub(crate) const SUPPORTED_EXTERNS: &[&str] = &[$($symbol,)*];

        $(#[$meta])*
        pub(crate) async fn $dispatch(
            $link_name: &str,
            $args: &[Value],
        ) -> Option<Result<Value, RuntimeError>> {
            match $link_name {
                $($symbol => Some($handler),)*
                _ => None,
            }
        }
    };
}

extern_table! {
    /// Run the registered extern under C symbol `link_name` against
    /// `args`. Returns `None` when no handler is registered so the
    /// caller can surface [`RuntimeError::ExternNotSupported`].
    ///
    /// `async` because the cooperative I/O externs suspend: `koja_io_block`
    /// and the fd / socket read-write wrappers park the process (or, in
    /// function mode, block the thread) on fd readiness via the reactor before
    /// the syscall. Every other extern resolves synchronously.
    async fn dispatch(link_name, args) {
        "BIO_free" => tls::bio_free(args),
        "BIO_new_mem_buf" => tls::bio_new_mem_buf(args),
        "ERR_clear_error" => tls::err_clear_error(args),
        "EVP_DigestFinal_ex" => crypto::evp_digest_final_ex(args),
        "EVP_DigestInit_ex" => crypto::evp_digest_init_ex(args),
        "EVP_DigestUpdate" => crypto::evp_digest_update(args),
        "EVP_MD_CTX_free" => crypto::evp_md_ctx_free(args),
        "EVP_MD_CTX_new" => crypto::evp_md_ctx_new(args),
        "EVP_PKEY_free" => tls::evp_pkey_free(args),
        "EVP_sha1" => crypto::evp_sha1(args),
        "EVP_sha256" => crypto::evp_sha256(args),
        "EVP_sha384" => crypto::evp_sha384(args),
        "EVP_sha512" => crypto::evp_sha512(args),
        "HMAC" => crypto::hmac(args),
        "PEM_read_bio_PrivateKey" => tls::pem_read_bio_private_key(args),
        "PEM_read_bio_X509" => tls::pem_read_bio_x509(args),
        "SHA1" => crypto::sha1(args),
        "SHA256" => crypto::sha256(args),
        "SHA384" => crypto::sha384(args),
        "SHA512" => crypto::sha512(args),
        "SSL_CTX_add1_chain_cert" => tls::ssl_ctx_add1_chain_cert(args),
        "SSL_CTX_free" => tls::ssl_ctx_free(args),
        "SSL_CTX_get_cert_store" => tls::ssl_ctx_get_cert_store(args),
        "SSL_CTX_load_verify_locations" => tls::ssl_ctx_load_verify_locations(args),
        "SSL_CTX_new" => tls::ssl_ctx_new(args),
        "SSL_CTX_set_default_verify_paths" => tls::ssl_ctx_set_default_verify_paths(args),
        "SSL_CTX_set_verify" => tls::ssl_ctx_set_verify(args),
        "SSL_CTX_use_PrivateKey" => tls::ssl_ctx_use_private_key(args),
        "SSL_CTX_use_certificate" => tls::ssl_ctx_use_certificate(args),
        "SSL_accept" => tls::ssl_accept(args),
        "SSL_connect" => tls::ssl_connect(args),
        "SSL_free" => tls::ssl_free(args),
        "SSL_get_error" => tls::ssl_get_error(args),
        "SSL_get_verify_result" => tls::ssl_get_verify_result(args),
        "SSL_new" => tls::ssl_new(args),
        "SSL_read" => tls::ssl_read(args),
        "SSL_set1_host" => tls::ssl_set1_host(args),
        "SSL_set_fd" => tls::ssl_set_fd(args),
        "SSL_set_tlsext_host_name" => tls::ssl_set_tlsext_host_name(args),
        "SSL_shutdown" => tls::ssl_shutdown(args),
        "SSL_write" => tls::ssl_write(args),
        "TLS_method" => tls::tls_method(args),
        "X509_STORE_add_cert" => tls::x509_store_add_cert(args),
        "X509_free" => tls::x509_free(args),
        "X509_verify_cert_error_string" => tls::x509_verify_cert_error_string(args),
        "koja_cwd" => system::cwd(args),
        "koja_errno_code" => net::errno_code(args),
        "koja_fd_close" => fd::fd_close(args),
        "koja_fd_read" => fd::fd_read(args).await,
        "koja_fd_write" => fd::fd_write(args).await,
        "koja_file_delete" => fd::file_delete(args),
        "koja_file_exists" => fd::file_exists(args),
        "koja_file_is_dir" => fd::file_is_dir(args),
        "koja_file_mkdir" => fd::file_mkdir(args),
        "koja_file_mkdir_p" => fd::file_mkdir_p(args),
        "koja_file_open" => fd::file_open(args),
        "koja_file_read_all" => fd::file_read_all(args),
        "koja_file_rename" => fd::file_rename(args),
        "koja_file_rmdir" => fd::file_rmdir(args),
        "koja_file_write_all" => fd::file_write_all(args),
        "koja_get_env" => system::get_env(args),
        "koja_hostname" => system::hostname(args),
        "koja_io_block" => fd::io_block(args).await,
        "koja_kernel_exit" => kernel::exit(args),
        "koja_last_error_code" => net::last_error_code(args),
        "koja_random_bytes" => random::bytes(args),
        "koja_random_int" => random::int(args),
        "koja_rt_live_blocks" => runtime::live_blocks(args),
        "koja_rt_mailbox_depth" => runtime::mailbox_depth(args),
        "koja_rt_process_count" => runtime::process_count(args),
        "koja_rt_process_count_by_state" => runtime::process_count_by_state(args),
        "koja_rt_process_state" => runtime::process_state(args),
        "koja_rt_sched_violations" => runtime::sched_violations(args),
        "koja_rt_scheduler_count" => runtime::scheduler_count(args),
        "koja_rt_self_mailbox_depth" => runtime::self_mailbox_depth(args),
        "koja_rt_unwatch_fd" => fd::rt_unwatch_fd(args),
        "koja_rt_watch_fd" => fd::rt_watch_fd(args),
        "koja_set_env" => system::set_env(args),
        "koja_socket_accept" => net::socket_accept(args).await,
        "koja_socket_bind" => net::socket_bind(args),
        "koja_socket_connect" => net::socket_connect(args),
        "koja_socket_create" => net::socket_create(args),
        "koja_socket_listen" => net::socket_listen(args),
        "koja_socket_send_to" => net::socket_send_to(args).await,
        "koja_socket_setsockopt_reuse" => net::socket_setsockopt_reuse(args),
        "koja_socket_try_accept" => net::socket_try_accept(args),
        "koja_time_now_millis" => time::now_millis(args),
        "koja_toolchain_version" => system::toolchain_version(args),
        "strlen" => cptr::strlen_(args),
    }
}

#[cfg(test)]
mod tests {
    use super::SUPPORTED_EXTERNS;

    // `supports_extern` binary-searches the table, so the rows must
    // stay in ASCII order.
    #[test]
    fn table_is_sorted_and_unique() {
        assert!(SUPPORTED_EXTERNS.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
