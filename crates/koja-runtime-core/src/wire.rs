//! Envelope wire format: the ABI between emitted code and the runtime.
//!
//! A mailbox message is a heap buffer laid out as a fixed-size tag
//! header followed by the payload:
//!
//! ```text
//! offset 0              offset TAG_HEADER_SIZE
//! [ tag: u8 | padding ][ payload ... ]
//! ```
//!
//! This layout is a contract, not an implementation detail. The
//! compiler backend emits code that stamps the tag and reads the
//! payload at these offsets, and the runtime's `koja_rt_send` /
//! `koja_rt_receive` family produces and consumes the same shape.
//! Treat it like the `koja_rt_*` function signatures: an ABI that any
//! backend (LLVM today, others after self-hosting) must conform to.
//!
//! **This module is the authoritative definition.** The conforming
//! constants on the backend side (`ENVELOPE_PAYLOAD_OFFSET` in
//! `koja-ir-llvm` and `ReceiveTag::wire_byte` in `koja-ir`) mirror
//! these values by spec, not via a shared type, because a shared crate
//! would solidify a Rust-level coupling that self-hosting is meant to
//! remove. A mismatch needs no dedicated test: the
//! `lang_process_*` / `lang_io` suites read garbage the moment the
//! offsets disagree.
//!
//! Tags are routing classes, not payload shapes: they decide which part
//! of the receiver's mailbox an envelope lands in (see
//! [`crate::mailbox`]). Receive arms only ever see `TAG_BUSINESS`,
//! `TAG_LIFECYCLE`, `TAG_IO_READY`, and `TAG_EXIT_SIGNAL`. `TAG_REPLY`
//! is consumed by the blocked caller inside `koja_rt_call_receive` and
//! never surfaces.

use std::ptr;

use crate::memory;
use crate::protocol::{Message, Tag};

/// Forward business traffic: casts, call requests, timer fires. Payload
/// is the message, a `Pair<M, Option<ReplyTo<R>>>`.
pub const TAG_BUSINESS: u8 = 0;
/// Lifecycle signal. Payload is the lifecycle variant byte.
pub const TAG_LIFECYCLE: u8 = 1;
/// I/O readiness event from the reactor. Payload is the IOReady
/// variant byte followed by the `Fd`.
pub const TAG_IO_READY: u8 = 2;
/// Reply to an in-flight `Ref.call`, correlated by the envelope's
/// [`reply_token`](Envelope::reply_token). Routed to the caller's
/// one-shot reply slot, never the receive queues.
pub const TAG_REPLY: u8 = 3;
/// A monitor's exit notification. Payload is a bare
/// `Process.ExitSignal` struct (see the `EXIT_SIGNAL_*` offsets).
pub const TAG_EXIT_SIGNAL: u8 = 4;

/// Bytes reserved for the tag header. The payload begins at this
/// offset. Backends know this value as the envelope payload offset.
pub const TAG_HEADER_SIZE: usize = 8;

/// Total size of a lifecycle envelope: tag header + one variant byte.
pub const LIFECYCLE_BUF_SIZE: usize = 16;

/// Total size of an IOReady envelope: tag header + variant byte + `Fd`.
pub const IO_READY_BUF_SIZE: usize = 24;
/// Offset of the IOReady variant byte within the envelope.
pub const IO_READY_VARIANT_OFFSET: usize = 8;
/// Offset of the `Fd` (i64) within an IOReady envelope.
pub const IO_READY_FD_OFFSET: usize = 16;

/// IOReady variant: the fd became readable.
pub const IO_READY_READ: u8 = 0;
/// IOReady variant: the fd became writable.
pub const IO_READY_WRITE: u8 = 1;
/// IOReady variant: the fd reported an error or hangup.
pub const IO_READY_ERROR: u8 = 2;

/// Total size of an ExitSignal envelope: tag header + the 32-byte
/// `Process.ExitSignal` struct (`Pid` i64, then the `ExitReason`
/// outer: tag byte, 7 pad bytes, two `CrashInfo` string pointers).
pub const EXIT_SIGNAL_BUF_SIZE: usize = 40;
/// Offset of the dying process's `Pid` (i64) within the envelope.
pub const EXIT_SIGNAL_PID_OFFSET: usize = 8;
/// Offset of the `ExitReason` tag byte within the envelope.
pub const EXIT_SIGNAL_REASON_OFFSET: usize = 16;
/// Offset of `CrashInfo.message` (heap `String` pointer, null unless
/// `Crashed`) within the envelope.
pub const EXIT_SIGNAL_MESSAGE_OFFSET: usize = 24;
/// Offset of `CrashInfo.backtrace` (heap `String` pointer, null unless
/// `Crashed`) within the envelope.
pub const EXIT_SIGNAL_BACKTRACE_OFFSET: usize = 32;

