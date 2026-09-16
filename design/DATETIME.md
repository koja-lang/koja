# DateTime: Calendar and Zone Types on `Timestamp`

**Status: draft (2026-09-15). Nothing here is implemented.** This
document argues a position for the 0.19 breaking window: Koja gets
civil date and time types, a zoned `DateTime`, and a `TimeZone` whose
rules come from a separately versioned package. It depends on
[TIME.md](TIME.md), which renames the epoch instant to `Timestamp` and
frees the `DateTime` name.

## Summary

- Three kinds of value, kept as separate types. `Timestamp` is an
  absolute instant. `Date`, `Time`, and `LocalDateTime` are civil
  values with no zone. `DateTime` is a `Timestamp` viewed in a
  `TimeZone`.
- `TimeZone` is an enum. `UTC` and `FixedOffset(Offset)` need no
  rules. `Named` carries an IANA identifier and its own transition
  table, so a zone value is self-contained and no database is
  consulted after construction.
- The stdlib ships `UTC` and fixed offsets. IANA rules come from a
  `koja-lang/tz` package that embeds the tz database and releases on
  IANA's schedule.
- A civil time that falls in a DST gap or overlap is surfaced as
  `TimeZone.Resolution`, an enum the caller must match.
- `Duration` arithmetic is exact and lives on `Timestamp`. Calendar
  arithmetic is named methods on the civil and zoned types.
- RFC 3339 parses to a `DateTime` in a `FixedOffset` zone and formats
  from any `DateTime`.

## What is missing today

After [TIME.md](TIME.md) the stdlib has `Timestamp`, an epoch
microsecond count. It has no calendar. There is no way to ask what day
a `Timestamp` falls on, build one from a year, month, and day, or
render one for a human. Every Koja project that needed this wrote it:
`auth_manager` carries an RFC 3339 module built on Hinnant's civil
date algorithms, and `postgres-koja` passes `timestamp` and
`timestamptz` columns through as text because there is no type to
decode them into.

The gap is not that Koja lacks a datetime type. It is that the types
which would let a program be wrong in the well-known ways are absent,
so programs are wrong in ad hoc ways instead.

## Principles

1. Three questions, three types. "When did it happen" is a
   `Timestamp`. "What was on the clock and calendar" is a civil value.
   "What was on the clock in this place" is a `DateTime`. Go's single
   `time.Time` answers all three and behaves differently depending on
   how the value was made. Everything designed after it splits them.
2. An offset is not a zone. `-05:00` is a number. `America/Chicago` is
   a rule set that produces a number for a given instant. A value that
   only knows its offset cannot do calendar arithmetic correctly, and
   the type says so.
3. A zone carries its own rules. No global database, no ambient
   configuration, no lookup at use time. Constructing a `Named` zone
   is the one place rules are needed, and it is the one place that can
   fail.
4. The compiler sees the DST edge. A civil time in a gap or an overlap
   returns an enum, and `match` exhaustiveness forces the arm. This is
   the one-hour outage from `{:ok, dt}` made unwritable.
5. Exact arithmetic and calendar arithmetic have different types.
   `Duration` is nanoseconds. "One day" is not a `Duration`.
6. Full words. `LocalDateTime`, not `NaiveDateTime`. Naive is a warning
   label on a type that is the correct choice for many columns.

## Design

### Civil types

```koja
@doc "A calendar date in the proleptic Gregorian calendar. No zone."
struct Date
  year: Int
  month: Int
  day: Int

  fn new(year: Int, month: Int, day: Int) -> Date ! Date.Error
  fn day_of_week(self) -> Weekday
  fn day_of_year(self) -> Int
  fn plus_days(self, days: Int) -> Date
  fn plus_months(self, months: Int) -> Date
  fn plus_years(self, years: Int) -> Date
  fn at(self, time: Time) -> LocalDateTime
end

@doc "A time of day with microsecond precision. No date, no zone."
struct Time
  hour: Int
  minute: Int
  second: Int
  microsecond: Int

  fn new(hour: Int, minute: Int, second: Int, microsecond: Int = 0) -> Time ! Time.Error
end

@doc "A date and a time of day with no zone."
struct LocalDateTime
  date: Date
  time: Time

  fn in_zone(self, zone: TimeZone) -> TimeZone.Resolution
  fn plus_days(self, days: Int) -> LocalDateTime
  fn plus_months(self, months: Int) -> LocalDateTime
end

enum Weekday
  Monday
  Tuesday
  Wednesday
  Thursday
  Friday
  Saturday
  Sunday
end
```

