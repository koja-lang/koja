# Message & Envelope Lifecycle

Design for a single owned-envelope contract in the Koja runtime so
that every message buffer has exactly one owner, one allocator, one
free path, and one place that defines its wire format. Closes the
mailbox memory leaks left open by the resource-reclamation change
(which freed process stacks and `init_state` but deliberately left
mailbox buffers alone, because freeing them safely is one
underspecified contract, not a quick patch).

This is a destination doc, not a trajectory. Every claim reduces to a
concrete behavior in `koja-runtime/src/scheduler.rs`, the codegen in
`koja-ir-llvm/src/emit/process.rs`, or the IR in
`koja-ir/src/function.rs`. Where the doc proposes a change, it names
the call site it replaces.

## Why

A message in the compiled runtime is a raw heap buffer with an 8-byte
tag header followed by the payload. It is created at a `send` site,
parked in a `VecDeque<*mut u8>` mailbox, and handed back to compiled
code by `koja_rt_receive` as a bare `*const u8`. Nothing owns it:

- **Delivered envelopes leak.** `emit_receive` loads the payload out of
  the buffer (`build_load` at `ENVELOPE_PAYLOAD_OFFSET`) and abandons
  the pointer. The transport buffer is never freed.
- **Undelivered envelopes leak.** `koja_rt_kill` and process death do
  `mailbox.clear()` (or leave the mailbox intact on the `Dead`
  `Process`), dropping the `*mut u8` pointers without `dealloc`.
- **Size is unrecoverable.** The mailbox stores a bare pointer; the
  allocation size (`TAG_HEADER_SIZE + len`) is known only at the
  `send` site and discarded. The Rust global allocator needs the
  `Layout` to free, so even a willing owner can't `dealloc`.

These were acceptable while processes were effectively immortal and
message volume was low. They are not acceptable now that stacks are
reclaimed and long-lived actor systems are a supported workload: a
request/reply server leaks one transport buffer per message, forever.

The fix is not "add a `free` call somewhere." Five tangled
sub-problems all bottom out in the same missing abstraction — an
envelope that owns itself. This doc defines that abstraction once and
threads it through all three layers.

## Problem inventory

The current state, with exact references:

1. **No envelope owner.**
   - Delivered: `koja_rt_receive` / `koja_rt_receive_timeout`
     (`scheduler.rs`) `pop_front()` the pointer and return it;
     `deserialize_payload_into_local` (`emit/process.rs`) loads the
     value and drops the pointer on the floor.
   - Undelivered: `koja_rt_kill` calls `proc.mailbox.clear()`
     (`scheduler.rs`); normal death leaves the mailbox on the `Dead`
     `Process` and `take_resources` intentionally skips it.

2. **Wire format `[tag@0][payload@8]` defined in three places.**
   - Runtime: `TAG_HEADER_SIZE = 8` (`scheduler.rs`), plus the
     `IO_READY_*` offset constants for the tag-2 layout.
   - IR: `ReceiveTag::wire_byte` (`function.rs`) maps
     `Business → 0`, `Lifecycle → 1`.
   - Codegen: `ENVELOPE_PAYLOAD_OFFSET = 8` (`emit/process.rs`).
     The constants comment-reference each other but there is no shared
     definition; a change to the header size must be made in three files
     that don't share a type.

3. **One tag, two payload shapes — by direction.** A reply is not a
   separate wire category; it is a _business message going the other
   way_. Under the `Process<C, M, R>` protocol, `cast` / `call` /
   `send_after` send the forward shape `Pair<M, Option<ReplyTo<R>>>`
   to the target (`koja_rt_send`, tag `Business`), and `ReplyTo.send`
   sends the bare reverse shape `R` back to the caller's pid
   (`koja_rt_send`, _also_ tag `Business`; `emit_reply_to` in
   `intrinsics/process.rs`). Both shapes ride the `Business` tag
   because both _are_ business traffic — the tag intentionally does
   not distinguish them. Disambiguation is by receive _context_: the
   `run`-loop `receive` expects the forward pair; a process blocked in
   `Ref.call` expects the bare `R` (its inline `koja_rt_receive_timeout`
   reads `R` straight off `+8`). The consequence for _discard_ is that
   a dead process's mailbox can hold a mix of forward pairs (addressed
   to it) and pending replies of assorted `R` types (from calls it
   made), and neither size nor shape is recoverable from the tag — so a
   generic "drain and free" routine can't size or drop them from the
   tag alone.

