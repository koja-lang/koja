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
mod system;
mod time;

/// Run the registered extern under C symbol `link_name` against
/// `args`. Returns `None` when no handler is registered so the
/// caller can surface [`RuntimeError::ExternNotSupported`].
pub(crate) fn dispatch(link_name: &str, args: &[Value]) -> Option<Result<Value, RuntimeError>> {
    match link_name {
        "EVP_DigestFinal_ex" => Some(crypto::evp_digest_final_ex(args)),
        "EVP_DigestInit_ex" => Some(crypto::evp_digest_init_ex(args)),
        "EVP_DigestUpdate" => Some(crypto::evp_digest_update(args)),
        "EVP_MD_CTX_free" => Some(crypto::evp_md_ctx_free(args)),
        "EVP_MD_CTX_new" => Some(crypto::evp_md_ctx_new(args)),
        "EVP_sha1" => Some(crypto::evp_sha1(args)),
        "EVP_sha256" => Some(crypto::evp_sha256(args)),
        "EVP_sha384" => Some(crypto::evp_sha384(args)),
        "EVP_sha512" => Some(crypto::evp_sha512(args)),
        "HMAC" => Some(crypto::hmac(args)),
        "SHA1" => Some(crypto::sha1(args)),
        "SHA256" => Some(crypto::sha256(args)),
        "SHA384" => Some(crypto::sha384(args)),
        "SHA512" => Some(crypto::sha512(args)),
        "koja_cwd" => Some(system::cwd(args)),
        "koja_errno_code" => Some(net::errno_code(args)),
        "koja_fd_close" => Some(fd::fd_close(args)),
        "koja_fd_read" => Some(fd::fd_read(args)),
        "koja_fd_write" => Some(fd::fd_write(args)),
        "koja_file_delete" => Some(fd::file_delete(args)),
        "koja_file_exists" => Some(fd::file_exists(args)),
        "koja_file_open" => Some(fd::file_open(args)),
        "koja_file_read_all" => Some(fd::file_read_all(args)),
        "koja_file_rename" => Some(fd::file_rename(args)),
        "koja_file_write_all" => Some(fd::file_write_all(args)),
        "koja_get_env" => Some(system::get_env(args)),
        "koja_hostname" => Some(system::hostname(args)),
        "koja_io_block" => Some(fd::io_block(args)),
        "koja_kernel_exit" => Some(kernel::exit(args)),
        "koja_last_error" => Some(net::last_error(args)),
        "koja_last_error_code" => Some(net::last_error_code(args)),
        "koja_random_bytes" => Some(random::bytes(args)),
        "koja_random_int" => Some(random::int(args)),
        "koja_rt_unwatch_fd" => Some(fd::rt_unwatch_fd(args)),
        "koja_rt_watch_fd" => Some(fd::rt_watch_fd(args)),
        "koja_set_env" => Some(system::set_env(args)),
        "koja_socket_accept" => Some(net::socket_accept(args)),
        "koja_socket_bind" => Some(net::socket_bind(args)),
        "koja_socket_connect" => Some(net::socket_connect(args)),
        "koja_socket_create" => Some(net::socket_create(args)),
        "koja_socket_listen" => Some(net::socket_listen(args)),
        "koja_socket_send_to" => Some(net::socket_send_to(args)),
        "koja_socket_setsockopt_reuse" => Some(net::socket_setsockopt_reuse(args)),
        "koja_socket_try_accept" => Some(net::socket_try_accept(args)),
        "koja_time_now_millis" => Some(time::now_millis(args)),
        "strlen" => Some(cptr::strlen_(args)),
        _ => None,
    }
}
