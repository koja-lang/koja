//! Shared scaffolding for the scheduler integration tests.
//!
//! The runtime is a process-global singleton (`SCHED`, the reactor
//! `OnceLock`, signal handlers, and a one-shot `SHUTDOWN` flag), so each
//! test file holds exactly one `#[test]`: a second `koja_rt_main_done` in
//! the same process would observe an already-shutdown scheduler. This
//! module carries the boilerplate those files share: the `koja_rt_*`
//! extern declarations, the app-name symbol generated programs provide,
//! and the blocking-receive helpers.

#![allow(dead_code)]

/// Process entry signature, matching the runtime's `ProcessFn` typedef.
pub type ProcessFn = extern "C" fn(*const u8);

// The runtime exposes its scheduler purely through `#[no_mangle]` C
// symbols, which tests reach via this block.
unsafe extern "C" {
    pub fn koja_rt_is_process_alive(pid: i64) -> i64;
    pub fn koja_rt_kill(pid: i64);
    pub fn koja_rt_main_done();
    pub fn koja_rt_monitor(target: i64) -> i64;
    pub fn koja_rt_parks_refused() -> i64;
    pub fn koja_rt_receive(out: *mut u8, out_cap: i64) -> i64;
    pub fn koja_rt_sched_violations() -> i64;
    pub fn koja_rt_self() -> i64;
    pub fn koja_rt_send(
        pid: i64,
        msg_ptr: *const u8,
        msg_len: i64,
        drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    );
    pub fn koja_rt_spawn(
        fn_ptr: ProcessFn,
        state_ptr: *const u8,
        state_len: i64,
        drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    ) -> i64;
    pub fn koja_rt_yield_check();
}

/// Generated Koja programs emit this null-terminated app-name string and
/// the runtime's panic handler links against it. Provide an empty one so
/// the runtime rlib resolves at link time.
#[unsafe(no_mangle)]
static __koja_app_name: [u8; 1] = [0];

/// The `ExitSignal` wire tag, mirroring `wire::TAG_EXIT_SIGNAL` (a frozen
/// ABI constant, since the core crate is not visible to integration tests).
pub const TAG_EXIT_SIGNAL: i64 = 4;

/// Blocks until a real message arrives and returns its wire tag,
/// retrying spurious empty wakes (`koja_rt_receive` returns -1 when woken
/// with an empty mailbox). The payload is discarded.
pub fn recv_tag_blocking() -> i64 {
    let mut buf = [0u8; 64];
    loop {
        let tag = unsafe { koja_rt_receive(buf.as_mut_ptr(), buf.len() as i64) };
        if tag >= 0 {
            return tag;
        }
    }
}

/// [`recv_tag_blocking`] for callers that only need the arrival.
pub fn recv_blocking() {
    recv_tag_blocking();
}

/// Reads a stress knob from the environment, for `just tsan` to scale
/// soaks up or down.
pub fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Spawns a stateless process.
pub fn spawn_simple(entry: ProcessFn) -> i64 {
    unsafe { koja_rt_spawn(entry, std::ptr::null(), 0, None) }
}

/// Spawns a process whose init state is one PID (8 native-endian bytes),
/// for bodies that need a peer to target.
pub fn spawn_with_pid(entry: ProcessFn, pid: i64) -> i64 {
    let bytes = pid.to_ne_bytes();
    unsafe { koja_rt_spawn(entry, bytes.as_ptr(), bytes.len() as i64, None) }
}

/// The PID an entry received via [`spawn_with_pid`].
///
/// # Safety
/// `state` must point to the 8-byte init state of a `spawn_with_pid`
/// process.
pub unsafe fn state_pid(state: *const u8) -> i64 {
    unsafe { std::ptr::read_unaligned(state.cast::<i64>()) }
}

/// Sends a one-byte message with no drop glue.
pub fn send_byte(pid: i64, byte: u8) {
    unsafe { koja_rt_send(pid, &byte, 1, None) };
}
