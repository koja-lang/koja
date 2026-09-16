# Time: A Monotonic Clock and Sub-Millisecond Precision

**Status: implemented (2026-09-15).** The design landed in one branch
for the 0.19 breaking window. The "What is wrong today" section
describes the state before it. This document argues the position: Koja gets an
`Instant` type on a monotonic clock, `Duration` and `Instant` count
nanoseconds, and `DateTime` becomes `Timestamp` and counts
microseconds. It lands before the [IO design](IO.md), whose socket
timeouts take a `Duration`, and before [DATETIME.md](DATETIME.md),
which builds the calendar and zone types on `Timestamp`.

## Summary

- `Instant` is a point on the monotonic clock. It has `now`,
  `elapsed`, `since`, and `plus`. It has no epoch, no formatting, and
  no conversion to `Timestamp`.
- `Duration` counts nanoseconds. Constructors and accessors exist for
  seconds, milliseconds, microseconds, and nanoseconds.
- `DateTime` is renamed `Timestamp` and counts microseconds since the Unix
  epoch. The name is Postgres's and jiff's, and it frees `DateTime` for the
  zoned type in [DATETIME.md](DATETIME.md). The precision matches
  Postgres `timestamp`, Python `datetime`, and Elixir `DateTime`.
- Elapsed time is measured with `Instant`, never with `Timestamp`. The
  test runner moves first.
- New APIs that take a bound take a `Duration`. The existing `Int`
  millisecond parameters on `receive ... after`, `Ref.call`, and
  `send_after` stay for 0.19.

## What was wrong before

`time.koja` had two structs, `DateTime{millis}` and `Duration{millis}`,
and one extern, `koja_time_now_millis`, which read the wall clock.

- There is no monotonic clock. `lib/test/src/runner.koja` measures each
  test's elapsed time as the difference of two `DateTime.now()` calls.
  A clock step from NTP during a run produces a negative or wildly
  large duration. Any user code that times an operation has the same
  bug, and the language offers nothing better.
- Milliseconds are too coarse for storage. Postgres `timestamp` and
  `timestamptz` carry microseconds, so a `DateTime` round trip through
  a database truncates on the way out and cannot reproduce what came
  in. A driver is the first place this bites, and `postgres-koja`
  already passes timestamps through as text for this reason.
- Milliseconds are too coarse for measurement. A benchmark or a
  per-query timing that reads `0ms` for most samples is not a
  measurement.
- Timeouts have no type. `receive ... after 5000`, `ref.call(msg, 5000)`,
  and `send_after(msg, 200)` take a bare `Int`. The unit is in the doc
  comment, and a caller who passes seconds gets a 5 millisecond wait
  with no diagnostic.

## Principles

1. One type per question. "How long since" is `Instant`. "What time
   is it" is `Timestamp`. "How much time" is `Duration`. A value of one
   type does not convert to another without going through a `Duration`.
2. Resolution is the platform's, not a rounding choice. Both clocks
   deliver nanoseconds. The stored precision of each type is whatever
   fits `Int` for its useful range.
3. A monotonic point has no meaning on its own. Rust's `Instant` and
   Go's hidden monotonic reading hide the number. Koja has struct-level
   privacy only, so `Instant` keeps its field public and stays honest
   about it instead. See "Why the field is public" below.
4. Bounds carry their unit. A timeout is a `Duration`, and the call
   site reads `Duration.from_seconds(30)`, not `30_000`.

## Design

### `Duration`

```koja
@doc "A span of time. Counts nanoseconds and covers about 292 years."
struct Duration
  nanoseconds: Int

  fn from_seconds(seconds: Int) -> Duration
  fn from_milliseconds(milliseconds: Int) -> Duration
  fn from_microseconds(microseconds: Int) -> Duration
  fn from_nanoseconds(nanoseconds: Int) -> Duration

  fn as_seconds(self) -> Int
  fn as_milliseconds(self) -> Int
  fn as_microseconds(self) -> Int
  fn as_nanoseconds(self) -> Int

  fn plus(self, other: Duration) -> Duration
  fn minus(self, other: Duration) -> Duration
  fn zero?(self) -> Bool
end
```

`Int` is 64-bit, so nanoseconds cover 292 years, which is longer than
any process runs. The `as_*` accessors truncate toward zero, the way
Rust's `as_millis` does. `Duration` derives `Equality`. It has no
ordering. Comparing two durations is a three-way question, and a
boolean `less?` would be the wrong shape for it. Ordering waits for
open question 1. Until then callers compare through
`as_nanoseconds()`.

The current `millis()` accessor becomes `as_milliseconds()`. The field
`millis` becomes `nanoseconds`.

### `Instant`