4. **Type erasure blocks drop glue.** A payload can contain nested
   Koja heap (e.g. a `String`, which is a `malloc`'d block pointed to
   from inside the message struct). Freeing the transport buffer does
   not free that nested allocation. Running the right drop glue
   requires a per-message-type descriptor the runtime does not have.

5. **Two allocators, no record of which owns what.** Envelope
   transport buffers come from the Rust global allocator
   (`alloc::alloc` / `alloc::dealloc`, 8-byte aligned). Koja heap
   values (`String` / `Binary`, and `__koja_alloc` in `intrinsics.rs`)
   come from `libc::malloc` / `free` with an `i64` length header at
   `payload - 8`. The two must never be crossed, but nothing in the
   types enforces it.

6. **Timers outlive their target.** A `Timer` that fires after its
   target died still allocates an envelope and `push_back`s it onto the
   `Dead` process's mailbox (the worker-loop timer path checks
   `idx < len` but not `state == Dead`). Death does not cancel pending
   timers, so their staged `msg_buf` also leaks.

## The contract

One type owns every message buffer for its whole life:

```rust
struct Envelope {
    /// Wire tag (see `koja-runtime/src/wire.rs`): the dispatch class
    /// only (Business / Lifecycle / IORead), *not* the payload shape.
    tag: u8,
    /// Transport buffer: `[tag header | payload]`, global-allocator owned.
    buf: *mut u8,
    /// Total buffer length, so the buffer is freeable without the send site.
    len: usize,
    /// Drop glue for nested Koja heap in the payload; null if the
    /// payload owns no heap (the common case for copy-only messages).
    drop_glue: Option<unsafe extern "C" fn(payload: *mut u8)>,
}
```

The mailbox becomes `VecDeque<Envelope>`. From this, each leak closes:

- **Freeable transport.** `len` travels with the buffer, so any owner
  can reconstruct the `Layout` and `dealloc`. `Envelope::free` is the
  single transport free path.
- **Owned delivery.** Receive transfers the `Envelope` to the receiving
  frame; the frame frees the transport buffer once the payload is
  loaded (see "Receive and ownership transfer"). Exactly one free.
- **Owned discard.** On death, undelivered `Envelope`s are handed to
  the existing `Reclaim` and freed off-lock — running `drop_glue` over
  the payload first so nested heap is reclaimed, then freeing the
  transport buffer.
- **Self-describing for discard.** The discard path never needs to know
  whether a payload is a forward pair or a reverse reply: `len` sizes
  the transport buffer and `drop_glue` (stamped by the sender, who knows
  the static type) reclaims any nested heap. Sizing and dropping come
  from the envelope's own fields, not from the tag — which is why
  replies need no tag of their own.

### Wire format: an ABI, not a shared type

The envelope layout is an ABI between emitted code and the runtime,
exactly like the `koja_rt_*` function signatures. The right model is
not a shared Rust type that all three layers import — that would
solidify a crate-level coupling between the backend and the runtime
(and between the backend and `koja-ir`) that self-hosting is meant to
dissolve: post-self-hosting the backend won't be a Rust crate at all.

So the runtime owns the ABI as its authority, and backends conform to
it the same way they already declare conforming `extern` prototypes
for `koja_rt_send` and friends:

- The authoritative definition lives in `koja-runtime/src/wire.rs`:
  the tag bytes (`TAG_BUSINESS = 0`, `TAG_LIFECYCLE = 1`,
  `TAG_IO_READY = 2`), the header size (`TAG_HEADER_SIZE = 8`), and the
  lifecycle / IOReady payload offsets. The module doc states it is the
  spec every backend must match.
