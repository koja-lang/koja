# Supervision

> Archived on 2026-07-22. This snapshot combines the landed process
> reliability foundation with a prescriptive registry and supervisor design.
> See the current [supervision foundation](../SUPERVISION.md).

The destination design developed for the original Phase 5 supervision
milestone: crash isolation, failure detection (monitors), guaranteed
teardown, a global name registry, and an OTP-shaped `Supervisor`. Crash
isolation, monitors, parenting, and kill cascades have landed. Registry
and `Supervisor` remain proposals. The
[current roadmap](ROADMAP.md#ecosystem-validation) requires ecosystem
validation before Koja commits to a universal supervision protocol.

This is a destination doc, not a trajectory. Every claim reduces to a
behavior in `koja-runtime-core/src/process_table.rs`, the POSIX adapter in
`koja-runtime-posix/src/{scheduler.rs,panic.rs}`, the eval scheduler in
`koja-ir-eval/src/scheduler.rs`, or the stdlib in `lib/global/src/process.koja`.
Where it proposes a change, it names the seam it fills.

It supersedes the supervision, discovery, and `child_spec` sections of
[archive/20260323-CONCURRENCY.md](archive/20260323-CONCURRENCY.md); where the
two disagree, this doc wins.

## The shape in one paragraph

A process is a service with a published contract `Process<C, M, R>`: `C` is
its startup contract, `M` its inbox contract (the messages it accepts), `R`
its reply contract for synchronous calls. A `Ref<M, R>` is a typed client
for that contract; a `Process.Identifier` is the contract-less, type-erased
identity used to monitor, register, and kill. Three orthogonal mechanisms make a process
tree resilient: a **crash** unwinds one process into `ExitReason.Crashed`
without touching the runtime; a **monitor** delivers a typed `ExitSignal`
into a watcher's inbox so it can react; and a runtime **kill-cascade**
guarantees a parent's children die with it. The stdlib `Supervisor`
composes monitors (to decide) and the cascade (to contain) into OTP's
restart strategies.

## Type namespacing (depends on nested type names)

The types this doc introduces are namespaced under the type that owns them,
completing Koja's existing "types are namespaces" rule — already true for
static functions and FFI externs — for _nested type declarations_:

- **`Process.Identifier`** (the type-erased process id; working-named `Pid`
  in earlier drafts), `Process.ExitSignal`, `Process.MonitorRef`,
  `Process.ExitReason`, `Process.CrashInfo`, `Process.RegisterError`.
- `Supervisor.ChildSpec`, `Supervisor.RestartType`, `Supervisor.Shutdown`,
  `Supervisor.Strategy`.

This requires the **nested type names** language feature — now implemented
(design archived in
[archive/20260630-NESTED-TYPES.md](archive/20260630-NESTED-TYPES.md)). The
runtime work (P1 crash-unwind) is independent and proceeds in parallel. Field and variable
names stay short (`pid`, not `identifier`); a per-file `alias
Process.Identifier` keeps use sites terse. For brevity the sections below
use the bare leaf name (`ExitSignal`, `ChildSpec`, …) — read them as the
namespaced form above.

## Non-goals

- **Distribution / clustering.** Pids and names are node-local. No remote
  supervision, no global (cross-node) registry.
- **User-facing links / `trap_exit`.** Death _notification_ is monitors;
  death _containment_ is the runtime kill-cascade. The two jobs OTP links
  conflate are split, and neither is exposed as a bidirectional, surprise-
  propagating link. See [Why not links](#why-not-links).
- **Message adapters.** No private `R -> M` translation shims. Processes
  agree on contracts; they do not adapt each other's payloads. See
  [The two reply patterns](#the-two-reply-patterns).
- **Detached / orphan spawns.** There is no `spawn detached` and no
  independent-spawn form: lifetime is controlled solely by _parent choice_
  (who runs the `spawn`). See
  [Lifetime is controlled by parent choice](#lifetime-is-controlled-by-parent-choice--no-detach).
- **The `Registry` stdlib process and `shared_map`.** A3 follow-ons built
  _on top of_ the primitives here; out of scope.
- **Hot code reload.** No.
- **Behavior changes to the existing scheduler.** Supervision adds to the
  `notify_exit` seam; it does not redesign scheduling. The A1 oracles
  (`koja_rt_sched_violations`, `tests/lang/memory/`, `just tsan`) stay green.

---

## 1. Process contracts and the reply model

Everything below rests on one framing: **a process is a service, and
`C` / `M` / `R` are its published contract.** This is already the shape of
`Process<C, M, R>` in [process.koja](../lib/global/src/process.koja); this
doc makes the framing load-bearing.

- **`M` is the inbox contract** — the set of message "endpoints" the
  process accepts. Mirrors a service's request schema.
- **`R` is the reply contract** for synchronous calls — the response
  schema.
- **`C` is the startup contract** — what you hand `start` to construct it.
- **`Ref<M, R>` is a typed client** for that contract. **`Pid` is the
  type-erased identity** — a handle you can name, monitor, and kill but
  cannot send to (it carries no contract). `Ref<M, R>` widens to `Pid`
  with an explicit helper (`ref.pid()`); there is no generic variance and
  no implicit coercion.

### The two reply patterns

A reply reaches a caller one of exactly two ways, and they have different
contract obligations:

1. **Synchronous `call` = request/response.** The reply rides the call
   itself and is consumed _inline_ by a one-shot, token-keyed, timed
   selective receive. It never enters `handle`. So request/response needs
   **no inbox admission** — `R` is simply the response type in the
   callee's contract. This is the common case and the reason `R` exists.

2. **Asynchronous reply = a published callback.** If a caller wants to be
   called back later, it must _publish a callback endpoint_: the reply
   message type must be a member of its own `M`. This is not a translation
   of the callee's output into the caller's vocabulary — it is the caller
   stating, in its own contract, that it accepts this inbound message.

This yields the single rule the rest of the doc reuses:

> **To receive something, it must be in your `M` contract.**

`ExitSignal` (monitors), async callbacks, and a future `IOReady` are all
instances of it. Monitors are not special — subscribing to exit signals is
just publishing that endpoint.

### Why not adapters

An adapter (`fn (R) -> M`, Akka-style) is a private shim that re-labels a
foreign type as one of yours. It has no analog in the service-contract
model — real services agree on schemas, they do not rewrite each other's
payloads — so we reject it. The problem adapters solve (a parent talking to
several children that reply with the same `R`) is solved the way services
solve it:

- use **synchronous `call`** (the reply is inline; there is no ambiguity), or
- publish **distinct callback endpoints** (distinct `M` variants), or carry
  a **correlation id** in the callback message.

### `ReplyTo<R>` is a one-shot, expiring capability

[`ReplyTo<R>`](../lib/global/src/process.koja) already carries `{id, token}`
and the mailbox already discards stale replies by token correlation
(RUNTIME-GAPS / message-lifecycle work). This doc makes the expiry visible:

```koja
extend ReplyTo<R>
  fn send(self, reply: R) -> Result<(), ReplyError>
end

enum ReplyError
  Expired   # the caller already returned from `call` (timeout or done)
end
```

`send` performs a **linearizable check-and-deliver under the scheduler
lock**: `Ok(())` if the caller is still parked on a reply slot expecting
this token, `Err(ReplyError.Expired)` if it has moved on. The result is
**advisory** — most handlers ignore it; a handler doing expensive
post-reply work can short-circuit. There is no race: the lock serializes
the timeout transition against the delivery, so the reply either lands just
before the caller gives up or is reported expired just after.

> Migration note: this changes `ReplyTo.send` from `()` to
> `Result<(), ReplyError>`. The convenience `ReplyTo.reply(from, value)`
> helper keeps swallowing the result (fire-and-forget ergonomics).

---

## 2. Crash model

Today a panic — user-level (`panic()`, `unwrap` on `None`) or internal —
prints a diagnostic and `std::process::abort()`s the whole OS process
([panic.rs](../crates/koja-runtime-posix/src/panic.rs)), a deliberate choice
so no unwind crosses the C-ABI or poisons the `SCHED` lock. Supervision
requires the opposite for _user_ faults: a crash must take down **one
process**, not the runtime.

### Decision: per-process unwind, split by panic origin

`panic.rs` already distinguishes `PanicOrigin::{User, Runtime}`. We make
that distinction load-bearing:

- **`PanicOrigin::User`** (compiled `koja_panic_backtrace`, `unwrap`/`expect`
  on stdlib sum types) **unwinds the crashing process only.** The unwind
  runs up the process's own stack to a `catch_unwind` boundary installed at
  [`process_trampoline`](../crates/koja-runtime-posix/src/scheduler.rs); the
  trampoline records `ExitReason.Crashed`, runs the process's drop glue,
  frees its stack, and marks it `Dead` — the same death path a normal
  return takes, just with a non-`Normal` reason.
- **`PanicOrigin::Runtime`** (poisoned lock, allocator failure, a bug in
  `koja_rt_*`) **still aborts.** These are not recoverable faults; the
  fail-fast guarantee is unchanged for them.

The unwind is sound because the **release-before-suspend invariant**
([SCHEDULER-PROTOCOL.md](SCHEDULER-PROTOCOL.md)) already guarantees no
process runs user code while holding the `SCHED` lock — so a user-origin
unwind never crosses a held lock. Process bodies must therefore compile
with unwinding enabled and emit unwind tables for the asm process stacks
([arch/*.s](../crates/koja-runtime-posix/src/arch/)); the `catch_unwind`
sits at the trampoline frame the stack was initialized to land in.

### Crash visibility: always print, PID 1 exits

Once a crash is process-local, an _unsupervised_ crash no longer aborts.
That risks a silent failure, which fights "errors are a feature." So:

- **Every crash always prints** the existing Elixir-style backtrace to
  stderr. `abort_with_diagnostic` is split into a `render_diagnostic` (the
  capture + stderr print, kept verbatim) and the terminal `abort()`; user
  crashes call only the former.
- **The runtime keeps running while PID 1 (the entry process) is alive.**
  A crash in any other process is contained.
- **PID 1 crashing prints and exits the OS process non-zero.** This is the
  existing "PID 1 death ⇒ shutdown" rule with an exit code derived from the
  reason. It also makes [root-supervisor crash ⇒ OS exit](#7-application-startup-root-supervisor-and-exit-codes)
  fall out for free.

Per-crash log-level control (quieting routine supervised restarts)
belongs to the ecosystem validation work around structured logging.
Until then, a crash loop is loud, which is acceptable and arguably
desirable.

### `ExitReason.Crashed` carries a structured capture

`panic.rs`'s `Frame` struct already anticipates "a future Sentry /
OpenTelemetry reporter can consume the same frames." So the crash reason
carries structure, not just a message:

```koja
enum ExitReason
  Normal
  Shutdown
  Crashed(CrashInfo)
end

struct CrashInfo
  message: String
  backtrace: String   # pre-rendered frames; structured Frame list is a post-1.0 refinement
end
```

The capture happens at the unwind site (the only place the stack is live);
the rendered backtrace travels in the `ExitSignal` so supervisors and a
future observability hook consume what the human renderer prints.

> This widens the existing `ExitReason.Crashed(String)` to
> `Crashed(CrashInfo)`. The wire/`ExitReason::from_index` code
> (`process_table.rs`) keeps `3 = Crashed`; the payload is carried
> alongside, not in the discriminant.

---

## 3. Monitors and `ExitSignal`

Death _notification_ is monitors, and only monitors (user-facing). A
monitor is unidirectional, stackable, and yields a token for cancellation —
strictly better than links for the "tell me when X dies" job.

### Types

```koja
struct Pid
  id: Int
end

struct ExitSignal
  pid: Pid
  reason: ExitReason
end

struct MonitorRef
  token: Int
end
```

`ExitSignal` carries a `Pid` (not a `Ref<M, R>`) so a watcher monitoring
heterogeneous children needs no knowledge of their contracts.

### API

```koja
extend Process
  # M of the calling process must include ExitSignal (see below).
  fn monitor(target: Pid) -> MonitorRef
  fn demonitor(reference: MonitorRef)
end
```

`monitor` / `demonitor` are static (the behavior is uniform — there is
nothing per-process to customize). `monitor` accepts a `Pid`, so any
`Ref<M, R>` is monitored via `ref.pid()`.

### The contract rule, enforced

At a `Process.monitor` site the type checker verifies the **current
process's `M` includes `ExitSignal`** (an instance of the section-1 rule).
If it does not, compile error with a hint to add `ExitSignal` to the
process's message union. Match exhaustiveness then forces the process to
handle it. This mirrors the archived decision and the async-callback rule —
one rule, three uses.

### Delivery: off-lock, deadlock-free

`notify_exit(pid, reason)` already fires from `ProcessTable::transition` on
every `* -> Dead` edge, _with the `SCHED` lock held_
([process_table.rs](../crates/koja-runtime-core/src/process_table.rs)).
Delivering an `ExitSignal` means enqueuing a message onto each monitor's
mailbox and possibly waking it — which must **not** happen under the lock,
or a monitor-of-a-monitor chain could deadlock.

So `notify_exit` only **stages** the work: it looks up the dying pid's
monitors (a `pid -> [MonitorRef + watcher pid]` map in the core) and pushes
synthesized `ExitSignal` envelopes onto the existing post-lock delivery
path — the same `Reclaim`-style "drop/deliver after `drop(guard)`"
discipline the message-lifecycle work established. Delivery then runs
exactly like a normal `cast` to each watcher.

The runtime must build the `ExitSignal` Koja value in wire form, so its
layout (and `ExitReason` / `CrashInfo`) is pinned in
[wire.rs](../crates/koja-runtime-core/src/wire.rs) / ABI, like every other
runtime-constructed value.

> Open (resolve in build): batched vs streaming delivery when one death has
> many monitors. Default: one `ExitSignal` per monitor, staged together,
> delivered individually — simplest and matches "one cast per watcher."

---

## 4. Universal parenting and the kill-cascade

Every process has exactly one parent, and the **process tree is a strict,
total ownership tree** — fractal with the value-ownership memory model:
values are owned and deterministically reclaimed at scope exit; processes
are owned by their spawner and deterministically torn down at parent death.

### One parent, set by who spawns

- **Every `spawn` parents the new process to the spawner**, recorded
  atomically at creation. There is one root — PID 1, the entry process —
  whose parent is `None`. Every other process has a parent.
- `Process.parent() -> Option<Pid>` exposes it (`None` only for PID 1). The
  core keeps a `parent` pointer on each PCB plus a `parent -> [child pid]`
  reverse index for the cascade.

```koja
extend Process
  fn parent() -> Option<Pid>
end
```

> Naming: `spawn` becomes a `Process.spawn` intrinsic alongside `monitor`,
> `parent`, `register`, etc. — spawning is a process operation (it parents
> the child to the caller), so it belongs on `Process` rather than being a
> free-standing keyword. The `spawn Type.start(config)` lowering is unchanged.

### The cascade is universal and unconditional

- When a process goes `Dead` for **any** reason — return, crash, or brutal
  `kill` — `notify_exit` force-kills its children (staged off-lock, then
  `ProcessTable::kill` each, which already records `ExitReason.Killed` and
  reclaims via the existing path).
- It is **transitive**: a killed child's children cascade in turn, so an
  entire subtree tears down from any node's death.

"No orphans" is therefore a **language-wide guarantee by construction**, not
a supervisor feature. Supervisors need no special spawn — they just `spawn`
(cascade is automatic) and add monitors for the _restart_ decision.

### Lifetime is controlled by parent choice — no detach

There is deliberately **no `spawn detached` and no `spawn_child`**. The only
knob on a process's lifetime is _who its parent is_, and a process's parent
is whoever ran its `spawn`. To make work that must outlive you, you do not
detach it — you get a **longer-lived process to spawn it**, so that process
owns it. Ownership is thus unspoofable (you cannot make something a child of
`Y` without `Y` executing the spawn) and every long-lived process is
necessarily visible under an owner in the tree — there is no back door.

The OTP idiom falls out directly: outliving, fire-and-forget work is routed
through a long-lived host (a `Task.Supervisor`-style process that runs the
spawn on your behalf). `Task.async(fn) |> await` is unaffected — the task is
your child and you await it — but a _never-awaited_ `Task.async` now dies
with its spawner; to outlive it, hand it to a task host. (A stdlib task-host
process is a small follow-on, not part of the core model.)

### Why not links

An OTP link bundles two orthogonal concerns: (1) death notification and (2)
fate-sharing / teardown. Koja splits them — **monitors** do (1), better; the
**universal parent cascade** does (2), enforced by the runtime regardless of
how the parent died. Peer-to-peer fate-sharing between arbitrary processes
(without one owning the other) is intentionally unsupported: the Koja answer
is "put them under a common `OneForAll` supervisor," which is more legible.

---

## 5. Global name registry

The runtime-level half of A3, here because it needs `Pid` and the
`notify_exit` death path.

```koja
enum RegisterError
  NameTaken   # the name already maps to another live pid
  NotAlive    # target is dead/stale
end

extend Process
  fn register(target: Pid, name: String) -> Result<(), RegisterError>
  fn unregister(name: String)
  fn whereis<M, R>(name: String) -> Option<Ref<M, R>>
end
```

- Runtime-backed `name -> Pid` map. **Names are unique** (one name maps to
  at most one pid), but **a pid may hold multiple names** — more flexible
  than Erlang's one-name-per-pid restriction and free to allow.
- **`register` returns `Result`**, not `Bool`: `NameTaken` distinguishes a
  collision from `NotAlive` (registering a dead pid), and leaves room for
  future named failures. `whereis` stays `Option` — absence is not an error.
- **Keys are `String` here, by design.** The runtime natively handles
  `String`; an arbitrary key type would force the runtime to run Koja's
  `Hash`/`Equality` glue on arbitrary heap values and copy the key in
  (shared_map-style) — heavyweight machinery for what is meant to be a
  _well-known singletons_ facility (db pool, metrics collector). Arbitrary
  and typed keys are the [stdlib `Registry`](#out-of-scope-a3)'s job (A3):
  a pure-Koja process holding a `Map<K, V>` keys on any `K: Hash + Equality`
  for free, and a `ServiceKey<M, R>` value can carry the contract type to
  make lookup _sound_ (no turbofish) — the A3 hook, not built here.
- **`whereis<M, R>` reconstructs a typed `Ref` from the stored `Pid` plus
  the caller's turbofish.** This is unsound in the same deliberate way as
  the fixed-`R` reply contract — there is no RTTI after monomorphization,
  so the runtime cannot verify the named process actually speaks `M`. It is
  a _gentleman's agreement_, exactly like a typed client over a REST
  endpoint. The `Option` return is the safety the type system _does_
  enforce (absence is explicit). The sound alternative is the stdlib
  `ServiceKey` above.
- **Auto-eviction:** `notify_exit` drops every name bound to the dying
  `pid` (off-lock, same staging as monitor delivery), so a `whereis` after
  death returns `None` rather than a stale handle.

<a id="out-of-scope-a3"></a>
Out of scope (A3 follow-ons that build on this): the typed, dynamic,
scoped `Registry` stdlib process (arbitrary `Hash + Equality` keys + sound
`ServiceKey<M, R>` lookup), and `shared_map`.

---

## 6. Supervisor

A stdlib process, not a language keyword. It composes **monitors** (to
decide) and the **kill-cascade** (to contain) into OTP's restart model.

### Child specs

The `Process` protocol gains a third default method (alongside `run`,
`handle_signal`, `priority`) that produces a uniform, **type-erased** spec:

```koja
enum RestartType
  Permanent   # always restart
  Transient   # restart only on abnormal exit (Crashed / external Killed)
  Temporary   # never restart
end

struct ChildSpec
  id: String            # stable identity within a supervisor (reports + matching)
  start: fn () -> Pid   # spawns the child as a child of the caller, returns its pid
  restart: RestartType
end

protocol Process<C, M, R>
  # ...existing...
  fn child_spec(config: C) -> ChildSpec
    ChildSpec{
      id: Self.type_name,                  # compiler-populated; see "Identity"
      start: fn () -> Pid
        ref = spawn Self.start(config)     # plain spawn; parented to the caller (§4)
        ref.pid()
      end,
      restart: RestartType.Permanent
    }
  end
end
```

**Type erasure via the closure.** `child_spec` is monomorphized per
implementing type, so each `Type.child_spec(cfg)` bakes the concrete
`C` / `M` / `R` into a closure whose _type_ is the erased `fn () -> Pid`. A
`List<ChildSpec>` is therefore homogeneous even though its children have
different contracts — the closure is the eraser, and it is the only thing
that _can_ spawn the child (the supervisor never sees the type). The
captured `config` is an independent value (value semantics), so the closure
is re-callable for every restart and yields fresh state each time — no
`copy` keyword.

**Identity.** `id` is a stable per-supervisor name used in restart reports
and to match an incoming `ExitSignal.pid` back to its spec across restarts
(the pid changes on restart; the `id` does not). The default is the
implementing type's name, populated by codegen from the **same type-name
string the auto-derived `Debug` already emits** — not a new user-facing
reflection feature. Override it for multiple children of the same type; ids
must be unique within a supervisor.

> Naming: the struct is `ChildSpec`; the _producer_ is the protocol default
> `child_spec`. (Resolves the name-vs-method conflict the archive flagged.)

**Spawn ownership is automatic (§4).** The closure uses a plain `spawn`, and
the supervisor always invokes that closure **from its own context** — at
initial start and on every restart — so the new child is parented to the
supervisor every time, with no special spawn form and no adopt race. This is
the whole reason child specs are closures the supervisor _runs_ rather than
data it hands to the runtime: running the spawn is what establishes
ownership.

### Startup failure

`Process.start` returns `Result<Self, StopReason>`, and a body can also
panic during `start`. The two are different signals:

- **`start` returns `Err(StopReason)`** — an _intentional decline_. The
  process exits with that `Normal` / `Shutdown` reason; under `Transient` /
  `Temporary` it is **not** restarted (a clean "don't run" path). Under
  `Permanent` it would restart-loop, so declining only makes sense for
  non-permanent children.
- **`start` panics** — a _failure_. It exits `Crashed` (abnormal), so it is
  restarted under `Permanent` / `Transient`, subject to intensity.

Either way the child has already spawned (the supervisor holds its pid), so
startup failure flows back through the **same `ExitSignal` path** as a
runtime crash — there is no separate init-time channel. A child that cannot
start keeps crashing, intensity trips, and the supervisor propagates upward.

### Strategies, intensity, propagation

```koja
enum SupervisorStrategy
  OneForOne     # restart only the crashed child
  OneForAll     # restart all children when any crashes
  RestForOne    # restart the crashed child and everything started after it
end

struct SupervisorConfig
  children: List<ChildSpec>
  strategy: SupervisorStrategy
  max_restarts: Int   # intensity: max restarts...
  period_ms: Int      # ...within this window
end
```

- `M = SupervisorMsg | ExitSignal` — it publishes the `ExitSignal`
  endpoint (the §3 rule) and monitors every child it spawns.
- It keeps children in an **ordered list** of `{id, spec, current_pid}`
  (order = `RestForOne` order), plus a `pid -> id` index to resolve an
  incoming `ExitSignal`.
- On a non-self-initiated child `ExitSignal`, it consults the child's
  `restart` type and the `strategy` to decide the affected set, kills the
  ones the strategy says must go (the kill-cascade tears down their
  subtrees), and re-runs the relevant `start` closures, updating
  `current_pid`.
- **Restart intensity:** if restarts exceed `max_restarts` within
  `period_ms` (a sliding window of restart timestamps via stdlib `DateTime`),
  the supervisor gives up and returns `Step.Done` with a crash reason — it
  dies, and its own supervisor (or PID 1) handles it. This is how failure
  propagates up the tree.
- **Containment on supervisor death:** because the supervisor ran each
  child's `spawn`, the children are its own (§4), so the universal cascade
  tears the whole subtree down if the supervisor itself dies for any reason.

**Restart classification.** Whether an exit triggers a restart is a function
of the child's `RestartType` and the `ExitReason`:

| `ExitReason`        | `Permanent` | `Transient` | `Temporary` |
| ------------------- | ----------- | ----------- | ----------- |
| `Normal`            | restart     | —           | —           |
| `Shutdown`          | restart     | —           | —           |
| `Crashed`           | restart     | restart     | —           |
| `Killed` (external) | restart     | restart     | —           |

**Self-initiated kills are not failures.** When a strategy (`OneForAll`,
`RestForOne`) kills siblings to restart them, those `ExitSignal`s are the
supervisor's own doing. The supervisor records the pids it is about to kill
and **correlates away** their incoming `ExitSignal`s, so an intentional
teardown never counts against intensity nor triggers a second, recursive
restart. Only deaths the supervisor did not initiate drive the restart logic.

**Shutdown order.** Children are stopped in **reverse start order** (LIFO,
OTP semantics) and started in forward order. A per-child graceful-shutdown
timeout is deferred; A2 uses the global A1 drain grace (`KOJA_GRACE_MS`) as
the budget before a child is force-killed.

A supervisor emits a **terse one-line restart report** per action
(`child <id> exited (<reason>), restarting [<n>/<max> in <period>ms]`),
distinct from the crash's full backtrace dump (which the crashing process
already printed, §2).

---

## 7. Application startup, root supervisor, and exit codes

`fn main` is already retired — the entry is a `Process` impl (PID 1). The
supervised application convention:

1. The entry process builds child specs via `Type.child_spec(config)`.
2. It spawns a root `Supervisor` with those specs (the supervisor becomes
   PID 1's child automatically, §4) and **monitors** it (the entry's `M`
   includes `ExitSignal`).
3. On the root supervisor's `ExitSignal`, the entry returns
   `Step.Done(reason)`, mapping the reason to an OS exit code via the
   existing `ExitStatus` protocol.

Because PID 1 returning ⇒ OS exit, and PID 1 crashing ⇒ OS exit (§2), the
ROADMAP requirement "root supervisor crash ⇒ OS process exit" is satisfied
by this monitoring relationship with no special case.

### Interaction with A1 graceful drain

On `SIGTERM`, A1's drain delivers `Shutdown` to PID 1 and arms the grace
deadline. The entry casts `Shutdown` to the root supervisor, which
propagates shutdown **down the tree in reverse start order** (LIFO, OTP
semantics), stopping children gracefully ahead of the grace deadline;
stragglers are force-killed at the deadline by the existing `kill_all`
backstop.

---

## 8. Backend parity (native + eval)

Both backends consume the same sealed IR and must show identical observable
supervision behavior; the golden suite asserts parity-eligible fixtures
under both.

- **`notify_exit` lives in `koja-runtime-core`**, so the monitor map,
  kill-cascade, and name-eviction logic are shared by both adapters.
- **Native** ([scheduler.rs](../crates/koja-runtime-posix/src/scheduler.rs)):
  user-origin unwind via `catch_unwind` at `process_trampoline`; off-lock
  staged delivery as described.
- **Eval** ([koja-ir-eval/src/scheduler.rs](../crates/koja-ir-eval/src/scheduler.rs)):
  crash isolation via `catch_unwind` around the per-process step in the
  cooperative driver (a user panic becomes that process's `Crashed` death,
  not a runtime abort); the single-threaded driver does staging/delivery
  inline (no lock to release). Eval carries typed `Value` envelopes, so the
  synthesized `ExitSignal` is a `Value`, not a wire buffer — the tag
  taxonomy stays aligned with `wire.rs`.

---

## 9. Build roadmap (documented, not built here)

Each phase compiles, passes the suite, and is independently revertible.

- **P1 — Crash unwind.** `PanicOrigin::User` unwinds to a `catch_unwind` at
  `process_trampoline`; record `ExitReason.Crashed(CrashInfo)`; run drop
  glue + free stack; split `abort_with_diagnostic` into render + abort;
  enable unwind tables for process bodies. Runtime panics still abort.
- **P2 — `Pid` + `ExitSignal` + monitors.** Stdlib types; `Process.monitor`
  / `demonitor` intrinsics; the `ExitSignal ∈ M` type-check rule; the
  monitor map + off-lock staged delivery in `notify_exit`; `ExitSignal`/
  `CrashInfo` wire layout. `ReplyTo.send -> Result<(), ReplyError>`.
- **P3 — Universal parenting + kill-cascade.** Record the spawner as parent
  on every `spawn` (PCB `parent` pointer + reverse index); `Process.parent`
  intrinsic; transitive force-kill of children on `notify_exit`.
- **P3b — Global name registry.** `register` / `unregister` / `whereis`
  intrinsics; `name -> Pid` map; auto-eviction hooked into `notify_exit`.
- **P4 — `Supervisor` stdlib process.** `ChildSpec` (with `id`, default from
  the `Debug` type-name source), `child_spec` default, the three strategies,
  restart types + classification table, self-initiated-kill correlation,
  restart intensity, reverse-order shutdown, terse restart reports.
- **P5 — App startup + root + eval parity + A1 drain ordering.**

### Done when

A supervised process tree restarts crashed children correctly under each
strategy, on **both** backends; a root-supervisor crash exits the OS
process with the right code; `examples/shortener` runs supervised and
drains on `SIGTERM`.

### Mechanical checks

- A user `panic()` in a non-PID-1 process prints its backtrace and leaves
  the runtime running; the process's supervisor restarts it. PID 1 panic
  exits non-zero.
- A spawn/kill loop over supervised subtrees holds RSS bounded (no orphan
  leak); `koja_rt_sched_violations` stays zero; `just tsan` reports no
  races.
- Every spawned process records its spawner as parent atomically;
  `Process.parent` returns `None` only for PID 1.
- Killing any process (`Ref.kill`, crash, or return) tears down its entire
  subtree transitively (no surviving descendant pids).
- A `Transient` child that returns `StopReason.Normal` is not restarted; one
  that panics is; a `OneForAll` restart does not recursively re-trigger on
  its own sibling kills.
- `whereis` after a registered process dies returns `None`.
- A `Process.monitor` call whose `M` omits `ExitSignal` is a compile error.
- Every parity-eligible supervision fixture matches under both backends.

---

## References

- [ROADMAP.md](ROADMAP.md) — current ecosystem validation strategy.
- [archive/20260722-ROADMAP.md](archive/20260722-ROADMAP.md) — the
  historical Phase 5 A2 supervision and A3 discovery milestones.
- [archive/20260323-CONCURRENCY.md](archive/20260323-CONCURRENCY.md) —
  superseded supervision / discovery / `child_spec` exploration.
- [SCHEDULER-PROTOCOL.md](SCHEDULER-PROTOCOL.md) — the
  release-before-suspend invariant the crash unwind relies on; the
  agnostic-core / adapter seam the shared logic lives in.
- [RUNTIME-GAPS.md](RUNTIME-GAPS.md) — the `notify_exit`, kill-tombstone,
  and off-lock-reclaim machinery this doc extends.
- [ABI.md](ABI.md) — the `koja_rt_*` and `wire.rs` contracts the
  `ExitSignal` / `CrashInfo` layouts join.
- [lib/global/src/process.koja](../lib/global/src/process.koja) — the
  `Process` protocol, `Ref`, `ReplyTo`, `ExitReason`, `StopReason` this doc
  extends.
- [crates/koja-runtime-core/src/process_table.rs](../crates/koja-runtime-core/src/process_table.rs)
  — `notify_exit`, `ExitReason`, `set_exit_reason`, `kill`, `transition`.
- [crates/koja-runtime-posix/src/panic.rs](../crates/koja-runtime-posix/src/panic.rs)
  — `PanicOrigin`, `abort_with_diagnostic`, `capture`, `Frame`.
