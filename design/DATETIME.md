# DateTime: Calendar and Zone Types on `Timestamp`

**Status: steps 1 and 2 implemented (2026-09-16). Steps 3 and 4 are
open.** This document argues a position for the 0.19 breaking window:
Koja gets civil date and time types, a zoned `DateTime`, and a
`TimeZone` whose rules come from a separately versioned package. It
depends on [TIME.md](TIME.md), which renames the epoch instant to
`Timestamp` and frees the `DateTime` name.

## Summary

- Three kinds of value, kept as separate types. `Timestamp` is an
  absolute instant. `Date`, `Time`, and `LocalDateTime` are civil
  values with no zone. `DateTime` is a `Timestamp` viewed in a
  `TimeZone`.
- `TimeZone` is an enum. `UTC` and `Fixed(TimeZone.Offset)` need no
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
- Text goes through a `Format` protocol per type. `ISO8601` is the
  stdlib implementation, and its `DateTime` output is valid RFC 3339.

## What is missing today

Before this document the stdlib had `Timestamp`, a `Duration` since
the epoch. It had no calendar. There was no way to ask what day
a `Timestamp` fell on, build one from a year, month, and day, or
render one for a human. Every Koja project that needed this wrote it:
`auth_manager` carried an RFC 3339 module built on Hinnant's civil
date algorithms, and `postgres-koja` passed `timestamp` and
`timestamptz` columns through as text because there was no type to
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
   `Duration` counts a fixed-length unit. "One day" is not a `Duration`.
6. Full words. `LocalDateTime`, not `NaiveDateTime`. Naive is a warning
   label on a type that is the correct choice for many columns.

## Design

### Civil types

```koja
@doc "A calendar date in the proleptic Gregorian calendar. No zone."
struct Date
  day: Int
  month: Int
  year: Int

  enum Error
    InvalidDay(Int)
    InvalidMonth(Int)
    InvalidYear(Int)
    Malformed(String)
  end

  protocol Format
    fn format_date(self, value: Date) -> String
    fn parse_date(self, text: String) -> Date ! Date.Error
  end

  const UNIX_EPOCH = Date{day: 1, month: 1, year: 1970}

  fn at(self, time: Time) -> LocalDateTime
  fn day_of_week(self) -> Weekday
  fn day_of_year(self) -> Int
  fn days_in_month(self) -> Int
  fn from_epoch_days(days: Int) -> Date
  fn leap_year?(self) -> Bool
  fn new(year: Int, month: Int, day: Int) -> Date ! Date.Error
  fn parse<F: Date.Format>(text: String, format: F = ISO8601.Extended) -> Date ! Date.Error
  fn plus_days(self, days: Int) -> Date
  fn plus_months(self, months: Int) -> Date
  fn plus_years(self, years: Int) -> Date
  fn to_epoch_days(self) -> Int
  fn to_string<F: Date.Format>(self, format: F = ISO8601.Extended) -> String
end

@doc "A time of day with microsecond precision. No date, no zone."
struct Time
  hour: Int
  microsecond: Int
  minute: Int
  second: Int

  enum Error
    InvalidHour(Int)
    InvalidMicrosecond(Int)
    InvalidMinute(Int)
    InvalidSecond(Int)
    Malformed(String)
  end

  protocol Format
    fn format_time(self, value: Time) -> String
    fn parse_time(self, text: String) -> Time ! Time.Error
  end

  const MIDNIGHT = Time{hour: 0, microsecond: 0, minute: 0, second: 0}

  fn after_midnight(duration: Duration) -> Time
  fn new(hour: Int, minute: Int, second: Int, microsecond: Int = 0) -> Time ! Time.Error
  fn parse<F: Time.Format>(text: String, format: F = ISO8601.Extended) -> Time ! Time.Error
  fn since_midnight(self) -> Duration
  fn to_string<F: Time.Format>(self, format: F = ISO8601.Extended) -> String
end

@doc "A date and a time of day with no zone."
struct LocalDateTime
  date: Date
  time: Time

  enum Error
    Malformed(String)
  end

  protocol Format
    fn format_local_date_time(self, value: LocalDateTime) -> String
    fn parse_local_date_time(self, text: String) -> LocalDateTime ! LocalDateTime.Error
  end

  fn in_zone(self, zone: TimeZone) -> TimeZone.Resolution
  fn parse<F: LocalDateTime.Format>(text: String, format: F = ISO8601.Extended) -> LocalDateTime ! LocalDateTime.Error
  fn plus_days(self, days: Int) -> LocalDateTime
  fn plus_months(self, months: Int) -> LocalDateTime
  fn to_string<F: LocalDateTime.Format>(self, format: F = ISO8601.Extended) -> String
end

enum Weekday
  Monday
  Tuesday
  Wednesday
  Thursday
  Friday
  Saturday
  Sunday

  fn iso_number(self) -> Int
end
```

