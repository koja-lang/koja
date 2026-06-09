//! Per-process mailbox: two receive queues plus a one-shot reply slot.
//!
//! Routing is by wire tag. Lifecycle signals land in the `system`
//! queue, which `receive` drains before any business traffic, so a
//! shutdown request can't be starved by a deep backlog. Casts, call
//! requests, timer fires, and I/O readiness land in the `business`
//! queue in arrival order.
//!
//! Replies (`TAG_REPLY`) bypass both queues into a one-shot `reply`
//! slot read only by `koja_rt_call_receive`. Calls are atomic — a
//! caller blocked in `Ref.call` handles no other traffic until the
//! call completes or times out — so at most one reply is expected at a
//! time and a slot, not a queue, is sufficient. A reply that arrives
//! while the slot is occupied displaces the occupant: the occupant is
//! necessarily stale (a leftover from an earlier call that timed out),
//! and the newest reply is the one the in-flight call is waiting on.

use std::collections::VecDeque;

use crate::wire::{Envelope, TAG_LIFECYCLE, TAG_REPLY};

/// Which part of the mailbox a blocked process is waiting on. Stored on
/// the process when it parks so delivery only wakes it for traffic that
/// can actually satisfy the wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitTarget {
    /// The receive queues — woken by system or business traffic.
    Receive,
    /// The reply slot — woken only by a `TAG_REPLY` delivery.
    Reply,
}

/// A process's mailbox. Owned by the process slot and drained only by
/// the process itself; dropping it discards every held envelope
/// (running each one's payload drop glue).
#[derive(Default)]
pub(crate) struct Mailbox {
    /// Business traffic in arrival order.
    business: VecDeque<Envelope>,
    /// The reply to the in-flight `Ref.call`, if it has arrived.
    reply: Option<Envelope>,
    /// Lifecycle signals, drained before business traffic.
    system: VecDeque<Envelope>,
}

impl Mailbox {
    /// Which wait target `envelope` will satisfy when pushed.
    pub(crate) fn target_of(envelope: &Envelope) -> WaitTarget {
        if envelope.tag() == TAG_REPLY {
            WaitTarget::Reply
        } else {
            WaitTarget::Receive
        }
    }

    /// Routes an incoming envelope by its wire tag. Returns a displaced
    /// stale reply (when the slot still held a leftover from a
    /// timed-out call) for the caller to drop after releasing the
    /// scheduler lock.
    pub(crate) fn push(&mut self, envelope: Envelope) -> Option<Envelope> {
        match envelope.tag() {
            TAG_LIFECYCLE => {
                self.system.push_back(envelope);
                None
            }
            TAG_REPLY => self.reply.replace(envelope),
            _ => {
                self.business.push_back(envelope);
                None
            }
        }
    }

    /// Next envelope for `receive`: system traffic first, then
    /// business. Never surfaces the reply slot.
    pub(crate) fn pop_received(&mut self) -> Option<Envelope> {
        self.system
            .pop_front()
            .or_else(|| self.business.pop_front())
    }

    /// Takes the pending reply, if one has arrived.
    pub(crate) fn take_reply(&mut self) -> Option<Envelope> {
        self.reply.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{TAG_BUSINESS, TAG_IO_READY};

    fn envelope(tag: u8, reply_token: i64) -> Envelope {
        let mut envelope = unsafe { Envelope::from_payload(tag, std::ptr::null(), 0, None) };
        envelope.reply_token = reply_token;
        envelope
    }

    #[test]
    fn system_traffic_is_drained_before_business() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.push(envelope(TAG_BUSINESS, 0)).is_none());
        assert!(mailbox.push(envelope(TAG_IO_READY, 0)).is_none());
        assert!(mailbox.push(envelope(TAG_LIFECYCLE, 0)).is_none());

        let tags: Vec<u8> = std::iter::from_fn(|| mailbox.pop_received())
            .map(|envelope| envelope.tag())
            .collect();
        assert_eq!(tags, vec![TAG_LIFECYCLE, TAG_BUSINESS, TAG_IO_READY]);
    }

    #[test]
    fn replies_fill_the_slot_and_never_surface_in_receive() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.push(envelope(TAG_REPLY, 7)).is_none());
        assert!(
            mailbox.pop_received().is_none(),
            "reply hidden from receive"
        );
        assert_eq!(mailbox.take_reply().map(|e| e.reply_token), Some(7));
        assert!(mailbox.take_reply().is_none(), "slot is one-shot");
    }

    #[test]
    fn newer_reply_displaces_stale_occupant() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.push(envelope(TAG_REPLY, 1)).is_none());
        let displaced = mailbox.push(envelope(TAG_REPLY, 2));
        assert_eq!(displaced.map(|e| e.reply_token), Some(1));
        assert_eq!(mailbox.take_reply().map(|e| e.reply_token), Some(2));
    }

    #[test]
    fn wait_targets_partition_by_tag() {
        assert_eq!(
            Mailbox::target_of(&envelope(TAG_BUSINESS, 0)),
            WaitTarget::Receive
        );
        assert_eq!(
            Mailbox::target_of(&envelope(TAG_LIFECYCLE, 0)),
            WaitTarget::Receive
        );
        assert_eq!(
            Mailbox::target_of(&envelope(TAG_REPLY, 0)),
            WaitTarget::Reply
        );
    }
}
