# Time: A Monotonic Clock and Sub-Millisecond Precision

**Status: implemented (2026-09-15).** The design landed in one branch
for the 0.19 breaking window. The "What is wrong today" section
describes the state before it. This document argues the position: Koja gets an
`Instant` type on a monotonic clock that counts nanoseconds, `Duration`
stores a count and the unit the caller chose, and `DateTime` becomes
`Timestamp` and counts
microseconds. It lands before the [IO design](IO.md), whose socket
timeouts take a `Duration`, and before [DATETIME.md](DATETIME.md),
which builds the calendar and zone types on `Timestamp`.

## Summary

- `Instant` is a point on the monotonic clock. It has `now`,
  `elapsed`, `since`, and `plus`. It has no epoch, no formatting, and
  no conversion to `Timestamp`.
- `Duration` is a `value` and a `unit`, from nanoseconds to hours. A
  constructor stores what the caller wrote, so the range follows the
  unit and nothing overflows on the way in. Equality and arithmetic
  work in the finer of the two units.
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
   deliver nanoseconds. Each clock type stores the precision that fits
   `Int` for its useful range, and `Duration` stores whatever unit it
   was given.
3. A monotonic point has no meaning on its own. Rust's `Instant` and
   Go's hidden monotonic reading hide the number. Koja has struct-level
   privacy only, so `Instant` keeps its field public and stays honest
   about it instead. See "Why the field is public" below.
4. Bounds carry their unit. A timeout is a `Duration`, and the call
   site reads `Duration.new(30, Duration.Unit.Seconds)`, not `30_000`.

## Design

### `Duration`

```koja
@doc "A span of time, stored as a count of one unit."
struct Duration
  unit: Duration.Unit
  value: Int

  enum Unit
    Hours
    Microseconds
    Milliseconds
    Minutes
    Nanoseconds
    Seconds
  end

  fn minus(self, other: Duration) -> Duration
  fn new(value: Int, unit: Duration.Unit) -> Duration
  fn plus(self, other: Duration) -> Duration

  fn to_hours(self) -> Int
  fn to_microseconds(self) -> Int
  fn to_milliseconds(self) -> Int
  fn to_minutes(self) -> Int
  fn to_nanoseconds(self) -> Int
  fn to_seconds(self) -> Int

  fn zero?(self) -> Bool
end

impl Equality for Duration
  fn equals?(self, other: Duration) -> Bool
end
```

`new` stores the caller's number and unit as given.
`Duration.new(30, Duration.Unit.Seconds)` holds `30` and `Seconds`,
and `Debug` prints it that way. Nothing is multiplied on the way in,
so the constructor cannot overflow, and the range follows the unit:
292 years at nanoseconds, 292,000 years at microseconds, and 292
billion years at seconds. `Minutes` and `Hours` add a factor of 60
each, which buys no range that matters. They exist so two hours reads
back as `2` and `Hours`.

One constructor rather than six `from_*` functions. The unit is
already a value, so a `from_seconds` would only spell
`Duration.Unit.Seconds` a second way, and the accessors keep the
`to_` prefix the rest of the stdlib uses (`to_string`, `to_int`,
`to_binary`). Rust's `as_secs` was the source of the earlier `as_`
spelling, and Koja has no cheap-view meaning for `as_` to carry.

The two clocks produce the unit they measure in. `Instant.since`
returns nanoseconds and `Timestamp.since` returns microseconds, so
the difference of any two valid `Timestamp`s is a valid `Duration`
without conversion. The row whose `valid_until` is `9999-12-31` has
an age of 7,973 years, and that is a plain microsecond count.

Every conversion happens on the way out. A `to_*` accessor that moves
to a coarser unit divides and truncates toward zero, the way Rust's
`as_millis` does. One that moves to a finer unit multiplies, and traps
when the result does not fit `Int`. That trap fires only when the
caller asks for a number that does not exist in 64 bits, such as
7,973 years in nanoseconds, and not because the type picked a unit on
the caller's behalf.

`plus`, `minus`, and `equals?` convert both sides to the finer of the
two units and work there. `Duration` writes its own `equals?` because
the derived one would compare fields and call one second and `1_000`
milliseconds different. `zero?` is `value == 0` and
needs no conversion. The type has no ordering. Comparing two
durations is a three-way question, and a boolean `less?` would be the
wrong shape for it. Ordering waits for open question 1. Until then
callers compare through a `to_*` accessor.

`Unit` stops at `Hours`. A day is 86,400 seconds in this type and 23
or 25 hours across a DST change in a zoned `DateTime`, so `from_days`
would be the mistake [DATETIME.md](DATETIME.md) routes to `plus_days`.
Months and years have no fixed length at all. Elixir's `Duration`
carries `month` and `year` fields, which is why it has no equality
that crosses units, no ordering, and no conversion to a number, and
why `DateTime.diff/3` there returns an integer rather than a
`Duration`.

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

  fn minus(self, d: Duration) -> Timestamp
  fn new(value: Int, unit: Duration.Unit) -> Timestamp
  fn now -> Timestamp
  fn plus(self, d: Duration) -> Timestamp
  fn since(self, earlier: Timestamp) -> Duration
  fn since_epoch(self) -> Duration