`Date.new` and `Time.new` fail on an out-of-range field, including
February 30 and hour 24. A constructed value is always valid, so no
other method has to check. `plus_months` clamps to the last day of the
target month, so January 31 plus one month is February 28 or 29. That
is the java.time and Temporal behavior and the only one that makes
`plus_months(1)` total.

`Time` has microsecond precision to match `Timestamp`. Nanoseconds
would be a precision `Timestamp` cannot round-trip.

### `Offset` and `TimeZone`

```koja
@doc "A fixed distance from UTC in whole seconds, from -18:00 to +18:00."
struct Offset
  seconds: Int

  fn new(seconds: Int) -> Offset ! Offset.Error
  fn utc -> Offset
end

@doc """
The rules that map a `Timestamp` to a civil time and back. `UTC` and
`FixedOffset` need no data. `Named` carries an IANA identifier and its
transition table, so a value of this type never consults a database.
"""
enum TimeZone
  UTC
  FixedOffset(Offset)
  Named(TimeZone.Rules)

  fn utc -> TimeZone
  fn fixed(offset: Offset) -> TimeZone
  fn identifier(self) -> String
  fn offset_at(self, at: Timestamp) -> Offset
end

@doc "An IANA zone's identifier and transition table. Built by a tz package."
struct TimeZone.Rules
  identifier: String
  transitions: List<TimeZone.Transition>
  final: TimeZone.Rule
end
```

`TimeZone.Rules` is plain data. Struct fields have no visibility of
their own in Koja, and the transition table has nothing to hide: a
rules package builds one from the identifier, the transition list,
and the rule that applies after the last transition. That struct
literal is the whole contract between the stdlib and a rules package. The stdlib never reads a file, never embeds a
table, and never has a "current database." A `TimeZone.Named` value is
a complete description of the zone and can be passed, stored in a
struct, and sent to another process like any other value. The
transition list is reference counted, so copying a zone costs nothing.

### Where the rules come from

The stdlib ships `TimeZone.utc()` and `TimeZone.fixed(offset)`. IANA
zones come from a package:

```koja
alias TZ.zone

chicago = try zone("America/Chicago")
```

`koja-lang/tz` embeds the compiled tz database, exposes
`zone(identifier: String) -> TimeZone ! TZ.Error`, and releases a new
version each time IANA does. A project adds it to `koja.toml` like any
dependency and pins the version in `koja.lock`. A government moving a
DST date with a month's notice becomes a dependency bump, not a
compiler release.

This is Elixir's `tzdata` and `tz`, Ruby's `tzinfo-data`, and Python's
`tzdata` on PyPI. Three ecosystems arrived at "the rules are a
separately versioned package" independently. The alternative that
reads `/usr/share/zoneinfo` fails in a `scratch` or distroless
container, which is where a Koja service runs, and Go and jiff both had
to add an embedded fallback for that reason. Embedding in the stdlib
works everywhere and couples IANA's release cadence to the compiler's.

The one cost is that the first program to want `America/Chicago` has to
add a dependency. The error that tells it so is a compile error, since
`TZ` is not a package until it is declared.

### `DateTime`

```koja
@doc "A `Timestamp` viewed in a `TimeZone`. The human-facing type."
struct DateTime
  timestamp: Timestamp
  zone: TimeZone

  fn now(zone: TimeZone) -> DateTime
  fn from_timestamp(timestamp: Timestamp, zone: TimeZone) -> DateTime

  fn date(self) -> Date
  fn time(self) -> Time
  fn local(self) -> LocalDateTime
  fn offset(self) -> Offset

  fn in_zone(self, zone: TimeZone) -> DateTime
  fn to_utc(self) -> DateTime

  fn plus(self, d: Duration) -> DateTime
  fn minus(self, d: Duration) -> DateTime
  fn since(self, earlier: DateTime) -> Duration

  fn plus_days(self, days: Int) -> TimeZone.Resolution
  fn plus_months(self, months: Int) -> TimeZone.Resolution
end
```

A `DateTime` is two fields and nothing is cached. `date()` and `time()`
compute the civil fields from the timestamp and the zone's offset at
that instant. This keeps the type small, keeps `Equality` meaningful
(two `DateTime` values are equal when they name the same instant in
the same zone), and means a `DateTime` can never be internally
inconsistent.