/// An owned mailbox message: the tagged transport buffer plus the
/// metadata needed to free it without consulting the send site.
///
/// Freeing is RAII via [`Drop`], which runs `drop_glue` over the
/// payload (when present) and then deallocates the buffer: the
/// discard semantics for any undelivered envelope (process death,
/// send-to-dead, stale reply). The delivered-receive path is the one
/// exception: it copies the payload into the receiver's slot and then
/// opts out of the glue via [`Envelope::free_transport`], so only the
/// transport buffer is freed (the nested heap has moved to the
/// receiver).
pub struct Envelope {
    /// Transport buffer `[tag header | payload]`, owned by the
    /// allocator funnel (`memory::alloc`, 8-byte aligned) and freed with
    /// `memory::free` on drop.
    pub buffer: *mut u8,
    /// Drop glue for nested Koja heap in the payload, run before the
    /// buffer is freed on the discard path. Null when the payload owns
    /// no nested heap.
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    /// Total buffer length in bytes, so the delivered-receive copy can
    /// size the payload without consulting the send site.
    pub length: usize,
    /// Correlation token, meaningful only for [`TAG_REPLY`] envelopes
    /// (zero otherwise). `koja_rt_call_receive` discards a slotted
    /// reply whose token doesn't match the in-flight call.
    pub reply_token: i64,
}

/// Envelope owns a heap pointer detached from any thread, so moving it
/// across worker threads is sound.
unsafe impl Send for Envelope {}

impl Envelope {
    /// Wraps a hand-built transport buffer of `length` bytes (the tag
    /// must already be stamped at offset 0), with no payload drop glue.
    pub fn new(buffer: *mut u8, length: usize) -> Self {
        Self {
            buffer,
            drop_glue: None,
            length,
            reply_token: 0,
        }
    }

    /// Allocates a transport buffer, stamps `tag`, and copies
    /// `payload_len` bytes from `payload` in after the tag header.
    ///
    /// # Safety
    /// `payload` must point to `payload_len` readable bytes (it may be
    /// anything, including null, when `payload_len` is zero).
    pub unsafe fn from_payload(
        tag: u8,
        payload: *const u8,
        payload_len: usize,
        drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    ) -> Self {
        let length = TAG_HEADER_SIZE + payload_len;
        let buffer = memory::alloc(length);
        unsafe {
            ptr::write_bytes(buffer, 0, TAG_HEADER_SIZE);
            *buffer = tag;
            if payload_len > 0 {
                ptr::copy_nonoverlapping(payload, buffer.add(TAG_HEADER_SIZE), payload_len);
            }
        }
        Self {
            buffer,
            drop_glue,
            length,
            reply_token: 0,
        }
    }

    /// The wire tag byte stamped at offset 0 of the transport buffer.
    pub fn tag_byte(&self) -> u8 {
        unsafe { *self.buffer }
    }

    /// Delivered-path defuse: the receiver has already copied the
    /// payload (and any nested heap it references) into its own frame,
    /// so the transport buffer must be freed *without* running
    /// `drop_glue`. Clearing the glue and letting the envelope drop
    /// deallocates the buffer only. Consumes the envelope.
    pub fn free_transport(mut self) {
        self.drop_glue = None;
    }
}

/// Routing tag for the mailbox: maps the wire byte to its [`Tag`] class.
impl Message for Envelope {
    fn tag(&self) -> Tag {
        match self.tag_byte() {
            TAG_LIFECYCLE => Tag::Lifecycle,
            TAG_IO_READY => Tag::IOReady,
            TAG_REPLY => Tag::Reply,
            TAG_EXIT_SIGNAL => Tag::ExitSignal,
            _ => Tag::Business,
        }
    }

    fn reply_token(&self) -> i64 {
        self.reply_token
    }
}

/// Discard-path free: runs payload drop glue (when present) over the
/// undelivered payload, then frees the transport buffer. This is the
/// default for any envelope that is dropped without being delivered:
/// undelivered mail on process death, sends to a dead target, stale
/// replies, etc. The delivered path opts out via
/// [`Envelope::free_transport`].
impl Drop for Envelope {
    fn drop(&mut self) {
        if let Some(drop_glue) = self.drop_glue {
            unsafe { drop_glue(self.buffer.add(TAG_HEADER_SIZE)) };
        }
        unsafe { memory::free(self.buffer) };
    }
}

/// An owned, untagged payload: a heap buffer of Koja value bytes plus
/// the drop glue for any nested heap it references. The RAII
/// counterpart of [`Envelope`] for payloads that live outside a
/// transport buffer: a process's spawn config (`init_state`).
///
/// The empty value ([`Default`]) is null and drops as a no-op, so it
/// doubles as the placeholder left behind when ownership moves out via
/// `mem::take`.
pub struct OwnedPayload {
    buf: *mut u8,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
}

/// OwnedPayload owns a heap pointer detached from any thread, so moving
/// it across worker threads is sound.
unsafe impl Send for OwnedPayload {}

impl OwnedPayload {
    /// Wraps an allocation from [`memory::alloc`] (or null for empty)
    /// and the drop glue for its nested heap (or null for none).
    pub fn new(buf: *mut u8, drop_glue: Option<unsafe extern "C" fn(*mut u8)>) -> Self {
        Self { buf, drop_glue }
    }

    /// The payload bytes, or null for the empty value.
    pub fn as_ptr(&self) -> *const u8 {
        self.buf
    }
}

impl Default for OwnedPayload {
    fn default() -> Self {
        Self {
            buf: ptr::null_mut(),
            drop_glue: None,
        }
    }
}

impl Drop for OwnedPayload {
    fn drop(&mut self) {
        if self.buf.is_null() {
            return;
        }
        if let Some(drop_glue) = self.drop_glue {
            unsafe { drop_glue(self.buf) };
        }
        unsafe { memory::free(self.buf) };
    }
}
