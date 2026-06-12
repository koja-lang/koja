//! Externs declared in `lib/global/src/fd.koja`.
//!
//! Three families:
//!
//! - **Plain fd I/O** (`koja_fd_close` / `koja_fd_read` / `koja_fd_write`)
//!   — call straight into [`koja_runtime::fs`] over libc so eval and
//!   the LLVM backend observe the same kernel return values.
//! - **File-path operations** (`koja_file_*`) — wrap the runtime's
//!   path-based helpers; the runtime owns null-termination and CStr
//!   parsing on the C side.
//! - **Actor-coupled I/O** (`koja_io_block`, `koja_rt_watch_fd`,
//!   `koja_rt_unwatch_fd`) — register here so dispatch routes them,
//!   but they require an initialized scheduler / reactor in the
//!   runtime; calling them from a plain `koja eval` panics inside
//!   the runtime's `REACTOR.get().expect(...)`. That's the same
//!   behavior the LLVM backend exhibits when the runtime hasn't been
//!   booted, so the byte-equivalent contract holds.
//!
//! `Value::Int` carries every sized-integer width inside eval; the
//! generated handlers narrow on the way out (`as i32`) at the C ABI
//! boundary.

use crate::externs::marshal::pass_through_externs;

pass_through_externs! {
    fd_close => fn koja_fd_close(fd: Int32) -> Int32;
    fd_read => fn koja_fd_read(fd: Int32, count: Int64) -> CPtr;
    fd_write => fn koja_fd_write(fd: Int32, data: CPtr, len: Int64) -> Int64;
    file_delete => fn koja_file_delete(path: CPtr) -> Int64;
    file_exists => fn koja_file_exists(path: CPtr) -> Int64;
    file_open => fn koja_file_open(path: CPtr, mode: Int64) -> Int32;
    file_read_all => fn koja_file_read_all(path: CPtr) -> CPtr;
    file_rename => fn koja_file_rename(src: CPtr, dst: CPtr) -> Int64;
    file_write_all => fn koja_file_write_all(path: CPtr, content: CPtr) -> Int64;
    io_block => fn koja_io_block(fd: Int32, readable: Int64) -> ();
    rt_unwatch_fd => fn koja_rt_unwatch_fd(fd: Int32) -> ();
    rt_watch_fd => fn koja_rt_watch_fd(fd: Int32, interest: Int64) -> ();
}
