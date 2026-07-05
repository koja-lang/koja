//! Per-process mailbox: two receive queues plus a one-shot reply slot.
//!
//! Generic over the message representation `M`: the native adapter
//! carries byte [`Envelope`](crate::wire::Envelope)s, and a cooperative
//! adapter can carry typed values. The routing and priority semantics
//! (the part that must stay identical across backends for observable
//! parity) live here once and are driven purely by [`Message::tag`].
//!
//! Routing is by [`Tag`]. Lifecycle signals land in the `system` queue,
//! which `receive` drains before any business traffic, so a shutdown
//! request can't be starved by a deep backlog. Casts, call requests,
//! timer fires, and I/O readiness land in the `business` queue in
//! arrival order.
//!
//! Replies bypass both queues into a one-shot `reply` slot read only by
//! `koja_rt_call_receive`. Calls are atomic (a caller blocked in
//! `Ref.call` handles no other traffic until the call completes or
//! times out), so at most one reply is expected at a time and a slot,
//! not a queue, is sufficient. A reply that arrives while the slot is
//! occupied displaces the occupant: the occupant is necessarily stale
//! (a leftover from an earlier call that timed out), and the newest
//! reply is the one the in-flight call is waiting on.

use std::collections::VecDeque;

use crate::protocol::{Message, Tag};

/// Which part of the mailbox a blocked process is waiting on. Stored on
/// the process when it parks so delivery only wakes it for traffic that
/// can actually satisfy the wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitTarget {
    /// The receive queues, woken by system or business traffic.
    Receive,
    /// The reply slot, woken only by a reply delivery.
    Reply,
}

/// A process's mailbox. Owned by the process slot and drained only by
/// the process itself. Dropping it discards every held message (running
/// each one's drop glue, for representations that carry it).
pub struct Mailbox<M> {
    /// Business traffic in arrival order.
    business: VecDeque<M>,
    /// The reply to the in-flight `Ref.call`, if it has arrived.
    reply: Option<M>,
    /// Lifecycle signals, drained before business traffic.
    system: VecDeque<M>,
}

// Manual `Default` so `Mailbox<M>` is default-constructible regardless
// of whether `M: Default` (the queues and slot start empty).
impl<M> Default for Mailbox<M> {
    fn default() -> Self {
        Self {
            business: VecDeque::new(),
            reply: None,
            system: VecDeque::new(),
        }
    }
}

impl<M: Message> Mailbox<M> {
    /// Which wait target `message` will satisfy when pushed.
    pub fn target_of(message: &M) -> WaitTarget {
        if message.tag() == Tag::Reply {
            WaitTarget::Reply
        } else {
            WaitTarget::Receive
        }
    }

    /// Routes an incoming message by its [`Tag`]. Returns a displaced
    /// stale reply (when the slot still held a leftover from a
    /// timed-out call) for the caller to drop after releasing the
    /// scheduler lock.
    pub fn push(&mut self, message: M) -> Option<M> {
        match message.tag() {
            Tag::Lifecycle => {
                self.system.push_back(message);
                None
            }
            Tag::Reply => self.reply.replace(message),
            Tag::Business | Tag::ExitSignal | Tag::IOReady => {
                self.business.push_back(message);
                None
            }
        }
    }

    /// Next message for `receive`: system traffic first, then business.
    /// Never surfaces the reply slot.
    pub fn pop_received(&mut self) -> Option<M> {
        self.system
            .pop_front()
            .or_else(|| self.business.pop_front())
    }

    /// Whether any system/lifecycle traffic is queued.
    pub fn has_system(&self) -> bool {
        !self.system.is_empty()
    }

    /// Takes the pending reply, if one has arrived.
    pub fn take_reply(&mut self) -> Option<M> {
        self.reply.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Envelope, TAG_BUSINESS, TAG_IO_READY, TAG_LIFECYCLE, TAG_REPLY};

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
            .map(|envelope| envelope.tag_byte())
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
            Mailbox::<Envelope>::target_of(&envelope(TAG_BUSINESS, 0)),
            WaitTarget::Receive
        );
        assert_eq!(
            Mailbox::<Envelope>::target_of(&envelope(TAG_LIFECYCLE, 0)),
            WaitTarget::Receive
        );
        assert_eq!(
            Mailbox::<Envelope>::target_of(&envelope(TAG_REPLY, 0)),
            WaitTarget::Reply
        );
    }
}