- `koja-ir-llvm`'s `ENVELOPE_PAYLOAD_OFFSET` and `koja-ir`'s
  `ReceiveTag::wire_byte` stay local constants, each documented as
  conforming to the runtime ABI. No shared crate, no new
  `llvm -> runtime` or `llvm -> ir` constant dependency.
- Conformance is verified end-to-end: the `lang_process_*` / `lang_io`
  suites read garbage the instant the offsets or tag bytes disagree, so
  a mismatch fails the build. This is the same trust model as the
  `extern` signatures, which are likewise checked by linking + running,
  not by a shared type.

### Why replies do not get their own tag

A reply is a business message flowing callee→caller, so it keeps the
`Business` tag — splitting it out would misrepresent the protocol and
gain nothing. The thing a discard routine actually needs is _size_ and
_nested-heap layout_, and the tag never carried those even for forward
messages (two different `M`s already share `Business`). The fix is to
make every envelope self-describing via `len` + `drop_glue`, both
stamped at the `send` site from the statically-known sent type. This
covers forward pairs and reverse replies uniformly, with no new tag and
no change to receive dispatch.

The tag stays purely a _dispatch_ discriminator for the `run`-loop
`receive`: `Business` → the `pair:` arm, `Lifecycle` → the lifecycle
arm, `IORead` → the (reserved) I/O arm. Replies never reach
`dispatch_arms` at all — they are consumed inline by the blocked
`Ref.call` (next section), which statically knows the payload is `R`.

### Receive and ownership transfer

The destination shape eliminates the dangling transport buffer
entirely. `koja_rt_receive` deserializes into a caller-provided slot
and frees the transport buffer before returning, rather than handing
back a raw pointer the caller must remember to free:

```text
koja_rt_receive(out: *mut u8) -> i64   // returns the wire tag, or -1 (no message)
  pop Envelope from mailbox
  copy `length - TAG_HEADER_SIZE` payload bytes into `out`
  Envelope::free_transport(self)   // nested heap now owned by `out`'s frame
  return tag
```

Nested Koja heap referenced from the payload (e.g. a `String` pointer)
transfers by value into the receiver's local and follows ordinary Koja
ownership from there — no drop glue needed on the _delivered_ path,
because the value is now live in the frame. `drop_glue` is only for the
_discard_ path, where the value never reaches user code.

There are **two** delivered-receive sites, and both must adopt the
owned shape:

- The `run`-loop `receive` (`emit_receive` → `dispatch_arms`): branch on
  the returned tag, deserialize the matching arm's payload.
- The inline reply receive inside `Ref.call` (`emit_call`): the tag is
  always `Business` and the payload is statically `R`; it deserializes
  straight into the call result with no dispatch.

`emit_receive` changes from "load through the returned pointer" to
"point `koja_rt_receive` at the arm's payload `alloca`, then branch on
the returned tag"; `emit_call` makes the same slot-out change for its
`R`. Both keep the single-copy behavior (the payload is memcpy'd once,
as today) while removing the leak and the raw pointer from the ABI.

### Discard path: death and kill

The resource-reclamation change already routes dead-process cleanup
through `Process::take_resources` → `Reclaim::free`, freed off the
`SCHED` lock. Extend `Reclaim` to carry the mailbox:

```rust
struct Reclaim {
    init_state: *mut u8,
    init_state_len: usize,
    mailbox: VecDeque<Envelope>, // NEW
    stack: ProcessStack,
}
```

`take_resources` moves the mailbox out (leaving `VecDeque::new()`);
`Reclaim::free` drains it, running each envelope's `drop_glue` over its
payload and then freeing the transport buffer. Because this runs after
`drop(guard)`, draining a large mailbox never holds the scheduler lock.
`koja_rt_kill`'s `mailbox.clear()` is replaced by the same path; its
doc comment (updated in the reclamation change to say mailbox cleanup
is "pending the envelope redesign") becomes accurate.

### Drop glue source

`drop_glue` is a per-message-type function emitted by codegen. Each
`send` site would stamp the message type's drop-glue pointer into the
envelope (null when the message is copy-only / owns no heap, which the
type checker already knows). The runtime never introspects the
payload; it only calls the function the compiler supplied. This is the
piece that makes discard sound without the runtime knowing Koja types.

