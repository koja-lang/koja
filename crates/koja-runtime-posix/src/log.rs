//! Runtime plumbing under the stdlib `Log` module, the C ABI behind
//! the package-private `LogRuntime` intrinsics in
//! `lib/global/src/log.koja`.
//!
//! Each process carries a log floor word and a busy word on its table
//! slot (see `koja_runtime_core::process_table`) and an optional
//! configuration payload on its execution state. The runtime inspects
//! none of the payload. It copies the bytes in on `configure`, clones
//! them into each child at `spawn`, and copies them back out on every
//! `config` read, so the Koja side always holds a value that shares no
//! heap with the slot. The clone is a memcpy followed by the deep-copy
//! shim the LLVM backend mints for the configuration type, which
//! replaces every heap pointer the memcpy duplicated with a fresh
//! block.

use std::ptr;

use koja_runtime_core::wire::OwnedPayload;

use crate::memory;
use crate::scheduler::{CURRENT_PID, TABLE};

/// A by-pointer glue the runtime calls over payload bytes: the drop
/// shim releases them, the copy shim severs their heap shares in
/// place. Null when the payload owns no nested heap.
type PayloadGlue = Option<unsafe extern "C" fn(*mut u8)>;

/// A log configuration the runtime holds on behalf of `Log`: the Koja
/// struct bytes plus their length, drop glue, and deep-copy glue.
/// Dropping it runs the drop glue, which is the right thing for a
/// process that died or reconfigured.
pub(crate) struct LogConfig {
    payload: OwnedPayload,
    len: usize,
    copy_glue: PayloadGlue,
}

impl LogConfig {
    /// Copies `len` bytes from `payload` into a fresh configuration.
    ///
    /// # Safety
    /// `payload` must point to `len` readable bytes.
    unsafe fn copy_in(
        payload: *const u8,
        len: usize,
        drop_glue: PayloadGlue,
        copy_glue: PayloadGlue,
    ) -> Self {
        let buf = memory::alloc(len);
        unsafe { ptr::copy_nonoverlapping(payload, buf, len) };
        Self {
            payload: OwnedPayload::new(buf, drop_glue),
            len,
            copy_glue,
        }
    }

    /// A configuration whose bytes own an independent copy of this
    /// one's nested heap. What a child receives at `spawn`.
    pub(crate) fn duplicate(&self) -> Self {
        let buf = memory::alloc(self.len);
        unsafe {
            ptr::copy_nonoverlapping(self.payload.as_ptr(), buf, self.len);
            if let Some(copy_glue) = self.copy_glue {
                copy_glue(buf);
            }
        }
        Self {
            payload: OwnedPayload::new(buf, self.payload.drop_glue()),
            len: self.len,
            copy_glue: self.copy_glue,
        }
    }

    /// Writes an independent copy of the configuration into `out`,
    /// which must hold at least `len` bytes since the copy shim walks
    /// the whole value. The caller owns the copy's nested heap.
    fn copy_out(&self, out: *mut u8, out_cap: i64) {
        assert!(
            out_cap >= 0 && out_cap as usize >= self.len,
            "Log: config copied out into {out_cap} bytes, the stored value needs {}",
            self.len
        );
        unsafe {
            ptr::copy_nonoverlapping(self.payload.as_ptr(), out, self.len);
            if let Some(copy_glue) = self.copy_glue {
                copy_glue(out);
            }
        }
    }
}

/// Runs `f` over the calling process's stored configuration.
fn with_log<R>(f: impl FnOnce(&mut Option<LogConfig>) -> R) -> R {
    let pid = CURRENT_PID.with(|c| c.get());
    // The calling process is the slot's `on_cpu` claim holder for the
    // whole call, which is what `with_execution` requires.
    unsafe { TABLE.with_execution(pid, |execution| f(&mut execution.log)) }
        .expect("a running process has an execution slot")
}

/// The configuration a child of the calling process starts with: a
/// clone of the caller's, or `None` on the host thread and for a
/// process that never configured. Called by `koja_rt_spawn` before
/// the child is registered.
pub(crate) fn inherited_config() -> Option<LogConfig> {
    let pid = CURRENT_PID.with(|c| c.get());
    if pid <= 0 {
        return None;
    }
    // The spawner is running, so it is its own slot's claim holder.
    unsafe {
        TABLE.with_execution(pid, |execution| {
            execution.log.as_ref().map(LogConfig::duplicate)
        })
    }
    .flatten()
}

/// The calling process's log floor word.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_log_level() -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.log_level(pid) as i64
}

/// Stores a copy of the `len`-byte configuration at `payload` and the
/// floor word `level` on the calling process. A configuration already
/// stored is released.
///
/// # Safety
/// `payload` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_log_configure(
    payload: *const u8,
    len: i64,
    drop_glue: PayloadGlue,
    copy_glue: PayloadGlue,
    level: i64,
) {
    let config = unsafe { LogConfig::copy_in(payload, len as usize, drop_glue, copy_glue) };
    let previous = with_log(|log| log.replace(config));
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.set_log_level(pid, level as u64);
    // Release the old configuration off the execution borrow.
    drop(previous);
}

/// Copies the calling process's configuration into `out` (at least
/// `out_cap` bytes) and returns `0`, or returns `-1` when none is
/// stored. The caller owns the copy.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_log_config(out: *mut u8, out_cap: i64) -> i64 {
    with_log(|log| match log {
        Some(config) => {
            config.copy_out(out, out_cap);
            0
        }
        None => -1,
    })
}

/// Marks the calling process as running its log handlers and returns
/// `1`, or returns `0` when it already was.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_log_enter() -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());
    i64::from(TABLE.log_enter(pid))
}

/// Clears the mark `koja_rt_log_enter` set.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_log_leave() {
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.log_leave(pid);
}
