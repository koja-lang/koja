//! Envelope wire format — the ABI between emitted code and the runtime.
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
//! constants on the backend side — `ENVELOPE_PAYLOAD_OFFSET` in
//! `koja-ir-llvm` and `ReceiveTag::wire_byte` in `koja-ir` — mirror
//! these values by spec, not via a shared type (a shared crate would
//! solidify a Rust-level coupling that self-hosting is meant to
//! remove). A mismatch needs no dedicated test: the `lang_process_*` /
//! `lang_io` suites read garbage the moment the offsets disagree.
//!
//! Tags partition the mailbox into dispatch classes, not payload
//! shapes — a reply is `Business` traffic flowing callee->caller, not
//! a category of its own (see `koja/design/MESSAGE-LIFECYCLE.md`).

use std::alloc;

/// Forward business traffic and replies. Payload is the message: a
/// `Pair<M, Option<ReplyTo<R>>>` going to the target, or a bare `R`
/// reply going back to a caller.
#[allow(dead_code)]
pub(crate) const TAG_BUSINESS: u8 = 0;
/// Lifecycle signal. Payload is the lifecycle variant byte.
pub(crate) const TAG_LIFECYCLE: u8 = 1;
/// I/O readiness event from the reactor. Payload is the IOReady
/// variant byte followed by the `Fd`.
pub(crate) const TAG_IO_READY: u8 = 2;

/// Bytes reserved for the tag header. The payload begins at this
/// offset; backends know this value as the envelope payload offset.
pub(crate) const TAG_HEADER_SIZE: usize = 8;

/// Total size of a lifecycle envelope: tag header + one variant byte.
pub(crate) const LIFECYCLE_BUF_SIZE: usize = 16;

/// Total size of an IOReady envelope: tag header + variant byte + `Fd`.
pub(crate) const IO_READY_BUF_SIZE: usize = 24;
/// Offset of the IOReady variant byte within the envelope.
pub(crate) const IO_READY_VARIANT_OFFSET: usize = 8;
/// Offset of the `Fd` (i64) within an IOReady envelope.
pub(crate) const IO_READY_FD_OFFSET: usize = 16;

/// IOReady variant: the fd became readable.
pub(crate) const IO_READY_READ: u8 = 0;
/// IOReady variant: the fd became writable.
pub(crate) const IO_READY_WRITE: u8 = 1;
/// IOReady variant: the fd reported an error or hangup.
pub(crate) const IO_READY_ERROR: u8 = 2;

/// An owned mailbox message: the tagged transport buffer plus the
/// metadata needed to free it without consulting the send site.
///
/// The wire tag itself lives in the buffer at offset 0 (read by
/// codegen); a typed `tag` field is added when the receive path starts
/// returning it (deferred; see `koja/design/MESSAGE-LIFECYCLE.md`).
///
/// Freeing is always explicit — there is no `Drop` impl — and splits
/// by path: the delivered-receive path copies the payload into the
/// receiver's slot and frees only the transport buffer via
/// [`Envelope::free_transport`] (nested heap moves to the receiver),
/// while the discard path (process death, send-to-dead) uses
/// [`Envelope::free`], which also runs `drop_glue` over the payload.
pub(crate) struct Envelope {
    /// Transport buffer `[tag header | payload]`, owned by the global
    /// allocator (`alloc::alloc`, 8-byte aligned).
    pub(crate) buffer: *mut u8,
    /// Drop glue for nested Koja heap in the payload, run before the
    /// buffer is freed on the discard path. Null until the deferred
    /// codegen phase that emits it.
    pub(crate) drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    /// Total buffer length in bytes, so the allocation `Layout` is
    /// recoverable here rather than only at the send site.
    pub(crate) length: usize,
}

/// Envelope owns a heap pointer detached from any thread, so moving it
/// across worker threads is sound.
unsafe impl Send for Envelope {}

impl Envelope {
    /// Wraps a freshly allocated transport buffer of `length` bytes,
    /// with no payload drop glue.
    pub(crate) fn new(buffer: *mut u8, length: usize) -> Self {
        Self {
            buffer,
            drop_glue: None,
            length,
        }
    }

    /// Discard-path free: runs payload drop glue (when present) over the
    /// undelivered payload, then frees the transport buffer. Consumes the
    /// envelope.
    pub(crate) fn free(self) {
        if let Some(drop_glue) = self.drop_glue {
            unsafe { drop_glue(self.buffer.add(TAG_HEADER_SIZE)) };
        }
        self.free_transport();
    }

    /// Delivered-path free: frees the transport buffer only, never
    /// running `drop_glue`. The payload's nested heap has already been
    /// copied into the receiver's frame, which now owns it. Consumes the
    /// envelope.
    pub(crate) fn free_transport(self) {
        unsafe {
            let layout = alloc::Layout::from_size_align(self.length, 8).unwrap();
            alloc::dealloc(self.buffer, layout);
        }
    }
}
