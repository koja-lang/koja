//! Runtime plumbing under the stdlib `Trace` module, the C ABI behind
//! the package-private `TraceRuntime` intrinsics in
//! `lib/global/src/trace.koja`.
//!
//! The runtime holds two kinds of span record for `Trace` and inspects
//! neither. Open records sit on a per-process stack in the process's
//! execution state, indexed by the handle `Trace` hands the closure.
//! Finished records go to one bounded export queue that an exporter
//! process drains. A record moves between Koja and the runtime the way
//! a message payload does: bytes plus drop glue in, bytes out with the
//! glue defused because the caller now owns the nested heap.
//!
//! Span ids come from a per-thread xoshiro generator seeded once from
//! the OS, so a sampled span costs no system call.

use std::cell::RefCell;
use std::ptr;

use koja_runtime_core::ExportQueue;
use koja_runtime_core::context::Context;
use koja_runtime_core::span_ids::SpanIds;
use koja_runtime_core::wire::OwnedPayload;

use crate::memory;
use crate::scheduler::{CURRENT_PID, TABLE};
use crate::system::fill_random;

/// A span record the runtime holds on behalf of `Trace`: the Koja
/// struct bytes plus their length and drop glue. Dropping it runs the
/// glue, which is the right thing for a record nobody will read again
/// (a process that died mid-span, a record the full export queue
/// rejected).
pub(crate) struct SpanRecord {
    payload: OwnedPayload,
    len: usize,
}

impl SpanRecord {
    /// Copies `len` bytes from `payload` into a fresh record.
    ///
    /// # Safety
    /// `payload` must point to `len` readable bytes.
    unsafe fn copy_in(
        payload: *const u8,
        len: usize,
        drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    ) -> Self {
        let buf = memory::alloc(len);
        unsafe { ptr::copy_nonoverlapping(payload, buf, len) };
        Self {
            payload: OwnedPayload::new(buf, drop_glue),
            len,
        }
    }

    /// Moves the record's bytes into `out` (at most `out_cap` bytes)
    /// and frees only the buffer. The nested heap the bytes reference
    /// now belongs to the caller.
    fn move_out(self, out: *mut u8, out_cap: i64) {
        let copy_len = self.len.min(out_cap.max(0) as usize);
        unsafe { ptr::copy_nonoverlapping(self.payload.as_ptr(), out, copy_len) };
        self.payload.free_transport();
    }
}

/// Finished span records waiting for an exporter.
static EXPORTS: ExportQueue<SpanRecord> = ExportQueue::new();

thread_local! {
    /// This worker's span id generator, seeded on first use.
    static SPAN_IDS: RefCell<Option<SpanIds>> = const { RefCell::new(None) };
}

/// Runs `f` over the calling process's open span stack.
fn with_spans<R>(f: impl FnOnce(&mut Vec<Option<SpanRecord>>) -> R) -> R {
    let pid = CURRENT_PID.with(|c| c.get());
    // The calling process is the slot's `on_cpu` claim holder for the
    // whole call, which is what `with_execution` requires.
    unsafe { TABLE.with_execution(pid, |execution| f(&mut execution.spans)) }
        .expect("a running process has an execution slot")
}

/// Installs the 32-byte `Process.Context` at `context` on the calling
/// process. `Trace.root` and `Trace.span` call it around their work and
/// put back what they found.
///
/// # Safety
/// `context` must point to `CONTEXT_SIZE` (32) readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_trace_install(context: *const u8) {
    let pid = CURRENT_PID.with(|c| c.get());
    let context = unsafe { ptr::read_unaligned(context.cast::<Context>()) };
    TABLE.set_context(pid, context);
}

/// A fresh non-zero 64-bit id from this worker's generator.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_span_id() -> i64 {
    SPAN_IDS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let ids = slot.get_or_insert_with(|| {
            let mut seed = [0u8; 32];
            fill_random(seed.as_mut_ptr(), seed.len());
            SpanIds::from_seed(seed)
        });
        ids.next_id() as i64
    })
}

/// Pushes a copy of the `len`-byte record at `payload` onto the calling
/// process's open span stack and returns its handle.
///
/// # Safety
/// `payload` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_span_open(
    payload: *const u8,
    len: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) -> i64 {
    let record = unsafe { SpanRecord::copy_in(payload, len as usize, drop_glue) };
    with_spans(|spans| {
        spans.push(Some(record));
        (spans.len() - 1) as i64
    })
}

/// Moves the open record at `handle` out into `out` (at most `out_cap`
/// bytes), leaving the slot empty until `koja_rt_span_put` fills it.
/// An empty or missing slot is a `Trace` bug and aborts the runtime.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_span_take(handle: i64, out: *mut u8, out_cap: i64) {
    let record = with_spans(|spans| {
        spans
            .get_mut(handle as usize)
            .and_then(Option::take)
            .unwrap_or_else(|| panic!("Trace: span_take on handle {handle} with no open record"))
    });
    record.move_out(out, out_cap);
}

/// Moves a copy of the `len`-byte record at `payload` back into the
/// empty slot at `handle`. A filled or missing slot is a `Trace` bug
/// and aborts the runtime.
///
/// # Safety
/// `payload` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_span_put(
    handle: i64,
    payload: *const u8,
    len: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let record = unsafe { SpanRecord::copy_in(payload, len as usize, drop_glue) };
    with_spans(|spans| {
        let slot = spans
            .get_mut(handle as usize)
            .unwrap_or_else(|| panic!("Trace: span_put on handle {handle} past the open stack"));
        assert!(
            slot.is_none(),
            "Trace: span_put on handle {handle} that still holds a record"
        );
        *slot = Some(record);
    });
}

/// Pops the innermost open record, which must be `handle`, into `out`
/// (at most `out_cap` bytes). Closing out of order or closing a slot
/// left empty by `koja_rt_span_take` is a `Trace` bug and aborts the
/// runtime.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_span_close(handle: i64, out: *mut u8, out_cap: i64) {
    let record = with_spans(|spans| {
        assert!(
            spans.len() == handle as usize + 1,
            "Trace: span_close on handle {handle} with {} open spans",
            spans.len()
        );
        spans
            .pop()
            .flatten()
            .unwrap_or_else(|| panic!("Trace: span_close on handle {handle} with no record"))
    });
    record.move_out(out, out_cap);
}

/// Queues a copy of the `len`-byte finished record at `payload` for the
/// exporter. A full queue drops it and counts the drop.
///
/// # Safety
/// `payload` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_export_push(
    payload: *const u8,
    len: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let record = unsafe { SpanRecord::copy_in(payload, len as usize, drop_glue) };
    EXPORTS.push(record);
}

/// Moves the oldest queued record into `out` (at most `out_cap` bytes)
/// and returns `0`, or returns `-1` when the queue is empty.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_export_pop(out: *mut u8, out_cap: i64) -> i64 {
    match EXPORTS.pop() {
        Some(record) => {
            record.move_out(out, out_cap);
            0
        }
        None => -1,
    }
}

/// Records the export queue has dropped since the runtime started.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_export_dropped() -> i64 {
    EXPORTS.dropped() as i64
}
