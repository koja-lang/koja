# Observability: Context in the Runtime

**Status: in progress (2026-09-28). `Process.context` and the stdlib
`Trace` module have shipped. Log, metrics, and the exporter port are
open.** This document argues a position for how Koja programs produce traces, logs,
and metrics. The runtime carries two typed fields on every process: a
small request context that also rides on every message, and a log
configuration that children inherit at `spawn`. The standard library
owns the APIs that create spans, log records, and metric points.
Exporters such as OpenTelemetry live in packages that never touch user
code. The document is the contract for the observability work in the
0.20 section of [ROADMAP.md](ROADMAP.md).

The first draft (2026-09-23) lived in the remem repository, because
remem was the first Koja service that needed tracing and its vendored
`lib/open_telemetry` and `lib/log` packages were the experiments that
told this design what hurts. Their findings are recorded below.

## Summary

- The runtime owns propagation. A fixed 32-byte context sits on every
  process, copies to children at `spawn`, and rides on every message
  envelope. No user code threads it.
- The runtime owns log configuration the same way. `Log.configure`
  writes the calling process's log slot, `spawn` copies it into the
  child, and `Log.info` reads the current process's slot. No user code
  threads a logger.
- The standard library owns the vocabulary. `Trace`, `Log`, and
  `Metrics` are stdlib modules that libraries call without knowing
  which exporter is installed, the same way `trail` and the HTTP
  client share `HTTP.Request` without knowing about each other.
- Exporters are packages. An `OpenTelemetry` package drains runtime
  queues and speaks OTLP. Stdout and the test harness are other
  exporters. Nothing in the stdlib names OpenTelemetry.
- Developers still name spans, set attributes, and choose where a
  trace starts. The runtime cannot know that `db.query` is a span
  boundary. It can know who asked for the query.
- Explicit wins over bolted-on ambient state. A runtime-native context
  removes the reasons to object to implicit state, because it cannot
  be forgotten and cannot orphan a subtree.
- Two runtime fields and no process dictionary. A third field needs a
  runtime reader to justify it.
- Metrics are designed here for coherence but are not 0.20 work.

## Survey

Every language with a tracing story is in one of two camps.

**Explicit parameter.** Go passes `context.Context` as the first
argument to everything. `tracer.Start(ctx, name)` returns a new `ctx`
that carries the new span, and whatever `ctx` a function receives is
the parent of any span it starts. Go rejected goroutine-local storage
on purpose, so there is no escape hatch, and the ecosystem complied.
The design came out of Google after Dapper, so it was a considered
choice by people who had tracing in the room, not a gap.

**Ambient context.** Everyone else stores the current span in
something scoped to the running unit of execution and looks it up
implicitly. Java uses `ThreadLocal`. .NET uses `Activity.Current` on
`AsyncLocal`. Python uses `contextvars`. Node uses
`AsyncLocalStorage`. Erlang and Elixir use the process dictionary.
Haskell's `hs-opentelemetry` keeps a per-green-thread `Context` by
reaching into GHC internals. Rust keeps a thread-local current span in
`tracing` and a thread-local `Context` in `opentelemetry`. Swift uses
`@TaskLocal`. Kotlin uses a `CoroutineContext` element. Scala effect
systems use `FiberRef` or `IOLocal`. Gleam wraps the Erlang SDK by
FFI, so `use ctx <- span.with(name, [])` reads the process dictionary
underneath.

The ambient camp splits again on one question. When work forks, does
the child inherit the parent's context?

Runtimes that copy it at fork have working propagation with no user
code. .NET `AsyncLocal` flows through `await` and `Task.Run`. Python
`asyncio.create_task` copies the current `contextvars` context. Node
`AsyncLocalStorage` follows promise chains. Swift structured
concurrency copies task-locals into child tasks.

Runtimes that do not copy it require an attach dance at every
boundary. Java threads and executors need `Context.taskWrapping`.
Erlang processes start blank, so the idiom is `Ctx.get_current()`
outside and `Ctx.attach(ctx)` inside every `Task.async`, and the
`opentelemetry_process_propagator` package exists to wrap `Task` for
this. Haskell `forkIO` starts blank, and `hs-opentelemetry` 1.0 ships
`tracedForkIO` and `tracedAsync` to capture and reinstall. Rust
`tokio::spawn` does not inherit, so futures need
`.in_current_span()` or `FutureExt::with_context`, and a thread-local
guard held across `.await` is a bug because the task may resume on a
different worker thread.

