# expo-runtime

C ABI static library linked into compiled Expo binaries. Multi-threaded process scheduler and system intrinsics.

## Key files

- `scheduler.rs` -- Multi-threaded round-robin scheduler, process spawn/send/receive, mailboxes, signal handling, shutdown (~791 lines)
- `reactor.rs` -- Non-blocking I/O reactor via `polling` crate (kqueue/epoll). Processes suspend on EAGAIN, wake on readiness
- `socket.rs` -- `expo_socket_*` non-blocking socket syscalls with reactor integration
- `fs.rs` -- `expo_fd_*` and `expo_file_*` file I/O helpers with reactor integration
- `string.rs` -- Parse/format helpers for strings and binaries at runtime
- `system.rs` -- `expo_cwd`, env vars, time, hostname
- `panic.rs` -- Panic handler with DWARF backtraces and Elixir-style formatting
- `ffi.rs` -- libc/socket constants and platform FFI declarations
- `util.rs` -- Allocation helpers, `STRING_HEADER_SIZE`, thread-local last I/O error
- `build.rs` -- Compiles arch-specific asm (aarch64/x86_64) for stack-based context switching

## C ABI exports

All public functions use `#[no_mangle] extern "C"` and are called by compiler-generated code:
`expo_rt_spawn`, `expo_rt_send`, `expo_rt_receive`, `expo_rt_main_done`, `expo_rt_self`, etc.

## Adding new runtime intrinsics

1. Add the `extern "C"` function in the appropriate file (fs.rs, socket.rs, system.rs, etc.)
2. Declare it in `expo-codegen/src/builtins.rs`
3. Call it from the codegen intrinsic in `expo-codegen/src/intrinsics/`
