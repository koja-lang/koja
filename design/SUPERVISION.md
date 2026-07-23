# Supervision Foundation

Koja's supervision foundation is the set of process reliability primitives
that has already shipped. It provides crash containment, typed failure
observation, deterministic runtime process-tree teardown, and graceful
application shutdown. It does not prescribe a universal `Supervisor`
abstraction.

The user-facing process API lives in [LANGUAGE.md](../LANGUAGE.md). Scheduler
invariants live in [SCHEDULER-PROTOCOL.md](SCHEDULER-PROTOCOL.md), and wire
layouts live in [ABI.md](ABI.md).

## Process identity and contracts

`Ref<M, R>` is a typed handle for business messages and replies. `Pid` is the
type-erased process identity used for lifecycle relationships and observation.
Converting a `Ref` to its `Pid` preserves identity without retaining its message
contract.

A process implements `Process<C, M, R>`.

- `C` is the startup config.
- `M` is the accepted business message type.
- `R` is the synchronous reply type.

Each process runs one message handler at a time. Its state is an ordinary value
returned through `Process.Step.Continue`. Returning `Process.Step.Done`
terminates it intentionally.

## Calls and replies

`Ref.call` creates a correlation token and waits for one matching reply until
its deadline. A timeout and a dead target are distinct
`Process.CallError` variants.

`ReplyTo<R>` is a typed one-shot capability. Sending through it reports
`ReplyTo.Delivery.Delivered` or `ReplyTo.Delivery.Expired`. The result is
advisory. A stale reply cannot satisfy a later call because token validation
and delivery are one linearizable operation.

Replies use a dedicated mailbox slot and never surface through `receive`.

## Crash containment

A panic in user code terminates only the crashing process. Other processes
continue running.

`Process.ExitReason.Crashed` carries `Process.CrashInfo` with the panic message
and backtrace. Native execution captures both fields. Eval currently records
the message with an empty backtrace. A crash in the entry process terminates
the OS process with a nonzero status.

Native unwind tables carry a user panic to the process trampoline, but compiled
frames do not have cleanup landing pads. Runtime-owned resources such as the
mailbox, spawn config, and process stack are reclaimed. Managed allocations
referenced only by active frames are not guaranteed to be released on panic.
The same limitation applies when a process is force-killed. See
[MEMORY-MODEL.md](MEMORY-MODEL.md#failure-and-forced-termination).

This matters for restart policy. A native process that repeatedly crashes with
live managed frame values can grow memory even though its stack and mailbox are
reclaimed. Supervision libraries must not describe restart loops as
memory-neutral until this gap is closed.

Panics originating inside runtime implementation code are not user failures.
They remain fatal because continuing after a violated scheduler or memory
invariant is unsafe.

## Monitors

`Process.monitor(pid)` registers the calling process as a watcher and returns a
unique `Process.MonitorRef`. Each registration is independent.

When the target dies, the watcher receives:

```koja
Process.ExitSignal{
  pid: target,
  reason: reason,
}
```

The watcher message type must include `Process.ExitSignal`. Type checking
enforces that requirement at the monitor call site.

Monitoring an already-dead process delivers immediately. `Process.demonitor`
cancels one registration and is a no-op after that monitor fired or was already
removed.

Death records monitor notifications while the registry mutex is held, then
delivers them after releasing it. This prevents monitor chains from
recursively acquiring the lock.

## Parenting

Every spawned process records its spawner as parent. The entry process is the
only process without one. `Process.parent()` exposes that relationship to the
child.

A parent death force-kills all live children regardless of the parent's exit
reason. The cascade is transitive and applies to normal return, intentional
shutdown, panic, and explicit kill.

The cascade deterministically tears down runtime process state and queued
messages. Force-killed native frames do not run user cleanup or managed local
drop glue.

There is no detached spawn. Work that must outlive its creator must be spawned
by a process whose own lifetime includes that work.

Universal parenting provides a deterministic ownership tree even before a
higher-level supervision library exists.

## Lifecycle and drain

Platform lifecycle events are translated into `Process.Lifecycle` messages for
the entry process. The default process handler stops on shutdown and interrupt
and continues on reload.

On graceful termination, the runtime enters drain mode, refuses new spawns,
delivers shutdown, and arms a grace deadline. The process tree may finish
in-flight work during that window. Remaining processes are force-killed when
the deadline expires.

Drain policy lives in the platform-neutral process table so native and
cooperative adapters share the same observable lifecycle semantics.

## Backend parity

The LLVM runtime and interpreter use the same process-table policy for:

- process identity and generations
- calls, reply tokens, and timeout correlation
- monitors and exit delivery
- parent relationships and kill cascades
- lifecycle signals and graceful drain
- process-visible payload ownership and mailbox cleanup

Representation differs by backend, but the sealed program and runtime protocol
preserve the same process-control behavior. Allocation mechanics and crash
backtrace detail differ as described above.

## Supervision policy

The shipped primitives are sufficient to build supervisors, registries, pools,
and restart policies as libraries. Koja intentionally does not commit to a
global registry, `ChildSpec`, restart strategy enum, or universal `Supervisor`
protocol yet.

The ecosystem work in [ROADMAP.md](ROADMAP.md#ecosystem-validation) will
exercise HTTP servers, persistent connections, telemetry, pools, and
registry-style discovery. Repeated patterns from those systems should define
the eventual supervision API. Until then, prescriptive OTP-shaped APIs remain
historical design exploration in
[archive/20260722-SUPERVISION.md](archive/20260722-SUPERVISION.md).

## Invariants

- A user crash cannot unwind through another process.
- PID reuse cannot make a stale handle target a new generation.
- Monitor delivery never occurs while holding the target's process-state lock.
- A reply is delivered only to the matching in-flight call.
- A process cannot outlive its parent.
- Forced teardown reclaims queued payloads without running user handlers.
- Native and eval preserve the same process-control outcomes.