The symptom in every non-copying runtime is the same. Spans appear,
but each process forms its own one-span trace, and nobody notices
until they need the trace.

**Code you do not control.** Three levers exist. Java agents, Python
and Node monkeypatching, and .NET `DiagnosticSource` rewrite or hook
libraries at load time, so a library that never heard of tracing gets
spans anyway. Elixir's `:telemetry` has libraries emit neutral events
that a tracer subscribes to, so Ecto never depends on OpenTelemetry.
Go convinces library authors to take `ctx`. Koja has no load-time
rewriting and no monkeypatching, so only the last two are available.

**The forgotten precedent.** Erlang's `seq_trace` from the 1990s is a
token stored on the process that the VM copies onto every message and
installs on the receiver when the message is read. It is runtime
propagation across process boundaries, and it never became the
tracing story because it had no span model, no export, and no
ecosystem. The primitive alone is not the product. The primitive plus
an exporter contract is.

**The precedent for the log slot.** Erlang has no global for where
`io:format` output goes. Each process carries a group leader pid,
`spawn` copies the parent's, and IO follows it. That is a per-process
inherited value owned by the runtime, older than `:logger`, and it is
the shape the log configuration takes here.

## Position

The advantage of Go's required parameter is not that it is explicit.
It is that forgetting is a compile error. Every ambient scheme in the
survey fails silently, and for code written by a model that property
matters more than for code written by a person, because the model
produces a plausible `Task.async` with no attach and no test catches
it.

The cost of explicit is signature churn. Every function on the request
path grows a parameter, every library on the path must accept it, and
the cost lands hardest on packages, whose authors did not choose to be
on anyone's request path. For one service whose `conn`, `trail`,
`pooler`, `postgres-koja`, and HTTP client are all one author the cost
is bearable. For an ecosystem it is not.

A runtime-native context resolves the tension. The objections to
ambient state are fork breakage and silent orphaning. A runtime that
inherits on `spawn` and carries context in the message envelope leaves
no code path that runs without a context, so orphaning cannot happen.
What remains implicit is total, and total implicit state is fine.
`self` is already ambient in Koja. Who a process is working for
belongs in the same category as who the process is.

## Runtime context

The runtime carries one fixed-size value per process and per message.

```
flags       8 bytes  (bit 0: sampled)
span        8 bytes
trace_hi    8 bytes  (trace id bytes 0 to 7)
trace_lo    8 bytes  (trace id bytes 8 to 15)
```

The value is named `Process.context`, not `Process.trace`, so a
deadline can join it later without a rename. It is not a map, it
carries no baggage, and it has no user-visible mutability. Those three
properties are what keep it cheap, see Performance below.

The four words are the runtime layout, and the Koja struct
`Process.Context` declares them in the same order so a read is one
32-byte copy. User code does not touch the words. It reads the ids
through `trace_id()` and `span_id()`, which return them as bytes, and it
never builds a context. `Trace` and `Propagation` are the only
constructors, through a package-private function in `global`. That
rule is what lets the layout grow: a field added to the struct breaks
no code outside the package. A 128-bit integer type would let
`trace_id` become one field, and the byte helper would not change.
`flags` has sixty-three spare bits for future markers.

Rules for the runtime:

- `spawn` copies the parent's context into the child.
- Every `cast`, `call`, and `send_after` copies the sender's context
  onto the message envelope.
- When a business message is dequeued, the runtime installs the
  envelope's context on the receiving process before the handler runs.
  Nothing restores the previous value. The next business message
  installs its own, so a `Pool` serving two requests never crosses
  them, and `Process.run` never observes the slot between messages
  because it tail-recurses inside the receive arm.
- Lifecycle signals, IO readiness, exit signals, and replies leave the
  slot alone. A reply carries no context and the caller keeps its own,
  so a `call` is transparent to the caller's trace. An exit signal
  describes the dead process, not a request, and inherits nothing from
  it.
- `send_after` captures the context at scheduling time, and the timer
  message carries it.
- A process with no root context carries zeros with the sampled bit
  clear, and nothing downstream of it produces records.

The dequeue rule has one consequence for code that writes its own
`receive` loop. The slot changes at each business dequeue, and the
process keeps the last installed value after the arm returns. A
closure that must return to its own context around a `receive` wraps
it in `Trace.span`, which restores what it found. The `Process`
protocol's `run` never needs this.