```koja
@doc """
A point on the monotonic clock. Only useful relative to another
`Instant`. It has no epoch and is not a timestamp.
"""
struct Instant
  nanoseconds: Int

  @doc "The current monotonic time."
  fn now -> Instant

  @doc "Time since this instant."
  fn elapsed(self) -> Duration

  @doc "Time from `earlier` to this instant. Zero if `earlier` is later."
  fn since(self, earlier: Instant) -> Duration

  fn plus(self, d: Duration) -> Instant
  fn minus(self, d: Duration) -> Instant
end
```

`Instant` derives `Equality` and `Debug`. It has no ordering, for the
same reason `Duration` has none.

#### Why the field is public

Rust hides the number inside `Instant`. Koja cannot. Struct fields
have no visibility of their own, the whole struct is public or
package private, and that is a language decision, not a gap. A public
`Instant` therefore has a public `nanoseconds`, and someone will read
it. Three choices keep that honest rather than pretend otherwise.

- The anchor is the process, not boot. The runtime fixes the anchor on
  the first monotonic read, so a value reads as `1_284_113_009` about a
  second in. Nobody mistakes that for an epoch value, and it is useless
  to store because the next run starts from zero again. A misuse fails
  loudly instead of subtly. `Instant{nanoseconds: 0}` is the moment the
  anchor was fixed.
- `Debug` is derived. `Instant{nanoseconds: 1284113009}` is the truth.
  With a public field, an opaque rendering would only hide what any
  field read shows.
- The doc comment says what the number is not. Not comparable across
  processes or machines, not a timestamp, and `Timestamp.now` is the
  wall clock if that is what you wanted.

The remaining hazard is two processes on one machine comparing raw
values. That produces nonsense on the first try, which is as good as a
public field can do. Making `Instant` a builtin to get an opaque
representation was rejected: a builtin is for a representation the
language cannot express, and `Instant` is a struct with one field.

The monotonic clock is `CLOCK_MONOTONIC` on Linux and macOS. It does
not advance while the machine sleeps, which is the behavior every
timeout wants. `CLOCK_BOOTTIME` is not offered. If a caller needs
"including sleep", that is a `Timestamp` difference and the caller
accepts the wall clock's hazards knowingly.

The two-`Instant` subtraction saturates at zero rather than failing or
going negative. `since` is documented as such. This is the Rust
position since 1.60 and avoids a panic on a clock that reports a
smaller value after a CPU migration on some virtualized hosts.

### `Timestamp`

```koja
@doc "Wall-clock time as microseconds since the Unix epoch."
struct Timestamp
  microseconds: Int

  fn now -> Timestamp
  fn from_milliseconds(milliseconds: Int) -> Timestamp
  fn from_microseconds(microseconds: Int) -> Timestamp

  fn as_milliseconds(self) -> Int
  fn as_microseconds(self) -> Int

  fn plus(self, d: Duration) -> Timestamp
  fn minus(self, d: Duration) -> Timestamp
  fn since(self, earlier: Timestamp) -> Duration
end
```

Microseconds since the epoch fit `Int` until the year 294,000 and
round-trip through Postgres, SQLite's `julianday`, and every wire
format that carries microseconds. Nanoseconds since the epoch overflow
`Int` in 2262 and force a two-field representation, which is what Go
does, for a precision no database stores. Microseconds is the
practical ceiling for a calendar timestamp.

`Timestamp.since` exists because differences of stored timestamps are a
real need (how old is this row), but the doc comment states that it is
not for measuring elapsed time in the running process. The field
`millis` becomes `microseconds`, and `timestamp_millis()` becomes
`as_milliseconds()`, which truncates. The `from_*` and `as_*` names match
`Duration` so the two types read the same way.

RFC 3339 formatting and parsing remain a [GAPS.md](GAPS.md) item and
are not part of this document. When they land they format the
microseconds.

### Externs

Two externs replace one:

```koja
@extern "C"
priv fn koja_time_now_microseconds -> Int64

@extern "C"
priv fn koja_time_monotonic_nanoseconds -> Int64
```

`koja_time_now_millis` goes. The interpreter shims in
`koja-ir-eval/src/externs/time.rs` and the posix runtime in
`koja-runtime-posix/src/system.rs` both gain the two functions. The
monotonic extern reads `std::time::Instant` against an anchor held in
a `OnceLock`, fixed on the first call. The LLVM and interpreter time
tests move to the new names, and the stdlib test adds one that two
`Instant.now()` calls separated by a `receive ... after 20` differ by
at least 20 milliseconds.

A stale `DateTime` gets a hint. `koja-typecheck` keeps a small table
of renamed globals and attaches `` `DateTime` was renamed to
`Timestamp` `` to the unknown-identifier and unknown-type errors, so
an old program points at its own fix.