`from_timestamp` and `in_zone` cannot fail. An instant always has
exactly one civil reading in a zone. Only the other direction, civil to
instant, can fall in a gap or an overlap.

`plus(Duration)` is exact. It moves the timestamp and leaves the zone.
Adding 24 hours across a spring transition produces a `DateTime` whose
clock reads one hour later than it started. That is correct and is
what the type signature says. `plus_days` is calendar arithmetic. It
moves the civil date by one and resolves the same clock time in the
zone, which can land in a gap or overlap, so it returns a
`TimeZone.Resolution`.

### Gaps and overlaps

```koja
@doc """
The result of placing a civil time in a zone. `Unique` is the common
case. `Gap` is a clock time the zone skipped, with the instants just
before and just after the transition. `Ambiguous` is a clock time the
zone repeated, with the earlier and the later instant.
"""
enum TimeZone.Resolution
  Unique(DateTime)
  Gap(DateTime, DateTime)
  Ambiguous(DateTime, DateTime)

  @doc "The choice most callers want. Shifts forward through a gap, earlier in an overlap."
  fn compatible(self) -> DateTime
  fn earlier(self) -> DateTime
  fn later(self) -> DateTime
  fn unique(self) -> DateTime ! TimeZone.Error
end
```

`LocalDateTime.in_zone`, `DateTime.plus_days`, and `DateTime.plus_months`
return this enum. A caller that does not care writes
`.compatible()` and gets java.time's default behavior. A caller that
does care writes a `match`, and the compiler rejects a `match` that
forgets `Ambiguous`. The scheduler bug that produced a one-hour outage
in an Elixir service was a `case` with one clause. Koja cannot compile
that program.

`compatible` is a method and not the return type so that the choice is
visible at the call site. `unique` is for the caller who believes the
time cannot be in a transition, such as a time that came from a
`DateTime` in the same zone, and wants a typed failure if that belief
is wrong.

### RFC 3339

```koja
extend DateTime
  fn parse_rfc3339(text: String) -> DateTime ! DateTime.ParseError
  fn format_rfc3339(self) -> String
end
```

RFC 3339 carries an offset and no zone identifier, so
`parse_rfc3339("2026-03-08T02:30:00-05:00")` returns a `DateTime` in
`TimeZone.FixedOffset`. It is a complete and correct value. It cannot
do calendar arithmetic across a DST change, and `match zone` says so.
A caller who knows the zone follows with `.in_zone(chicago)`.
`format_rfc3339` writes the offset the zone has at that instant and
six fractional digits when the microsecond field is not zero.

This replaces the RFC 3339 item in [GAPS.md](GAPS.md). A `strftime`
style pattern formatter is not part of this document.

### Postgres

The type mapping a driver needs, which `postgres-koja` can adopt as
soon as the types exist:

- `timestamptz` is a `Timestamp`. Postgres stores it as UTC
  microseconds and discards the session zone on write, so there is no
  zone to preserve and `DateTime` would invent one.
- `timestamp` is a `LocalDateTime`. It has no zone and never did.
- `date` is a `Date`. `time` is a `Time`.
- `timetz` has no sensible mapping and Postgres's own documentation
  says not to use it. It stays text.
- `interval` is not a `Duration`. It carries months and days that are
  not fixed lengths. It stays text until a calendar span type exists.

## Migration

Additive, after [TIME.md](TIME.md).

1. `Date`, `Time`, `LocalDateTime`, `Offset`, `TimeZone` with `UTC`
   and `FixedOffset`, and `TimeZone.Resolution`. One MR. No rules
   package yet. Everything is testable against fixed offsets.
2. `DateTime` and RFC 3339. `auth_manager` drops its RFC 3339 module.
   The GAPS entry closes.
3. `TimeZone.Rules`, `Named`, and the `koja-lang/tz` package. The
   package ships with a test that resolves a known gap and a known
   overlap in a zone with a recent rule change.
4. `postgres-koja` decodes the four column types.

Steps 1 and 2 are the 0.19 surface. Step 3 can be 0.19 or 0.20
depending on how the release lands. Nothing in steps 1 and 2 changes
when it arrives.

## Rejected

- **One type with an optional zone.** Go's `time.Time`. A nil
  `Location` is UTC, `Sub` uses the monotonic reading when both
  operands have one and the wall clock otherwise, and `Equal` and `==`
  disagree. Every language designed after Go split the types.