Note (deferred): this codegen does **not** exist yet. The current drop
pipeline frees only leaf heap locals (`String`/`Binary`/`Bits`) at
function exit; it has no per-type destructor that recurses through a
composite's heap-owning fields. Emitting `drop_glue` therefore means
building that recursive drop-glue subsystem first — a general language
need, split into its own effort (see phase 6).

### Allocator unification

Two defensible end states:

1. **Envelopes on the Koja allocator.** Allocate transport buffers via
   `__koja_alloc` (the `malloc` + `payload-8` header path) so there is
   one heap and one free path. The tag header would move into / coexist
   with the length header. Pro: one allocator. Con: the `payload-8`
   header convention is tuned for `String` / `Binary`, not arbitrary
   tagged buffers.
2. **Envelopes stay on the global allocator, with `len` carried.** Keep
   `alloc::alloc`, but now every buffer knows its `len` (via
   `Envelope`), so the global-allocator `Layout` is always
   reconstructable. Pro: no churn to the Koja heap; the split is
   explicit and type-enforced (`Envelope` owns global memory, Koja
   values own `malloc` memory). Con: two allocators remain, but the
   ownership is no longer ambiguous.

This doc recommends **option 2**: it resolves the _ambiguity_ (the
actual bug) without reworking the Koja heap, and `Envelope` makes the
boundary a type, not a convention.

### Timers

Two fixes, both small once `Envelope` exists:

- **Don't deliver to the dead.** The worker-loop timer-fire path checks
  `idx < len`; add `&& state != Dead`. A timer that loses the race
  frees its staged buffer instead of pushing onto a dead mailbox.
- **Cancel on death.** `take_resources` (or a sibling step under the
  lock) scans `guard.timers` for `target_pid == dead_pid`, removes
  them, and hands their `msg_buf` to `Reclaim` for freeing. This is the
  one cross-process step, so it stays under the lock (timer staging
  buffers are small and few).

## Implementation phases

Each phase compiles, passes the suite, and reverts cleanly.

1. **Wire SSOT.** Introduce the shared wire module; make
   `TAG_HEADER_SIZE`, `ENVELOPE_PAYLOAD_OFFSET`, and
   `ReceiveTag::wire_byte` re-export it. No behavior change; pure
   consolidation. Pinned by the existing `lower_process` tests.
2. **`Envelope` type + `VecDeque<Envelope>`.** Convert the mailbox and
   all `push_back` / `push_front` / `pop_front` sites (`koja_rt_send`,
   `send_lifecycle_to`, `send_io_event`, timer fire, both receive fns).
   Carry `len`; `drop_glue` is null for now. No leak fix yet — just the
   shape. Stress-test for no regression.
3. **Discard path.** Extend `Reclaim` with the mailbox; drain + free in
   `Reclaim::free`; route `koja_rt_kill` through it; fix the kill doc.
   Leak check: kill/spawn loop with non-empty mailboxes stays bounded.
4. **Timer cancellation.** Dead-target guard on fire + cancel-on-death.
   Tested with a delayed-send-to-killed-process fixture.
5. **Receive ownership transfer.** _(done)_ The `koja_rt_receive` /
   `koja_rt_receive_timeout` ABI is now slot-out: `i64 receive(out,
out_cap[, timeout])` copies the payload (header stripped) into the
   receiver's slot clamped to `out_cap`, frees the transport buffer via
   `Envelope::free_transport`, and returns the wire tag (`-1` on
   no-message/timeout). Both delivered-receive sites adopt it —
   `emit_receive` (`run`-loop dispatch, branching on the returned tag)
   and `emit_call` (inline reply `R`, `tag == -1` is the timeout). This
   removed the delivered-envelope leak and the raw pointer from the ABI;
   `ENVELOPE_PAYLOAD_OFFSET` is gone from codegen (the header offset is
   now purely a runtime concern). Pinned by the `lang_process_*` golden
   suite and a request/reply RSS leak check.