`Date.new` and `Time.new` fail on an out-of-range field, including
February 30 and hour 24. A constructed value is always valid, so no
other method has to check. `plus_months` clamps to the last day of the
target month, so January 31 plus one month is February 28 or 29. That
is the java.time and Temporal behavior and the only one that makes
`plus_months(1)` total.

The year runs from 0 to 9999, the range the ISO 8601 profile can
write without an expanded year. `Date.new` rejects a year outside it
with `InvalidYear`, and arithmetic that leaves it traps, the way `Int`
overflow does. The whole range is about 3.2 × 10^17 microseconds,
well inside an `Int`, so every conversion in this document is exact.

`Date.from_epoch_days` and `to_epoch_days` are Hinnant's civil date
algorithms, public so that `DateTime` and a database driver can share
one implementation. Both use floor division so that a day before the
epoch lands on the right date. `Int` division truncates toward zero,
and `-1 / 86_400_000_000` being `0` is the bug every hand-written
version of this has shipped once.

`Time` has microsecond precision. `Timestamp` keeps whatever unit its
value arrived in, and `Timestamp.now` reads microseconds, so a
nanosecond `Time` would carry digits the clock cannot supply and no
database stores. `DateTime.time` on a nanosecond `Timestamp` truncates
to microseconds. `Time.after_midnight` does the same for a `Duration`
and wraps at 24 hours, so a negative `Duration` counts back from
midnight.

The error enums carry the rejected field. `Malformed` carries the whole
text a format could not read. `LocalDateTime.Error` has only
`Malformed`, since a format reads the two halves together and a
`LocalDateTime` is never built from fields on its own.

### `TimeZone.Offset` and `TimeZone`

```koja
@doc """
The rules that map a `Timestamp` to a civil time and back. `UTC` and
`Fixed` need no data. `Named` carries an IANA identifier and its
transition table, so a value of this type never consults a database.
"""
enum TimeZone
  Fixed(TimeZone.Offset)
  UTC
  Named(TimeZone.Rules)

  enum Error
    Ambiguous(DateTime, DateTime)
    Gap(DateTime, DateTime)
  end

  @doc "A fixed distance from UTC in whole seconds, from -18:00 to +18:00."
  struct Offset
    seconds: Int

    enum Error
      OutOfRange(Int)
    end

    const UTC = TimeZone.Offset{seconds: 0}

    fn new(seconds: Int) -> TimeZone.Offset ! TimeZone.Offset.Error
    fn to_string(self) -> String
  end

  fn identifier(self) -> String
  fn offset_at(self, at: Timestamp) -> TimeZone.Offset
end
```

`Offset` nests under `TimeZone` because it is meaningless anywhere
else, and the flat name would take a common word from user code.
`TimeZone.UTC` is the variant and `TimeZone.Offset.UTC` is the
constant, so neither needs a function that returns a literal. A zone
is built with its variant, `TimeZone.Fixed(offset)`, and there is no
`fixed` constructor to learn beside it.

`Named` is not in 0.19. When the rules package lands, adding the
variant is a breaking change for every exhaustive `match zone` in user
code, and that is the point. A program that matched `UTC` and `Fixed`
and did calendar arithmetic on both has to decide what it does with a
zone that has transitions.

@doc "An IANA zone's identifier and transition table. Built by a tz package."
struct TimeZone.Rules
identifier: String
transitions: List<TimeZone.Transition>
final: TimeZone.Rule
end

````

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

The stdlib ships `TimeZone.UTC` and `TimeZone.Fixed(offset)`. IANA
zones come from a package:

```koja
alias TZ.zone

chicago = try zone("America/Chicago")
````

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

  enum Error
    Malformed(String)
  end

  protocol Format
    fn format_date_time(self, value: DateTime) -> String
    fn parse_date_time(self, text: String) -> DateTime ! DateTime.Error
  end

  fn now(zone: TimeZone) -> DateTime

  fn date(self) -> Date
  fn time(self) -> Time
  fn local(self) -> LocalDateTime
  fn offset(self) -> TimeZone.Offset

  fn in_zone(self, zone: TimeZone) -> DateTime
  fn to_utc(self) -> DateTime

  fn plus(self, d: Duration) -> DateTime
  fn minus(self, d: Duration) -> DateTime
  fn since(self, earlier: DateTime) -> Duration

  fn plus_days(self, days: Int) -> TimeZone.Resolution
  fn plus_months(self, months: Int) -> TimeZone.Resolution

  fn to_string<F: DateTime.Format>(self, format: F = ISO8601.Extended) -> String
  fn parse<F: DateTime.Format>(text: String, format: F = ISO8601.Extended) -> DateTime ! DateTime.Error
end
```

A `DateTime` is two fields and nothing is cached. `date()` and `time()`
compute the civil fields from the timestamp and the zone's offset at
that instant. This keeps the type small, keeps `Equality` meaningful
(two `DateTime` values are equal when they name the same instant in
the same zone), and means a `DateTime` can never be internally
inconsistent.

There is no `from_timestamp`. The struct literal
`DateTime{timestamp: t, zone: z}` is the constructor, and it cannot
fail. An instant always has exactly one civil reading in a zone. Only
the other direction, civil to instant, can fall in a gap or an
overlap.

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
  Ambiguous(DateTime, DateTime)
  Gap(DateTime, DateTime)
  Unique(DateTime)

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

### Formats

Text is a protocol per type, and a format is a value that implements
the protocols for the types it can read and write:

```koja
protocol Date.Format
  fn format_date(self, value: Date) -> String
  fn parse_date(self, text: String) -> Date ! Date.Error
end

protocol Time.Format
  fn format_time(self, value: Time) -> String
  fn parse_time(self, text: String) -> Time ! Time.Error
end

protocol LocalDateTime.Format
  fn format_local_date_time(self, value: LocalDateTime) -> String
  fn parse_local_date_time(self, text: String) -> LocalDateTime ! LocalDateTime.Error
end

protocol DateTime.Format
  fn format_date_time(self, value: DateTime) -> String
  fn parse_date_time(self, text: String) -> DateTime ! DateTime.Error
end

enum ISO8601
  Basic
  Extended
end

impl Date.Format for ISO8601 ... end
impl Time.Format for ISO8601 ... end
impl LocalDateTime.Format for ISO8601 ... end
impl DateTime.Format for ISO8601 ... end
```

Each type has a `to_string(format)` method and a `parse(text, format)`
function that forward to the protocol, and `format` defaults to
`ISO8601.Extended`, so the call site reads `date.to_string()` and
`Date.parse("2026-09-16")` in the common case and
`date.to_string(ISO8601.Basic)` otherwise. The text comes first in
`parse` because that is where `Int.parse`, `URI.parse`, and
`JSON.decode` put it. The method is `to_string` and not `format`
because every type derives `Debug`, whose `format(self)` would
collide with the zero-argument adapter, and because `IPAddress` and
`TimeZone.Offset` already pair `parse` with `to_string`.

The protocol method names carry the type (`format_date`, not
`format`) because one format implements all four protocols, and Koja
mangles a method by type and name, not by protocol. Four protocols
with the same `format` would collide on `ISO8601`.

The default on a generic parameter needed one compiler change. A
default desugars into a shorter-arity adapter, and the adapter used to
keep every type parameter, so `parse/1<F>(text: String)` had an `F`
no argument could bind. The adapter now keeps only the type
parameters its remaining signature mentions, and `F` comes from the
default expression through the canonical call.

`ISO8601` is the one format the stdlib ships. It is a profile, one
shape per type, in the two variants the standard defines. `Extended`
has separators, `2026-09-16T17:39:00Z`. `Basic` has none,
`20260916T173900Z`. Each variant reads and writes its own shape only.
A `DateTime` reads and writes a mandatory offset, `Z` for the `UTC`
zone and `+HH:MM` for a `Fixed` zone, and a numeric offset always
parses to `Fixed`, so `+00:00` and `Z` are different zones that name
the same instant. A `Time` reads optional seconds, since HTML `time`
and `datetime-local` inputs omit them, and a fraction of one to nine
digits after `.` or `,`, truncated to microseconds. The separator
between date and time reads as `T`, `t`, or a space and writes as `T`.