- **`OffsetDateTime` as a separate type from `DateTime`.** java.time
  has both. A `FixedOffset` variant on `TimeZone` gives the same
  information with one zoned type, and `match` on the zone answers
  "can this value do calendar arithmetic."
- **A global time zone database.** Elixir's
  `config :elixir, :time_zone_database`. It is ambient state that a
  reader cannot see at the call site, and a library that needs a zone
  cannot know whether the application configured one. A `TimeZone`
  that carries its rules has neither problem.
- **Reading `/usr/share/zoneinfo`.** Absent in minimal containers.
- **Embedding IANA data in the stdlib.** Couples a data release cycle
  measured in weeks to a compiler release cycle measured in months.
- **Silent resolution of gaps and overlaps.** java.time and Temporal
  pick `compatible` unless asked. A default that is right most of the
  time and produces an outage the rest is the case this document
  exists to remove.
- **`Duration` that carries calendar units.** Rails's
  `ActiveSupport::Duration` and Temporal's `Duration` both do this and
  both have a documented tail of edge cases. `Duration` stays exact.
  A `Period` or `Span` type can come later if the named methods grow
  unwieldy.
- **Caching civil fields on `DateTime`.** Faster reads, and a struct
  that can disagree with itself. Two fields and a computation is the
  smaller invariant.
- **Leap seconds.** Unix time does not have them. `Timestamp` does not
  either. A `Time` with `second: 60` is rejected by `Time.new`.
- **`Naive` or `Plain` as the civil prefix.** `Naive` is disparaging
  about a type that is the correct choice for a `timestamp` column.
  `Plain` is Temporal's word and is fine, but `Local` is the java.time
  word that Elixir and Rails users also recognize, and Koja's `Time`
  never carries an offset, so Go's `time.Local` confusion does not
  apply.

## Prior art

- **java.time (JSR-310).** `Instant`, `LocalDate`, `LocalTime`,
  `LocalDateTime`, `OffsetDateTime`, `ZonedDateTime`, `ZoneId`,
  `ZoneOffset`, `Duration`, `Period`. The model here with two types
  fewer and the gap and overlap surfaced instead of defaulted.
- **Temporal (ECMAScript).** `Instant`, `PlainDate`, `PlainTime`,
  `PlainDateTime`, `ZonedDateTime`, `TimeZone`, one `Duration` with
  calendar units and a `disambiguation` option. Designed by people who
  had watched java.time land.
- **jiff (Rust).** `Timestamp`, `Zoned`, `civil::DateTime`,
  `civil::Date`, `civil::Time`, `TimeZone`, `SignedDuration`, `Span`.
  The source of the `Timestamp` name. Reads the system database with
  an embedded fallback.
- **Elixir.** `DateTime`, `NaiveDateTime`, `Date`, `Time`, a
  `Calendar.TimeZoneDatabase` behaviour with a UTC-only default, and
  `DateTime.from_naive/3` returning `{:ok, _}`, `{:ambiguous, _, _}`,
  or `{:gap, _, _}`. The source of the resolution enum and the
  separately versioned rules package.
- **Ruby.** Core `Time` with an offset and an abbreviation string,
  `DateTime` deprecated in favor of `Time`, Rails's `TimeWithZone` and
  `tzinfo` plus `tzinfo-data`. A cautionary tale for giving the best
  name to the wrong type, and one more vote for the rules package.
- **Postgres.** `timestamptz` is UTC microseconds with no zone stored.
  `timestamp` is civil. The mapping section follows from that.

## Open questions

1. **The local zone.** `TimeZone.local()` would read `TZ` and
   `/etc/localtime`, which in a container is UTC or absent, and it
   needs the rules package to produce anything but UTC. It is not in
   this document. A program that wants the host zone reads `TZ` and
   calls `TZ.zone` itself.
2. **A calendar span type.** `Period` in java.time, `Span` in jiff.
   Needed for Postgres `interval` and for "every third Tuesday."
   Deferred until the named `plus_*` methods prove insufficient.
3. **Formatting beyond RFC 3339.** A pattern language or a builder.
   Not needed for any current project.
4. **Week and ordinal dates.** ISO week numbers and `day_of_year` are
   cheap on `Date`. `day_of_year` is included. ISO week is not, pending
   a use.
5. **`Equality` on `DateTime` across zones.** Two values naming the
   same instant in different zones are not equal under the derived
   implementation. java.time agrees (`equals` differs, `isEqual`
   compares instants). `same_instant?(other)` can be added if the
   distinction bites.