**Why parent pid is not enough.** The spawn tree is not the causal
tree. In remem, `App` spawns the `Pool` at boot. A request `Worker`
calls the `Pool`, and the database work happens on behalf of the
`Worker`, but the `Pool`'s parent pid is `App`. Ancestry parents the
database span to the boot sequence. Elixir met the same wall.
`$ancestors` existed from the start and is the spawn tree, and
`$callers` was added later because `Task.Supervisor` spawns tasks
whose ancestor is the supervisor and whose cause is the caller. The
envelope field is `$callers` with a fixed size.

For a `call`, the sender is blocked, so the receiver could read the
sender's slot by pid instead of receiving a copy. That fails for
`cast`, where the sender has moved on. The inline copy is simpler and
race-free, and it is the one to ship.

## Performance

The copy is the cheap part. A 32-byte inline copy plus a flag test
adds a few nanoseconds to a mailbox push that already costs 50 to 100
ns for the lock or CAS and the allocation. Erlang's `seq_trace` is one
word compare when no token is set and has never been measured as a
cost.

The version that gets expensive is the one OpenTelemetry specifies.
`Context` in the OTel API is a map of arbitrary keys that carries
baggage. In Erlang that is a term deep-copied on every send. In .NET
it is an `ExecutionContext` object graph that took years of
copy-on-write work to make tolerable. A 32-byte slot signs up for none
of that. Baggage stays a library concept that the rare code which
wants it passes explicitly.

The real costs, in order:

1. Span records. Each finished span allocates a name, two timestamps,
   an attribute list, and a status, and holds them until export. The
   runtime propagates context on every message but creates spans only
   where a library asked. Automatic spans per `call` are opt-in per
   process type. This one decision separates free from ruinous.
2. Sampling. Head sampling at the root. An unsampled request carries
   the flag clear and does nothing else, no id generation, no records,
   no clock reads.
3. Clock reads. Two per span through the vDSO, 20 to 30 ns each, only
   on sampled annotated spans.
4. Id generation. A span id is 8 random bytes. `getrandom` per span
   would be the largest cost in the design. Use a per-scheduler
   xoshiro seeded once from the OS. Trace ids are minted only at a
   root.
5. The export queue. One mutex-protected queue is the hottest lock in
   the runtime under load, and per-scheduler ring buffers that the
   exporter drains in turn avoid it. The first version ships the one
   queue with a drop counter and measures before building the ring
   buffers, the same rule as item 6. Either way the queue is bounded,
   a full queue drops rather than blocks, and the drop counter is
   exported. Tracing must never apply backpressure to the traced
   program.
6. Envelope size. The one permanent cost. If the header is 32 bytes
   today, it becomes 64. An interned context table with a 4-byte index
   in the envelope saves 28 bytes per message at the price of
   refcounting. Inline first, intern only if a benchmark says so.

Budget. Unsampled steady state is one branch and one 32-byte copy per
send, and one load and one store per handler invocation. Sampled with
a handful of annotated spans per request is a few hundred nanoseconds
per span. For comparison the Java agent measures at 1 to 3% CPU with
everything instrumented, and it weaves bytecode and carries map
contexts.

## Trace API in the standard library

The span API lives in the stdlib, not in the exporter package. If
`Trace.span` were an OpenTelemetry package function, `postgres-koja`
would depend on that package to emit a database span, and every
library would couple to one exporter. Elixir solved this with
`:telemetry` events as a neutral middle layer. Koja can skip the event
indirection because the stdlib is already the neutral layer.

```koja
# The boundary that reads an incoming traceparent starts a root.
Trace.root(
  "#{method} #{path}",
  parent,
  Trace.SpanKind.Server,
  fn (span: Trace.Span) -> Response
    response = router.dispatch(request)
    span.attribute("http.response.status_code", response.status.code)
    response
  end,
)

# Anything below reads Process.context and needs no parent argument.
Trace.span(
  "db.query",
  fn (span: Trace.Span) -> Rows
    span.attribute("db.system", "postgresql")
    work(db)
  end,
)

# Leaving the process tree is the one manual step.
headers.set("traceparent", Propagation.format(Trace.current()))
```

`Trace.span` mints a child span id, installs the child context in the
process slot, runs the closure, records the span with start and end,
and restores the parent context. User code never writes the slot
directly. A short closure (`span -> db.query(sql)`) holds one
expression, so a body with an attribute call uses the block form.