Out of the profile: week dates (`2026-W38-3`), ordinal dates
(`2026-259`), reduced precision (`2026-09`), expanded and negative
years, `24:00`, fractions on hours or minutes, durations, intervals,
and the RFC 9557 bracketed zone. `Extended` output for a `DateTime` is
also valid RFC 3339, which is what JSON consumers check, and the parse
side reads every RFC 3339 timestamp.

A parse fails with the field's error when a field is out of range and
the shape was right, so `Date.parse("2026-02-30")` is
`InvalidDay(30)`, and with `Malformed(text)` for every other input.
`LocalDateTime` and `DateTime` report a field error as `Malformed`
too, since the caller handed over one string and gets one reason.

Other formats belong to the packages that need them. `HTTP` owns the
IMF-fixdate `Date` header, `SMTP` owns RFC 5322, and a `strftime`
style pattern formatter is a `Pattern` struct that implements the
protocols when a project asks for one. None of them is in this
document.

This closed the calendar formatting gap that [GAPS.md](GAPS.md)
carried from 2026-08-10.

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

1. **[DONE]** `Date`, `Time`, `LocalDateTime`, `TimeZone.Offset`,
   `TimeZone` with `UTC` and `Fixed`, and `TimeZone.Resolution`. No
   rules package yet. Everything is testable against fixed offsets.
2. **[DONE]** `DateTime`, the four `Format` protocols, and `ISO8601`.
   The GAPS entry closes. `auth_manager` can drop its RFC 3339 module.
3. `TimeZone.Rules`, `Named`, and the `koja-lang/tz` package. The
   package ships with a test that resolves a known gap and a known
   overlap in a zone with a recent rule change.
4. `postgres-koja` decodes the four column types.

Steps 1 and 2 landed in one MR for 0.19, after two compiler changes
they needed: a protocol can nest under a type (`Date.Format`), and a
constant can nest under a type (`TimeZone.Offset.UTC`). Step 3 can be
0.19 or 0.20 depending on how the release lands. Nothing in steps 1
and 2 changes when it arrives except the `match zone` arms noted
above.

## Rejected

- **One type with an optional zone.** Go's `time.Time`. A nil
  `Location` is UTC, `Sub` uses the monotonic reading when both
  operands have one and the wall clock otherwise, and `Equal` and `==`
  disagree. Every language designed after Go split the types.
- **`OffsetDateTime` as a separate type from `DateTime`.** java.time
  has both. A `Fixed` variant on `TimeZone` gives the same
  information with one zoned type, and `match` on the zone answers
  "can this value do calendar arithmetic."
- **RFC 3339 as the only text form.** `parse_rfc3339` and
  `format_rfc3339` on `DateTime` cover JSON and nothing else. RFC 3339
  has no spelling for a bare `Date` or `Time`, and a form field, a
  CSV column, and a Postgres `date` all need one. ISO 8601 has a
  shape for every civil type, and its `DateTime` shape is RFC 3339.
- **One generic `Format<T>` protocol.** `protocol Format<T>` with
  `format(self, value: T)` and `parse(self, text) -> T`. Koja allows
  one implementation of a protocol per type, so `ISO8601` could
  implement `Format<Date>` or `Format<DateTime>` but not both. A
  protocol per type has no such limit and reads the same at the call
  site.
- **Separate parse and format protocols.** `Date.Parser` and
  `Date.Formatter` would let a write-only format skip the parser.
  That is eight protocols for four types, and every format in sight
  does both directions.
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
3. **Formatting beyond ISO 8601.** A pattern language or a builder,
   as a struct that implements the four protocols. Not needed for any
   current project.
4. **Week and ordinal dates.** ISO week numbers and `day_of_year` are
   cheap on `Date`. `day_of_year` is included. ISO week is not, pending
   a use, and the `ISO8601` profile does not read or write either
   shape until it is.
5. **`Equality` on `DateTime` across zones.** Two values naming the
   same instant in different zones are not equal under the derived
   implementation. java.time agrees (`equals` differs, `isEqual`
   compares instants). `same_instant?(other)` can be added if the
   distinction bites.