6. **Drop glue.** _(split into its own effort.)_ Closing the
   nested-heap leak for **discarded** messages needs a per-message-type
   drop-glue function. The premise that "the same drop-glue codegen
   already needs for owned struct fields" exists is **false**: today
   only leaf `String`/`Binary`/`Bits` are freed (via `DropLocal`), and
   nested heap inside structs/enums leaks language-wide. So this phase
   is really "build a recursive composite drop-glue subsystem" (a new
   per-monomorphized-type destructor that recurses struct/enum fields),
   which the language needs generally, not just for messages — tracked
   separately. Until then, `Envelope.drop_glue` stays `null`: discarded
   message payloads still leak their nested heap, but the transport
   buffer (phases 1-5) is reclaimed.

Phases 1-4 are pure runtime and landed independently. 5 was the codegen
crux (both receive sites) and is done. 6 is deferred to a dedicated
recursive-drop effort.

## Mechanical checks

- `rg "push_back|push_front|pop_front" koja/crates/koja-runtime/src/scheduler.rs`
  returns only `Envelope`-typed sites; no bare `*mut u8` mailbox ops.
- The header size has one authoritative definition (`TAG_HEADER_SIZE`
  in `koja-runtime/src/wire.rs`); the backend's `ENVELOPE_PAYLOAD_OFFSET`
  is a conforming local constant whose doc points back at the ABI.
- A spawn/kill loop over processes with queued, undelivered messages
  holds RSS bounded (same harness as the stack leak check in the
  reclamation change).
- A request/reply server processing N messages holds RSS bounded
  (delivered-envelope path).
- Neither `koja_rt_receive` nor `koja_rt_receive_timeout` returns a raw
  envelope pointer; grep the intrinsic decls in `emit/process.rs` for
  the slot-out form, including the `emit_call` reply path.
- Every `tests/lang/process_*` and `lang_io` golden still matches;
  `multi_process` stress (~3000 runs) shows zero crashes.
- A delayed send to a killed process leaks nothing (`/usr/bin/time -l`
  on a fixture that schedules timers then kills the target).

## Out of scope

- **Generational PIDs / slot reuse.** The `Dead` `Process` stays in the
  `Vec` to preserve the `pid - 1` index invariant; reclaiming the slot
  is Phase 5 B1 scheduler work, not envelope lifecycle.
- **Bounded mailboxes / backpressure.** Unbounded `VecDeque` is
  unchanged here; flow control is a separate concern.
- **Eval-backend mailbox.** Eval carries typed `Value` envelopes
  (`EVAL-PROCESS.md`), not byte buffers, so most of this doc is
  LLVM-runtime-only. The one shared piece is the tag taxonomy: eval's
  `EvalEnvelope` variants must stay in lockstep with the wire tags in
  `koja-runtime/src/wire.rs`.
- **Wire stability across versions.** The envelope format is internal;
  nothing observes it across a process or version boundary, so no
  compatibility guarantee is promised.

## Cross-references

- `koja/crates/koja-runtime/src/scheduler.rs` — mailbox, `send` sites,
  receive fns, timer fire, and the `Reclaim` path introduced by the
  resource-reclamation change this doc extends.
- `koja/crates/koja-ir-llvm/src/emit/process.rs` — `emit_receive`,
  `dispatch_arms`, `deserialize_payload_into_local`,
  `ENVELOPE_PAYLOAD_OFFSET`; the `run`-loop receive site phase 5 touches.
- `koja/crates/koja-ir-llvm/src/intrinsics/process.rs` — `emit_call`
  (inline reply receive), `emit_reply_to` (`ReplyTo.send`), `emit_cast`;
  the request/reply send + receive sites that prove replies are
  `Business` traffic and that phase 5's slot-out change must also cover.
- `koja/crates/koja-ir/src/function.rs` — `ReceiveTag` / `wire_byte`,
  the IR-side tag definition folded into the wire SSOT.
- `koja/design/EVAL-PROCESS.md` — the eval scheduler's typed-`Value`
  mailbox and `EvalEnvelope`; the tag taxonomy must stay aligned.
- `koja/design/ROADMAP.md` — Phase 5 B1 scheduler hardening, which owns
  PID reuse and the pluggable scheduler protocol this doc stays clear
  of.