### Where `Instant` gets used first

- `lib/test/src/runner.koja`. `started = Instant.now()` and
  `elapsed = started.elapsed()`. `Outcome` and `Summary` carry the
  `Duration`, the human reporters pick a unit from the size, and the
  `json` reporter emits integer microseconds.
- Socket timeouts in [IO.md](IO.md). `with_read_timeout(Duration)`
  bounds one call. A whole-request budget is
  `deadline = Instant.now().plus(Duration.from_seconds(5))` and
  `remaining = deadline.since(Instant.now())` before each read.
- `Runtime` metrics that report uptime or scheduler time, when they
  gain a duration field.

## Migration

One MR, one breaking line in the changelog.

1. Add `Instant`, rename `DateTime` to `Timestamp`, rename the fields,
   add the accessors, replace the extern, and update the runner. Every
   `DateTime` mention, every `Duration{millis: ...}` literal, and every
   `timestamp_millis()` call breaks. A grep across `lib`, `examples`,
   `postgres-koja`, and `remem` finds every one. `DateTime` does not
   come back until [DATETIME.md](DATETIME.md) lands, so a stale use is a
   compile error and not a silent change of meaning.
2. Leave `receive ... after`, `Ref.call`, `send_after`, and
   `Test.Runner`'s `--timeout` on `Int` milliseconds. Changing
   `receive ... after` is a parser and typecheck change, and changing
   the others alone would leave the language inconsistent. A follow-up
   can accept `Int | Duration` on all four at once, and `Duration` can
   become the only form in 0.20.

The IO migration then takes `Duration` in every new signature from its
first step.

## Rejected

- **`Duration` in microseconds.** Matches `Timestamp` and covers 292,000
  years. But `clock_gettime` returns nanoseconds and there is no reason
  to discard them for a type whose range only has to cover one
  process's lifetime.
- **`Timestamp` in nanoseconds.** Overflows `Int` in 2262 and stores
  more than any database accepts.
- **A `Time` union or protocol over both clocks.** The two clocks answer
  different questions and one abstraction over them invites the bug
  this document exists to remove.
- **`Instant.to_datetime`.** Converting a monotonic point to wall time
  needs the offset at the moment of conversion, and the result is a
  wall time that can be wrong by any clock step since the `Instant`
  was taken. Go hides this inside `time.Time`. Koja keeps the types
  apart.
- **Erlang's three clocks and time-warp modes.** `monotonic_time`,
  `system_time`, and `time_offset` exist for long-running distributed
  nodes that must agree on time across a clock step. Koja has no
  distribution and no node-lifetime clock agreement to preserve.
- **Changing `receive ... after` in this MR.** See migration step 2.

## Prior art

- **Rust.** `Instant` (monotonic, opaque, `elapsed`, saturating
  subtraction) and `SystemTime` (wall, `duration_since(UNIX_EPOCH)`).
  `Duration` is seconds plus nanoseconds. The split is the model here.
- **Go.** One `time.Time` that carries a hidden monotonic reading when
  produced by `time.Now()`. `Sub` uses the monotonic part when both
  operands have one. Convenient, and the source of a class of bugs when
  a `Time` from `Unix()` is compared with one from `Now()`.
- **Elixir and Erlang.** `System.monotonic_time/1` with a unit argument,
  `DateTime` with microsecond precision and a `{microsecond, precision}`
  pair. `:timer.tc/1` for elapsed measurement.
- **Postgres.** `timestamp` and `timestamptz` are 64-bit microseconds
  since 2000-01-01. `interval` carries microseconds.

## Open questions

1. **Ordering and operators.** `Instant`, `Duration`, and `Timestamp`
   have no comparison at all. Comparing two of them is a three-way
   question, so the answer is an `Ordering` enum (`Less`, `Equal`,
   `Greater`) and a `Comparable` protocol whose one function is
   `compare(self, other: Self) -> Ordering`, the same protocol
   [GAPS.md](GAPS.md) names for `Binary.compare`. Whether `<`, `<=`,
   `>`, and `>=` then route
   through `compare` the way `==` routes through `Equality` is a
   compiler change and its own MR. The three time types declare
   `Comparable` when it exists. Arithmetic operators on user types are
   a separate question, and `plus` and `minus` stay as the names.
2. **`Process.sleep(Duration)`.** There is no sleep today. Whether the
   first one takes a `Duration` or an `Int` depends on whether
   migration step 2 has landed. It should take a `Duration`.
3. **`Instant` on the LLVM backend across process boundaries.** Each
   spawned process runs in the same OS process, so one clock is shared.
   If `koja test` ever isolates tests in OS processes, an `Instant` from
   one is meaningless in another. The type's lack of a storable
   representation already prevents sending one across a boundary.