`Trace.Span` inside the closure is a handle to a runtime-owned record,
not a value. `span.attribute(...)` is a runtime call, the same way
`pool.checkin(...)` is a call on a process handle. A value would make
that line a discarded copy. The handle is an index into the calling
process's stack of open records, and the runtime moves the record out
and back in around each write. The finished record is a
`Trace.SpanRecord`.

`Trace.root` takes the parent as an argument and ignores the context
the process already carries, except to put it back when the closure
returns. A root with no parent mints a trace id and sets the sampled
bit. A root under a parent keeps the parent's trace id and sampled
bit, so an unsampled upstream stays unsampled and records nothing here
either. Sampling policy beyond "follow the parent" is not designed.

The record crosses into the runtime by the message send convention: a
deep copy moves into the runtime, and the caller's own value keeps its
slot lifecycle. That is three deep copies per recorded span (open,
close, export) plus one per attribute write. It is correct and it
reuses what `Ref.cast` already does. A move-into-runtime ownership
form that skips the copy is a compiler change and waits for a
benchmark that asks for it.

The developer still owns where a trace starts and from what incoming
parent, span names and kinds, attributes and error status, injecting
the context into outgoing headers, and whether background work is a
child or a link. The runtime defaults to parent. A library can ask for
a link when the work outlives the request.

An optional automation fits later. A per-process-type flag turns each
handled `call` into a span named after the message variant, so pools
and registries get spans without library changes. Opt-in, because for
a hot actor it is the difference between cheap and ruinous.

## Exporter contract

The runtime queues plain records. A span record holds trace id, span
id, parent span id, name, kind, start, end, attributes, and status. A
log record holds level, message, attributes, and the stamped context.
A metric snapshot holds instrument name, aggregation, and attribute
set. An exporter is a process that drains the queues on a timer and
does whatever it does.

The `OpenTelemetry` package is one exporter and encodes OTLP. Stdout
is another. The test harness is a third, and it is how a test asserts
that a span was recorded. The contract is designed once for all three
signals, and it is the part `seq_trace` lacked.

