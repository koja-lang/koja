//! The per-process request context: the 32-byte value the runtime
//! carries on every process slot and every business envelope.
//!
//! The layout mirrors the stdlib `Process.Context` struct field for
//! field, four `Int` words in declaration order, so the read and write
//! intrinsics are one 32-byte copy with no translation. The runtime
//! never interprets the trace or span words. It only tests the sampled
//! bit, copies the value at `spawn`, stamps it onto business envelopes
//! at send, and installs it on the receiver at business dequeue.
//!
//! - Authoritative: this module.
//! - Mirror: `lib/global/src/process.koja` (`Process.Context`) and the
//!   `Process.context` emit in `koja-ir-llvm/src/intrinsics/process.rs`.

/// Bit 0 of [`Context::flags`]: the trace is sampled and downstream
/// spans record. Every other bit is reserved and stays clear.
pub const FLAG_SAMPLED: u64 = 1;

/// Size of the context in bytes, on the slot and on the wire.
pub const CONTEXT_SIZE: usize = 32;

/// The per-process request context. Plain data, freely copied.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct Context {
    /// Bit flags. See [`FLAG_SAMPLED`].
    pub flags: u64,
    /// The current span id, or zero outside any span.
    pub span: u64,
    /// High 8 bytes of the 16-byte trace id.
    pub trace_hi: u64,
    /// Low 8 bytes of the 16-byte trace id.
    pub trace_lo: u64,
}

impl Context {
    /// The context a process starts with before anything installs one.
    /// No trace, no span, sampled bit clear.
    pub const ZERO: Self = Self {
        flags: 0,
        span: 0,
        trace_hi: 0,
        trace_lo: 0,
    };

    /// Whether the sampled bit is set.
    pub fn sampled(&self) -> bool {
        self.flags & FLAG_SAMPLED != 0
    }

    /// The four slot words, in field order.
    pub fn to_words(self) -> [u64; 4] {
        [self.flags, self.span, self.trace_hi, self.trace_lo]
    }

    /// Rebuilds a context from the four slot words, in field order.
    pub fn from_words(words: [u64; 4]) -> Self {
        Self {
            flags: words[0],
            span: words[1],
            trace_hi: words[2],
            trace_lo: words[3],
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{CONTEXT_SIZE, Context, FLAG_SAMPLED};

    #[test]
    fn layout_is_thirty_two_bytes() {
        assert_eq!(size_of::<Context>(), CONTEXT_SIZE);
    }

    #[test]
    fn words_round_trip_in_field_order() {
        let context = Context {
            flags: FLAG_SAMPLED,
            span: 2,
            trace_hi: 3,
            trace_lo: 4,
        };
        assert_eq!(context.to_words(), [1, 2, 3, 4]);
        assert_eq!(Context::from_words([1, 2, 3, 4]), context);
    }

    #[test]
    fn zero_is_unsampled() {
        assert!(!Context::ZERO.sampled());
        assert!(
            Context {
                flags: FLAG_SAMPLED,
                ..Context::ZERO
            }
            .sampled()
        );
    }
}