end
```

Microseconds since the epoch fit `Int` until the year 294,000.
Nanoseconds since the epoch overflow `Int` in 2262 and force a
two-field representation, which is what Go and protobuf do.
Microseconds is the ceiling because no consumer needs more to be
correct. Postgres `timestamptz`, Python `datetime`, and Elixir
`DateTime` store microseconds. Protobuf `Timestamp` and OTLP
`time_unix_nano` carry nanoseconds on the wire, and a round trip
through Koja truncates the last three digits of one of those, but
neither format's meaning depends on them. Where nanoseconds matter,
in a span's duration, `Instant` and `Duration` keep them.

The wire formats are the reason `new` takes a unit. What arrives is
`seconds` and `nanos` from protobuf, one nanosecond integer from
OTLP, seconds from a JWT `exp`, milliseconds from JavaScript and
Kafka, and microseconds from Postgres. Those are four members of
`Duration.Unit`, so `Timestamp.new(claims.exp, Duration.Unit.Seconds)`
and `Timestamp.new(nanos, Duration.Unit.Nanoseconds)` each need one
call. A protobuf pair is `Timestamp.new(seconds,
Duration.Unit.Seconds).plus(Duration.new(nanos,
Duration.Unit.Nanoseconds))`. The order matters: `Timestamp.plus`
works in microseconds, so a year 9999 protobuf timestamp converts,
where adding the two `Duration`s first would land in nanoseconds and
trap past 2262. `Minutes` and `Hours` are accepted because the enum
has them, and are of no use.

Every conversion out is one method. `since_epoch` returns the
`Duration` from the epoch in microseconds, and the caller picks the
unit from `Duration`: `t.since_epoch().to_seconds()` for Unix time,
`.to_nanoseconds()` for OTLP. `Timestamp` itself never divides or
multiplies. The one conversion table in the file is
`Duration.in_unit`.

`Timestamp.since` exists because differences of stored timestamps are a
real need (how old is this row), but the doc comment states that it is
not for measuring elapsed time in the running process. The field
`millis` becomes `microseconds`, and `timestamp_millis()` becomes
`since_epoch().to_milliseconds()`, which truncates.

One trap for the Postgres driver. Postgres `timestamp` without time
zone is civil time, a `LocalDateTime` in [DATETIME.md](DATETIME.md).
Only `timestamptz` is an epoch point, so the Postgres type that maps
to Koja `Timestamp` is the one not called timestamp.

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
  `deadline = Instant.now().plus(Duration.new(5, Duration.Unit.Seconds))` and
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

- **`Duration` as one nanosecond count.** The first draft, and Go's
  representation. It has no invariant a struct literal can break, but
  it caps every duration at 292 years, so `from_seconds` traps on a
  large input and `Timestamp.since` traps on the `9999-12-31` sentinel
  that databases use for "no expiry". Go answers the same problem by
  saturating `time.Sub`, which returns a wrong number silently.
- **`Duration` in microseconds.** Matches `Timestamp` and covers 292,000
  years. But `clock_gettime` returns nanoseconds and there is no reason
  to discard them for elapsed time in one process.
- **`Duration` as seconds plus nanoseconds.** Rust's layout. It covers
  292 billion years at nanosecond precision, but the two fields carry
  an invariant, `nanoseconds` in `0..1_000_000_000`, that any struct
  literal can violate, and Koja has no private fields to guard it.
  The `value` and `unit` pair has the same range at the coarse end,
  keeps the caller's unit for `Debug`, and has no invariant at all.
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
- **Folding `Timestamp` into `Duration`.** A `Timestamp` is a
  `Duration` from the epoch, so `Duration.since_epoch()` could replace
  the type and the unit question would vanish. What vanishes with it
  is the anchor. `Duration` has none, `Timestamp` is anchored at the
  epoch, and `Instant` at process start. The type records the anchor,
  and without it the anchor moves into the variable name, which is
  `time_t`, and the bugs are adding two timestamps or passing one as a
  timeout. The same argument would dissolve `Instant`. The calendar
  types in [DATETIME.md](DATETIME.md) also need a value that is known
  to be from the epoch before they format it as a date. The kernel
  survives as `since_epoch`.
- **Renaming `Instant`.** Java, Kotlin, JavaScript's Temporal, and
  Noda Time use `Instant` for the epoch point, the thing this document
  calls `Timestamp`. Rust uses it for the monotonic point. Koja keeps
  the Rust meaning on purpose. An instant is a moment with no calendar
  attached, which describes a monotonic reading better than it
  describes a value you can store in a database, and `Timestamp` is
  protobuf's and jiff's name for the epoch point. A reader from the
  Java side who writes `Instant.now()` expecting Unix time gets a
  process-relative number that fails on first use, which the public
  field section above already accepts.

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