An exporter that posts to a collector over the network depends on the
socket timeouts in [IO.md](IO.md#timeouts-are-socket-state). Without
them a collector that stops reading parks the exporter process on a
write forever, its queue fills, and the drop counter is the only thing
keeping the application alive. The same holds for a network log
handler. The deadlines land before any exporter ships, which is the
order [ROADMAP.md](ROADMAP.md) records.

## Log

Every Koja service today hand-rolls a `Log` struct. remem carries a
vendored `lib/log` and `auth_manager` has its own, and until recently
remem also carried its own calendar math to print a timestamp. This
is the clearest stdlib gap of the three signals.

**Two Elixir designs.** `Logger.Backends` (Elixir 1.0 to 1.14) ran
every backend inside one `gen_event` process. A log call sent a
message to that process, which fanned out to each backend. Overload
protection was global, one slow backend slowed all of them, one
crashing backend could take the manager down, and a burst filled the
mailbox before anyone noticed. Erlang `:logger` handlers (OTP 21,
Elixir 1.15) run in the calling process as a function call. Handler
and formatter are separate. Filters run before handlers. A handler
that raises is caught and removed. Handlers that need async IO own a
private process and their own overload protection with four modes,
async, sync, drop, and flush. Elixir moved to the second design for
these reasons, and the old backends became a compatibility package.
Koja follows the handler model.

### Shape

```koja
protocol Log.Handler
  fn handle_log(self, record: Log.Record) ! Log.Handler.Error
end

protocol Log.Formatter<T>
  fn format_log(self, record: Log.Record) -> T
end
```

The method names carry the `_log` suffix so a conforming type does not
park `handle` or `format` on its namespace. A process type that
implements `Log.Handler` still has `handle` for its messages, and a
struct that implements `Log.Formatter` still derives `Debug.format`.
remem hit the second collision in its vendored `lib/log`.

`Formatter<T>` names what the handler can write. A text handler
takes a `Formatter<String>` and a byte sink takes a
`Formatter<Binary>`, so a msgpack or protobuf formatter plugs into a
generic socket handler instead of being a whole handler of its own:

```koja
struct Log.Stdout<F: Log.Formatter<String>>
struct Log.Socket<F: Log.Formatter<Binary>>
```

`T` is unbounded. A `Formatter<JSON.Value>` feeding a handler that
adds fields and encodes is a legitimate use. What the protocol does
not cover is writing into a sink, so a formatter allocates one `T`
per record and the handler owns framing such as the trailing newline.
A sink-writing protocol over `Write` from [IO.md](IO.md) can be added
later without touching this one.

- `Log.Record` carries level, message, attributes, and is stamped by
  the runtime with pid, timestamp, and the trace and span ids from
  `Process.context`. Nobody interpolates a trace id into a message
  again.
- Structured by default. A message plus a map literal of attributes,
  as in `Log.info("checkout", ["pool": name])`. The attribute value
  type is the same `String | Int | Float | Bool` union spans use.
- Handlers run in the caller. A stdout write is one syscall and
  belongs in the request path. A handler that talks to a network owns
  its own process, posts to it with `cast`, and manages its own drop
  mode. A handler that fails is disabled with one warning through the
  remaining handlers.
- The default handler writes text in development and JSON in
  production. The JSON shape includes `logging.googleapis.com/trace`
  when the trace id is set, so Cloud Logging correlates lines to
  traces with no collector work.
- No process metadata. Elixir's `Logger.metadata(request_id: id)`
  stores per-process state in the process dictionary, and Koja has no
  process dictionary and should not grow one for this. Request
  correlation comes from the stamped context. Everything else binds
  to a logger value, the Go `slog.With` pattern.

```koja
log = Log.with(["entity": name, "tool": "create_entity"])
log.info("near duplicate refused")
log.warn("embedding failed", ["status": code])
```

Value semantics make this cleaner than Elixir's version. The bound
attributes travel with the value you pass down, not with the process,
so a `Pool` serving ten requests never leaks one request's metadata
into another's line. That is a real bug class in Elixir when
long-lived processes set metadata and forget to clear it.

- Runtime crash reports flow through the same path with pid, trace
  id, and panic message.
- A record emitted inside a sampled span can also attach to the span
  as an event. This is a handler option, off by default, since it
  doubles the data for chatty code.

### The log slot

Koja has no global mutable state and no process registry, so a static
`Log.info(...)` needs somewhere to read its level and handlers from.
Three answers were weighed.

A general write-once global, the `persistent_term` shape that Erlang
`:logger`, Rust `log`, and Go `slog.SetDefault` all use, relaxes the
no-globals rule to fix one library, and users will build registries
with it. A named `Logger` process relocates the global into a
name-to-pid table and buys back every `Logger.Backends` problem above.
Both are rejected.

The third answer is the `group_leader` shape. The runtime carries a
per-process log configuration next to `Process.context`:

- `Log.configure(config)` writes the calling process's slot. The entry
  process calls it first thing in `start`.
- `spawn` copies the parent's slot into the child.
- `Log.info` and the other level functions read the current process's
  slot. That is a field load on the process struct, cheaper than an
  atomic snapshot of a global.
- The slot is set once and read-only afterward for that process.
  Reconfiguration at runtime is not a goal, so a `SIGHUP` style toggle
  is off the table.
- The runtime default, before anyone configures anything, is text to
  stdout at `Info`. A five-line script never mentions `Log.configure`
  and still gets `Log.info` output. A compiled program on GKE opts
  into JSON through its own typed config rather than the runtime
  guessing from the environment.

One rule to teach: `configure` before `spawn`. A process spawned
before the entry process configures logging carries the default, so
the call sits at the top of `start`.

The three entry shapes behave the same because they are one
mechanism. A `.kojs` script's top-level statements run in a process,
so `Log.configure` at the top of the script sets the root of the tree.
`koja run pkg.task` synthesizes a process entry that calls `run`, so a
task body is the root. Each `test` block runs in its own process, and
that gives tests an isolation no global can:

```koja
test "warns on a bad embedding"
  lines = Log.Capture.start()
  Log.configure(Log.Config{level: Level.Debug, handlers: [lines.handler()]})

  embed_rows(db, embedder, rows)

  assert lines.take().any?(l -> l.contains?("embedding failed"))
end
```

The test configures its own process and its children. The test next
door, running in parallel, keeps the default. With a global this test
would need a mutex around the whole suite.

**Handlers are closures.** Protocol dispatch is static, so
`List<Log.Handler>` cannot hold handlers of two types without
existential protocol types. `Log.Config.handlers` is a list of
`fn (Log.Record) -> Result<(), Log.Handler.Error>` instead, the same
type-erased seam the vendored `lib/log` used for its formatter.
`Log.Handler` stays as the protocol a struct implements, and
`Log.configure` takes `handler.to_fn()` or performs the conversion.
Handler state stays out of the config and in the handler's own
process, so the value copied on `spawn` is a level and a list of
closures.

### Configuration

Typed, from the application's own config struct. The stdlib never
reads the environment.

```koja
Log.configure(Log.Config{
  level: settings.log_level,
  handlers: [Log.Stdout.new(Log.JSON.gcp()).to_fn()],
  scopes: ["postgres.wire": Level.Debug],
})
```

`settings.log_level` comes from the application's `Config`, which
reads `LOG_LEVEL` the same way it reads `PG_HOST`. One env var, one
parsing site, a `Level` enum the compiler checks.

Scopes are declared by libraries as constants and documented as part
of their API, the Zig model.

```koja
const WIRE = Log.scope("postgres.wire")

WIRE.debug("sent", ["kind": "Parse", "bytes": size])
```

The scope name is a promise the library author keeps. Refactoring the
driver's files changes nothing.

A compile-time floor in `koja.toml` under `[log] min_level` drops
calls below it before type checking, the same mechanism that removes
`test` blocks from a build. Development builds leave the floor at
`debug`. This is on the 0.20 cut list.

Per-request debug comes from a handler filter on
`record.context.sampled?`. Detail on the requests you are already
looking at, without turning it on for everything.

**The environment grammar, rejected.** `RUST_LOG=info,postgres=debug`
is a domain-specific language in a shell variable. It couples ops
configuration to code layout, because the filter key is the module
path and a rename silently breaks every deployment manifest. Nothing
type-checks it. It puts a second configuration system in the stdlib
next to the application's own. And per-package is the wrong axis,
because "debug this library" is served by a scope the library
promised, "debug the service" is served by one `LOG_LEVEL`, and "debug
this request" is served by the sampled flag. What Rust gets right,
flipping a deployed binary to debug without a rebuild, is one env var
in the pod spec that the application already reads.

### Source location

A log record wants the file, function, and line of the call site, and
so do `Kernel.panic` and `assert`. Koja has no macros, so the Zig
shape applies: one compiler builtin that folds to a
`Source.Location{file, function, line}` value at the position where it
sits. The proposed spelling is `@SOURCE`. Lowercase `@doc` and
`@intrinsic` annotate a declaration. Uppercase `@SOURCE` is an
expression that folds to a constant, and the case split matches
`SCREAMING_SNAKE` constants elsewhere in the language.

Written inside `Log.info`'s body, `@SOURCE` names a line in the stdlib,
which is useless. The useful position is the caller's, and nobody
writes `log.info("swept", at: @SOURCE)` consistently. Swift and C#
solve this by letting the builtin sit in a default parameter and
expand where the default is filled in:

```koja
fn info(
  self,
  message: String,
  attributes: Map<String, Attribute> = [:],
  at: Source.Location = @SOURCE,
)
```

This cuts against the rule in `LANGUAGE.md` that default expressions
are callee-scope and lower to adapter functions. `@SOURCE` would be
the one documented exception: a default whose value is `@SOURCE` is
supplied by the call site, and no adapter is generated for that arity.

`@PROJECT_VERSION` and `@KOJA_VERSION` fit the same family of values
the compiler supplies. `@ARGV` for scripts fits only if the family's
definition widens from "known at compile time" to "supplied from
outside the program and fixed for the life of the process," and that
is a separate decision. `@SOURCE` is first on the 0.20 cut list and
needs a cooling period before it is built.

## Two fields, no process dictionary

The log slot invites the generalization: if the runtime can carry log
configuration per process, why not a typed inherited map that any
library can write to? The answer is no, and the reason is that the
slot is the opposite of a process dictionary on every axis that
matters.

Erlang's process dictionary is per-process, keyed by any term, mutable
at any time by the owner, untyped, and not inherited on `spawn`.
Elixir's `Logger.metadata` sits on it, and the bug that follows is the
one named above: a long-lived process sets request metadata and
forgets to clear it. The two Koja fields have fixed types, are set
once and then read-only for the process, are inherited by children,
and are owned by the runtime. They are closer to a process header than
to a dictionary. You cannot put a request id in one during a request,
and that is the point.

Each thing a dictionary might hold already has a home. Trace and span
ids are `Process.context`, and they need per-message propagation a
dictionary does not give. Log configuration is the log slot. A
deadline joins `Process.context` when it arrives, because it needs
the same per-message copy. Request metadata like a user id is
`Log.with([...])` on a value that travels with the request. Library
configuration, a pool handle, or an HTTP client is a real dependency,
and a parameter is honest about it.

The test for a third field: the runtime itself must need to read it,
as the crash reporter reads the log slot. Nothing that only a library
reads qualifies.

## Metrics

Two layers with different owners. Neither is 0.20 work, and both are
here so the exporter contract is designed once for all three signals.

**Runtime metrics** need no user code. Process count, mailbox depth,
scheduler utilization, heap size, IO wait, and message rate. The
Runtime Observability roadmap item already plans to compute these, and
the exporter contract is the only new piece.

**Application metrics** need a small API. Instruments are declared
once as constants and recorded through the handle.

```koja
const REQUEST_DURATION = Metrics.histogram(
  "http.server.request.duration",
  buckets: DEFAULT_MS,
)

REQUEST_DURATION.record(elapsed, ["http.route": route])
```

Never string-named at the call site. A typo is a compile error, bucket
configuration has a home, and the runtime pre-allocates the shard slot
at declaration instead of hashing a string per call.

Aggregation lives in the stdlib. Per-scheduler shards with atomic adds
on the hot path, merged by the exporter on its timer. Histogram
buckets are fixed at declaration. A cardinality cap drops new
attribute sets past a limit and counts the drops, because unbounded
label sets are how every metrics system eventually falls over.

When a histogram records inside a sampled span, it keeps the trace id
as an exemplar on that bucket. Click a p99 spike, land on a trace.
OpenTelemetry added exemplars late and most SDKs do them badly. Here
it is one read of `Process.context` at record time.

Exporters again live outside the stdlib. OTLP metrics through the
`OpenTelemetry` package. A Prometheus `/metrics` scrape endpoint as a
second exporter, and the more useful one for anybody not on GCP.

The payoff is semantic conventions. If `trail` emits
`http.server.request.duration` with `http.route` and
`http.response.status_code`, `pooler` emits
`db.client.connection.wait_time`, and `postgres-koja` emits
`db.client.operation.duration`, every Koja HTTP service gets RED
metrics and database latency per route without a line of metrics
code. That is the moment stdlib placement stops being a purity
argument.

## Idiom rules

An audit of the first drafts of this design found several reflexes
borrowed from Rust. They are recorded here as rules so the
implementation does not repeat them.

- Attributes are map literals, `["key": value]`, not lists of tuples.
- Instruments and log scopes are declared constants, not strings
  looked up at the call site.
- Parse functions use the error channel like `URI.parse` and
  `Base.decode16`. `Propagation.parse` fails with
  `Propagation.Error`, it does not return `Option`.
- Spans are closure-scoped. There is no manual `start` and `finish`
  pair whose lifetime is the caller's problem.
- Mutation goes through handles. A value is rebound, never mutated in
  place, and an API that looks like mutation on a value is a bug.
- Closure error types are enums. `Result<(), Export.Error>`, not
  `Result<(), String>`.
- Packages do not read the environment. The application's config
  struct does, once, and passes typed values.
- The span kind enum is `SpanKind`, not `Kind`.

## Sequencing

The 0.20 order in [ROADMAP.md](ROADMAP.md) is by dependency.

1. Socket deadlines from [IO.md](IO.md). The exporter cannot ship
   without them.
2. `Process.context`: the slot, the envelope copy, the install at
   business dequeue. Small and mechanical, and everything after
   depends on it. Shipped.
3. `Trace` in the stdlib on top of the slot, with remem's
   `lib/open_telemetry` ported to the new API as the first exporter
   package. One mutex export queue with a drop counter first, ring
   buffers only after a benchmark asks for them. The stdlib half has
   shipped: `Trace.root`, `Trace.span`, `Trace.current`,
   `Trace.Export.pop` and `dropped`, with a 4096 record queue. The
   remem port is next.
4. The log slot, `Log.configure`, closure handlers, `Log.Record`
   stamped from `Process.context`, and the crash reporter routed
   through it. remem and `auth_manager` delete their vendored loggers.

The cut list, in drop order: `@SOURCE`, the compile-time
`[log] min_level` floor, span events on records, per-scheduler export
buffers. Runtime metrics ride with the Runtime Observability work and
application `Metrics` waits until `trail` and `pooler` show what they
want to record.

## Findings from the remem spikes

remem runs the explicit-passing version in production today as two
vendored packages. `lib/open_telemetry` threads parents as
`Option<SpanContext>` parameters that ride on `Conn` between
middleware and controllers, with a tracer process that batches spans
and posts OTLP JSON to the cluster collector. `lib/log` is a `Logger`
value built at boot from typed config and passed to every function
that logs, with text and Cloud Logging JSON formatters.

The runtime design deletes most of both. The `parent` parameters, the
`Conn` field, remem's `Trace` helper, and the threaded `Logger` go
away when the two runtime fields exist. The traceparent codec, the
OTLP encoder, the batching exporter, and the formatters survive as
the exporter package and the default handler.

### Where threading hurt

Threading the span parent by hand was awkward in three places, and
each is a finding for the runtime design.

- The `with_db` closure receives a connection and nothing else, so a
  child of the `db.query` span has no way to reach it. The embedding
  request that runs inside the database phase opens under the server
  span instead, as a sibling of `db.query`. Widening the closure to
  two parameters would force every call site into the block form.
- The `Embedder` is a value captured when the router is built, so it
  cannot know the request. Handlers rebind it with `with_trace` on
  every call, which is the Go `ctx` parameter wearing a different
  coat.
- `Mcp.handle` grew a `trace` parameter for one span, and the tool
  dispatcher rebinds the embedder again so the outgoing request nests
  under the tool span.

Threading the logger hurt everywhere at once. `App.start` builds it
and passes it to `Cleaner.Config`, `Backfill.Config`, `Embedder`, the
controllers, `Mcp.handle`, every tool function, and the trail
closures. Logging is cross-cutting and the runtime itself emits
through it, which is the test that separates it from a real
dependency like the pool.

One more finding shaped the sampler rule. remem's health probes bypass
the traced dispatch, so a `db.query` span opened under them had no
parent and was promoted to a root, producing an orphan trace every
five seconds. Under this design a process with no root context carries
the sampled bit clear and the span is dropped instead.

### Language questions

Three questions about Koja itself decide details of the API, and the
spikes answered them.

- A map literal does not widen its values into a union. The compiler
  reports `Expected Attribute, found String` for every entry. The
  package works around it with a struct `Attribute` that conforms to
  the four scalar literal protocols, so literals convert on their own
  and a variable needs `Attribute.from(value)`. Widening in collection
  literals, or a `Scalar` literal protocol for unions, is the language
  change that removes the wrapper.
- A closure cannot spell an error channel, so the exporter seam is
  `fn (String) -> Result<(), Export.Error>`. The semantics already
  work. `try` and `fail` run inside a block closure whose return type
  is `Result`, and a `! Error` function passes where a `Result`
  closure is expected. What is missing is surface. The parser reads
  `! E` only in `decl/function.rs`, so `fn (n: Int) -> Int ! Oops`
  stops at the `!`, and neither `ExprKind::Closure` nor the function
  `TypeExpr` has an `error_type` slot. Adding both, plus the same
  Ok-wrapping of the success value that declarations get, closes the
  gap.
- A `const` cannot hold a runtime handle today, so
  `const X = Metrics.histogram(...)` at module level waits on a
  constant initializer that registers with the runtime. The same
  limit affects `Log.scope`.

Smaller findings from the same spikes:

- A `type` alias for a function type is only half supported. A struct
  field of the alias type loses derived `Debug`, and a value of the
  alias type cannot be called. The package spells the function type
  out at every use.
- A block closure that starts a function body parses as a nested
  function declaration. Bind it to a name first.
- A method's generic parameters are not in scope inside a block
  closure written in that method. Move the closure body to a private
  generic function and call it from a short closure.
- Struct field defaults reject a struct literal of a type from another
  package when the literal holds `Option.None`, and a `const` of that
  shape fails to unify `Option<Ref<M, R>>`. Tracked in
  [GAPS.md](GAPS.md#struct-literal-defaults-stop-at-the-package-boundary).
  remem holds the tracer as `Option<Tracer>` instead.
- A struct field default is limited to literals and constants, so a
  field of type `Logger` cannot default to `Logger.default()`. A
  function parameter can.
- A user method named `format` collides with the derived
  `Debug.format`, so the vendored formatters expose `render`. The
  stdlib protocol methods are `format_log` and `handle_log` for this
  reason.
- A closure type spells its unit return as `fn (String) -> ()`, and a
  bare `fn (String)` does not parse in a type position.

## Open questions

- Whether `Process.context` grows a deadline, and what a downstream
  handler does when it finds one expired.
- Whether the closure form of handlers is permanent or a bridge until
  existential protocol types exist.
- The `@SOURCE` default-parameter exception, and whether `@ARGV`
  joins the `@UPPER` family.
- Whether a root with no parent should always sample, or take a
  sampler.
