# Error Handling

**Status: implemented (2026-07-31). The `koja test` contract changes
wait on a follow-up.** This document records the design rationale.
[LANGUAGE.md](../LANGUAGE.md) documents the shipped surface. One
correction from the draft: the `Option` bridge already existed in the
stdlib as `Option.or_err`, so the design added no new method and the
text below uses that name.

## Summary

Koja gets a fallibility notation over `Result`, not a second error
channel:

- `-> T ! E` in a signature is pure notation for `-> Result<T, E>`.
- `try expr` unwraps `Ok` or returns the `Err`, widened into the
  enclosing function's declared error union.
- `fail expr` is `return Result.Err(expr)`, with the same widening.
- In a `! E` function, plain `return value` and the trailing expression
  wrap in `Ok` automatically.
- `expr rescue e -> handler` handles one expression's error inline.

Error composition across domains is Koja's existing union widening. A
function that calls into two error domains declares
`! SocketError | Error`, and both propagate without conversion
ceremony. There is no `From`-style implicit conversion protocol.

## Motivation

The unwrap-or-propagate idiom costs four to five lines per fallible
call and appears in 20+ files across the stdlib and first-party
packages. `Connection.connect` in `postgres/src/connection.koja` is
representative. This is the shipped code:

```koja
fn connect(config: Config) -> Result<Connection, Error>
  socket =
    match TCPSocket.connect(config.host, config.port)
      Result.Ok(s) -> s
      Result.Err(e) -> return Result.Err(Error.ConnectFailed(e.message()))
    end

  startup = Message.encode_startup(config.user, config.database)

  match Wire.write_all(socket, startup)
    Result.Err(e) -> return Result.Err(e)
    Result.Ok(_) -> ()
  end

  buffer: Binary = <<>>

  match handshake(socket, buffer, config)
    Result.Ok(leftover) ->
      Result.Ok(Connection{buffer: leftover, socket: socket})

    Result.Err(e) ->
      Result.Err(e)
  end
end
```

The boilerplate decomposes into three distinct pains:

1. **Propagation.** `Result.Err(e) -> return Result.Err(e)`, including
   the inverted match for unit results (the `Wire.write_all` block).
2. **Success wrapping.** Every happy return site writes
   `Result.Ok(value)`. Here the last match exists _only_ to unwrap and
   rewrap `handshake`'s result.
3. **Boundary mapping.** Wrapping a foreign error into a domain error
   (`Error.ConnectFailed(e.message())`), which is deliberate API design
   and must stay visible.

The same function under this design:

```koja
fn connect(config: Config) -> Connection ! Error
  socket = TCPSocket.connect(config.host, config.port)
    rescue e -> fail Error.ConnectFailed(e.message())

  try Wire.write_all(socket, Message.encode_startup(config.user, config.database))

  leftover = try handshake(socket, <<>>, config)
  Connection{buffer: leftover, socket: socket}
end
```

Twenty-three lines to eight, with the one deliberate decision (the
`ConnectFailed` mapping) still on the page. For a language whose core
pillar is reviewer readability, the current state is an own goal. The
protocol logic drowns in plumbing, and Koja reads worse than Go's
three-line `if err != nil`.

## Design principle: notation, not machinery

The historic case against exceptions ("fancy goto") targets one
mechanism: unwinding with dynamic handler search. Control transfers N
frames with no mark at intermediate call sites. This design has none
of that. A `try` marks every hop, every hop is one frame, and the
mechanism is an ordinary `return` of an ordinary value.
Propagation is exactly as goto as early return. CLU (1979), the
original exception design, allowed signaling only to the immediate
caller for precisely this reason. `try`-per-frame is CLU's model with
better clothes.

Every serious error-model effort of the last decade converged on the
same fixed point from both directions: typed error channel in the
signature, visible marker at every hop, values underneath, bugs in a
separate uncatchable channel. Swift's `throws` passes the error back in
a register (no unwinding) and SE-0413 typed throws is interconvertible
with `Result`. Midori's error model and Herb Sutter's P0709 reach the
same place from the exceptions side, and Rust's `?` reaches it from
the values side.

This design goes further than Zig and Swift in one way: `!` introduces
**no semantic seam**. `-> Connection ! Error` _is_
`-> Result<Connection, Error>`, the same type. Callers can `match` it,
store it in a `List`, hold it in a field, and apply `try` to it later.
`try`, `fail`, and auto-wrapping are three syntax rules with zero new
semantics, zero runtime machinery, and no unwind tables (which matters
for the leaf-staticlib runtime and eventual self-hosting). Deterministic
RC drops run as ordinary scope exits because propagation is an ordinary
return.

`!` relates to `Result` exactly as `[1, 2, 3]` relates to `List<Int>`:
notation for the common construction of an ordinary type, not a second
kind of thing.

## Why `Result` survives underneath

One obvious sharpening got serious consideration: drop `Result`
entirely and make the two channels primitive, a success channel and an
error channel, with `fail` as the only door into the second. That is
literally Swift's model (`throws` errors are not values in the return
type, and the ABI is a flag plus payload, the same ABI `!` lowers to),
so it is a road taken, not a strawman.

It fails because three situations force an outcome to be **data**, and
a channel is control flow that cannot sit still:

1. **Concurrency boundaries (decisive for Koja).** `Task.await`
   delivers the outcome of work that finished in another process,
   through a mailbox. Mailboxes hold values, so a cross-process outcome
   must be a tagged value. `Process.start` already returns
   `Result<Self, StopReason>`, and outcomes-as-data
   (`{ok, _} | {error, _}`) is load-bearing BEAM heritage.
2. **Collections and batch work.** Consider
   `urls.map(u -> fetch_limits(u))`. With channel-only errors, the
   closure's failure must pass _through_ `map`. Function types then
   grow effect information, and every higher-order function needs
   effect polymorphism (a Koka-scale type-system purchase, plus
   function coloring). With `Result` underneath, a fallible closure is
   an ordinary closure returning an ordinary value and `map` works
   today, monomorphized.
3. **Generic code over fallibility.**
   `fn retry<T, E>(op: fn () -> Result<T, E>, attempts: Int)` is
   writable with zero new machinery, while abstracting over a
   primitive channel is again effect polymorphism.

Swift's history is the empirical confirmation. It shipped channel-only,
then added `Result` to the stdlib because the community kept
reinventing it for exactly these cases. It still needs glue
(`Result(catching:)`, `.get()`) to cross between the views. Zig's
storable error unions cannot carry payloads. Koja's version makes the
crossing free because there is nothing to cross. `T ! E` is the
control-flow view, and `Result<T, E>` is the data view of one type.
Signatures show the channel and values show the data.

A union return (`T | E`, no wrapper) also lost: it collapses
when the success and error types coincide (`String | String`).
`Result`'s tags keep the channels disjoint regardless of the types they
carry.

## Surface

### `!` return notation

```koja
fn connect(config: Config) -> Connection ! Error
fn fetch_limits(url: String) -> Limits ! HTTP.Error | NumericConversionError
```

`! E` appears only in return-type position. `E` is any type usable
as a `Result` error, including unions. The grammar is unambiguous: `|`
already parses in type position, and `!` appears nowhere else in type
syntax.

### `try`

`try expr` requires `expr : Result<T, E'>` where `E'` widens into the
enclosing function's declared error type. The expression's type is `T`.
On `Err`, the function returns immediately with the error widened.

- Applies to the whole postfix chain: `try socket.read(n)` propagates
  `read`'s error.
- Works on any `Result`-typed expression, not just calls. `try saved`
  on a stored value is legal.
- Statement position discards the `Ok` payload but still propagates,
  replacing the inverted match for unit results.
- `try` inside a closure propagates to the _closure's_ return type,
  never the enclosing function. Value propagation cannot cross closure
  boundaries invisibly the way unwinding exceptions do.

### `fail`

`fail expr` is `return Result.Err(expr)` with widening. Legal anywhere
`return` is, including `match` arms (arms can already diverge).

### Ok-wrapping

In a function declared `-> T ! E`, `return value` and the trailing
expression have expected type `T` and wrap in `Result.Ok`. This is the
one rule where what executes is not literally what appears on the
page. It read fine in every worked example. If the implicitness
bothers reviewers, this is the dial to revisit first (options: wrap
only the trailing expression, or require explicit `Ok` on `return`).

### `rescue`

```koja
socket = TCPSocket.connect(config.host, config.port)
  rescue e -> fail Error.ConnectFailed(e.message())

limits = fetch_limits(primary) rescue _ -> try fetch_limits(mirror)
```

`expr rescue e -> handler` requires `expr : Result<T, E>`. The handler
binds the error and must produce `T` or diverge with `fail` or
`Kernel.panic` (a `return` is a statement, so it cannot appear in the
handler expression). The whole expression has type `T`.

`rescue` is **expression-scoped by design**: it handles exactly one
call's error, so there is no which-line-failed provenance problem. The
handler is a single expression, and anything bigger stays a `match`.
There is deliberately no block form (`begin ... rescue`). A
block-scoped rescue is a handler search, which is the goto this design
exists to avoid. Ruby's postfix expression `rescue` (`x = risky rescue default`)
imports the right instinct.

## Error composition: union widening

Propagation converts error types by widening into the declared union.
This is the existing "member widens automatically" coercion, applied
at the `try` / `fail` boundary:

```koja
fn fetch_limits(url: String) -> Limits ! HTTP.Error | NumericConversionError
  response = try HTTP.get(url)     # HTTP.Error widens into the union
  try Limits.parse(response.body)  # NumericConversionError widens too
end
```

Two error domains compose with zero ceremony and the signature honestly
enumerates every failure source. This replaces Rust's `From`-based
implicit conversion (and the `thiserror` cottage industry that feeds
it) with a rule the language already has.

When a package _wants_ opacity instead of honesty (a public API hiding
its internals), `rescue e -> fail DomainError.Whatever(...)` is the
explicit collapse point. Union widening makes mapping unnecessary, and
`rescue` keeps it available and visible.

Signature ballooning (OCaml's polymorphic-variant disease) is the known
failure mode. The `type` alias is the pressure valve, and stdlib
conventions should keep public error unions small.

## Option interop

`try` is `Result`-only. Absence is not an error until the caller names
the error, and `Option.or_err(error) -> Result<T, E>` (already in the
stdlib) is where that happens:

```koja
scheme = try parsed.scheme.or_err(Error.InvalidUrl("URL must be absolute: " <> url))
```

Rust-style `try` on `Option` inside `Option`-returning functions is
severable and deferred.

## The crash channel stays separate

Panics remain process crashes that supervision handles, never
catchable in-process. Java's deepest sin was conflating bugs
(`NullPointerException`) with environmental facts
(`FileNotFoundException`) in one catchable channel. Koja's split is
load-bearing and this design does not touch it. Crossing from the value
channel into the crash channel is always explicit:

```koja
limits = fetch_limits(config_url) rescue e -> Kernel.panic("config unavailable: #{e}")
```

The cultural guidance composes with supervision: propagate liberally
within a process, and let failures that stop being expected crash the
process for the supervisor to handle. Separately, some stdlib
`Result<_, String>` signatures should likely become panics. Reserving
`Result` for errors a caller genuinely branches on shrinks the problem
regardless of syntax.

## `koja test` rides along

The `@test` contract is already the error channel, hand-rolled. Test
functions return `Result<Bool, String>`, `Err` is the failure message,
and every test body pays the propagation ceremony for its setup calls
plus a `Result.Ok(true)` trailer. Under this design the contract
becomes `-> () ! String`:

- `fail "expected 2 fields, got #{n}"` is the assertion-failure verb.
  The Ruby test lineage (`fail` / `flunk`) makes the keyword read even
  more naturally in tests than in application code.
- `conn = try connect_trust()` makes setup failures propagate like the
  test failures they are. The integration tests' setup matches
  collapse to one token each (`test_trust_select` in
  `postgres/test/integration_test.koja` goes from 54 lines to ~27).
- Success type `()` kills the `Result.Ok(true)` trailer, and a passing
  body just ends.

The runner contract is any `-> () ! E` with `E: Debug`. Plain
`fail "message"` widens `String` into the union, domain errors ride
along raw (`result = try outcome`), and a structured `Test.Failure`
gets rich rendering. Rust's `Result`-returning tests with `?` are the
precedent, and tests are a large fraction of what AI agents write, so
the ergonomics compound.

### Assertion helpers

A `Test` package (linked only into test builds) ships a structured
failure enum and helpers that are ordinary `! Failure` functions, with
no runner magic. Extractors return the unwrapped value, which is what
kills the remaining matches. Formatting happens at construction via
the `Debug` bound, so `Failure` carries strings, not generic payloads:

```koja
struct Comparison
  actual: String
  expected: String
end

enum Failure
  Missing(String)         # expected a present value, names what was absent
  NotEqual(Comparison)    # assert_eq: rendered as a diff
  Unexpected(String)      # assert_ok hit Err / assert_err hit Ok
  Untrue(String)          # check: the described predicate was false
end

fn assert_eq<T: Equality & Debug>(actual: T, expected: T) -> () ! Failure
fn assert_ne<T: Equality & Debug>(actual: T, expected: T) -> () ! Failure
fn assert_ok<T, E: Debug>(outcome: Result<T, E>) -> T ! Failure
fn assert_err<T: Debug, E>(outcome: Result<T, E>) -> E ! Failure
fn assert_some<T>(option: Option<T>, what: String) -> T ! Failure
fn check(condition: Bool, message: String) -> () ! Failure
```

The helpers dogfood the feature (`assert_ok` is one `rescue` line and
`assert_some` is `try option.or_err(Failure.Missing(what))`). The
integration test above becomes ~13 lines:

```koja
@test "connects with trust auth and runs SELECT"
fn test_trust_select -> () ! Failure | Postgres.Error
  conn = try connect_trust()

  (conn, outcome) = conn.query("SELECT 1 AS one, 'two' AS two")
  _ = conn.close()
  result = try outcome

  try assert_eq(result.tag, "SELECT 1")
  try assert_eq(result.fields.length(), 2)
  try assert_eq(try assert_some(cell(result, 0, 0), "cell (0,0)"), "1")
  try assert_eq(try assert_some(cell(result, 0, 1), "cell (0,1)"), "two")
end
```

The expected-failure pattern (`Result.Ok(_) -> return Result.Err(...)`)
becomes `_ = try assert_err(Base.decode64("Zm9vYg"))`.

Two accepted limitations. `try assert_eq(...)` per assertion is
uniform: every statement that can end the test carries a mark. If it
grates, the dial is a compiler-known `assert` statement that
participates in the channel natively. And without macros the helpers
cannot report the assertion's line. That needs a Swift-`#line`-style caller-location
intrinsic someday, and until then the runner reports the test name and
`Failure` carries the rest. (The `assert()` builtin mentioned in older
notes does not exist in the shipped surface, so the name is free.)

## Worked example

`sasl_authenticate` in `postgres/src/connection.koja` is the stress
test: six propagation matches, two mapping matches, and two
protocol-dispatch matches. The shipped code:

```koja
priv fn sasl_authenticate(
  socket: TCPSocket,
  buffer: Binary,
  config: Config,
  mechanisms: List<String>,
) -> Result<Binary, Error>

  unless mechanisms.any?(m -> m == "SCRAM-SHA-256")
    return Result.Err(Error.UnsupportedAuthentication("server offered no supported SASL mechanism"))
  end

  password =
    match require_password(config)
      Result.Ok(p) -> p
      Result.Err(e) -> return Result.Err(e)
    end

  nonce = Scram.generate_nonce()
  initial = Message.encode_sasl_initial(
    "SCRAM-SHA-256",
    Scram.first_message("", nonce),
  )

  match Wire.write_all(socket, initial)
    Result.Err(e) -> return Result.Err(e)
    Result.Ok(_) -> ()
  end

  (continue_message, continue_buffer) =
    match read_auth_message(socket, buffer)
      Result.Ok(p) -> p
      Result.Err(e) -> return Result.Err(e)
    end

  server_first =
    match continue_message
      Backend.AuthenticationSASLContinue(text) -> text
      Backend.ErrorResponse(server_error) ->
        return Result.Err(Error.Server(server_error))
      _ -> return Result.Err(Error.Protocol("expected SASL continue message"))
    end

  proof =
    match Scram.proof(password, "", nonce, server_first)
      Result.Ok(p) -> p
      Result.Err(e) -> return Result.Err(Error.AuthenticationFailed(e))
    end

  match Wire.write_all(
    socket,
    Message.encode_sasl_response(proof.client_final),
  )
    Result.Err(e) -> return Result.Err(e)
    Result.Ok(_) -> ()
  end

  (final_message, final_buffer) =
    match read_auth_message(socket, continue_buffer)
      Result.Ok(p) -> p
      Result.Err(e) -> return Result.Err(e)
    end

  server_final =
    match final_message
      Backend.AuthenticationSASLFinal(text) -> text
      Backend.ErrorResponse(server_error) ->
        return Result.Err(Error.Server(server_error))
      _ -> return Result.Err(Error.Protocol("expected SASL final message"))
    end

  match Scram.verify_server_final(server_final, proof.server_signature)
    Result.Ok(_) -> Result.Ok(final_buffer)
    Result.Err(e) -> Result.Err(Error.AuthenticationFailed(e))
  end
end
```

Under this design, 75 lines become ~46, and the only surviving `match`
blocks are the ones dispatching on protocol message shape:

```koja
priv fn sasl_authenticate(
  socket: TCPSocket,
  buffer: Binary,
  config: Config,
  mechanisms: List<String>,
) -> Binary ! Error

  unless mechanisms.any?(m -> m == "SCRAM-SHA-256")
    fail Error.UnsupportedAuthentication("server offered no supported SASL mechanism")
  end

  password = try require_password(config)
  nonce = Scram.generate_nonce()

  try Wire.write_all(socket, Message.encode_sasl_initial(
    "SCRAM-SHA-256",
    Scram.first_message("", nonce),
  ))

  (continue_message, continue_buffer) = try read_auth_message(socket, buffer)

  server_first =
    match continue_message
      Backend.AuthenticationSASLContinue(text) -> text
      Backend.ErrorResponse(server_error) -> fail Error.Server(server_error)
      _ -> fail Error.Protocol("expected SASL continue message")
    end

  proof = Scram.proof(password, "", nonce, server_first)
    rescue e -> fail Error.AuthenticationFailed(e)

  try Wire.write_all(socket, Message.encode_sasl_response(proof.client_final))

  (final_message, final_buffer) = try read_auth_message(socket, continue_buffer)

  server_final =
    match final_message
      Backend.AuthenticationSASLFinal(text) -> text
      Backend.ErrorResponse(server_error) -> fail Error.Server(server_error)
      _ -> fail Error.Protocol("expected SASL final message")
    end

  Scram.verify_server_final(server_final, proof.server_signature)
    rescue e -> fail Error.AuthenticationFailed(e)

  final_buffer
end
```

The feature removes plumbing, not pattern matching.

## Rejected alternatives

- **Postfix `?` (Rust).** The `?` glyph is already double-booked:
  identifiers can contain it (`empty?()`) and the ternary uses it
  (`x > 2 ? "big" : "small"`). A third meaning creates a genuine parse
  collision (`x = f() ? a : b`). The semantics survive as `try`. The
  spelling does not.
- **Elixir-style `with`.** Earns its keep in dynamic Elixir where
  failure shapes are heterogeneous. In typed Koja, `try` covers the
  happy path and unions cover composition, and `with` inherits the
  provenance problem (one `else` receives all failures unmarked).
- **Gleam-style `use`.** General CPS sugar with weak priors. It reads
  inside-out, and Gleam's own users find it confusing. Noted and
  passed.
- **`From`-protocol implicit conversion (Rust).** Union widening makes
  it unnecessary, and implicit conversion hides exactly the boundary
  information a reviewer wants.
- **Combinators only (`map_err`, `or_else`).** Cheapest, but real
  protocol code is intermediate-binding-heavy and closure chains do
  not fix its shape. (`or_err` and `map_err` already exist.)
- **Nothing (Go).** The status quo is already worse than Go per
  fallible call.
- **Type-routed `return` instead of `fail`** (plain `return` picks the
  channel from the value's type). Fatal counterexample in the shipped
  stdlib: `File.read -> Result<String, String>` puts the same type on
  both sides, so no routing rule exists. The tag is semantic, not
  derivable. It would also give every return site two candidate
  expected types, breaking single-expected-type bidirectional checking
  (literal coercion, empty collections) and degrading diagnostics to
  two-candidate mush. And it erases the entry marker: `try` marks the
  channel exit and `fail` marks the entry, and the symmetry is the
  answer to "is this just exceptions?". Note the shape that motivates
  the question is a feature. In a `! E` function, `fail`s enumerate
  the failure exits, the trailing expression is the one success exit,
  and early `return` of a success goes rare (cache-hit shape) but not
  extinct.

`guard`/`let-else` (refutable pattern binding with a mandatory
diverging else) is complementary rather than competing (it generalizes
to `Option` and custom enums). Revisit it after this ships.

## Prior art

The design needs roughly six ingredients at once, and every close
language is missing one or two:

- **Zig**: error sets compose by union, `!T`, `try`. But errors are
  bare tags with no payloads (payloads would need hidden allocation,
  which Zig refuses). Koja pays that with RC value semantics.
- **OCaml polymorphic variants**: the right type theory, payloads
  included. Lost to hostile syntax, row-variable type soup, and a
  competing legacy exception channel.
- **Swift SE-0413**: the typed channel, but Swift rejected union types
  (solver cost), so multi-error composition collapses to `any Error`.
- **Rust**: great value-level `Result`, no anonymous unions (trait
  coherence), so every combination needs a nominal enum plus `From`
  impls, which is the gap `thiserror`/`anyhow` fill.
- **TypeScript / Scala 3**: real unions (ZIO's typed error channel
  composes with them), but both sit on exception-throwing substrates.
- **Roc**: anonymous tag unions plus a propagation operator. The
  closest existing kit, but pre-1.0.
- **Java checked exceptions**: the failed ancestor. The right idea (a
  typed, declared channel) that failed on propagation ceremony (edit
  every `throws` clause or wrap-and-rethrow) and subclass-only
  composition, exactly the two points `try` plus union widening fix.

Koja's enabling conjunction:

- Structural unions as coercion at declared boundaries (not general
  subtyping, so local inference survives).
- Cheap RC-backed error payloads.
- Monomorphization, so unions are concrete by codegen.
- No exception legacy.
- A separate crash channel, so the error channel never carries bugs.

None of these exist for error handling's sake, yet they cohere here
anyway.

## Compatibility and migration

The feature is purely additive over the shipped language:

- **No new type, no ABI change.** `T ! E` desugars to `Result<T, E>`
  during typecheck, so IR, both backends, and the runtime never see
  it. Old and new signatures interoperate in both directions: new code
  can `try File.read(path)` against the untouched stdlib, and
  match-based callers consume `! E` functions without knowing.
- **Existing bodies check unchanged.** Ok-wrapping keys on the `!`
  _spelling_, not the type: a function declared `-> Result<T, E>`
  keeps return positions expecting `Result<T, E>`, so its explicit
  `Result.Ok(x)` returns compile as before. Declaring with `!` is the
  opt-in. `try`, `fail`, and `rescue` are unambiguous and legal in
  either spelling.
- **Keyword reservation is the only breakage surface.** A corpus scan
  (2026-07-30) of all first-party `.koja`/`.kojs` found `try`, `fail`,
  and `rescue` only in comments, strings, and `@test` descriptions,
  never as identifiers. Identifiers such as `fail?` remain legal (the
  keyword check is on the whole token).
- **Migration is optional and incremental.** Signatures can flip to
  `!` file by file with zero downstream breakage, and the `Option`
  bridge (`or_err`) was already in the stdlib.

## Effect on `Result`'s combinators

The rule after this ships is simple: **control flow uses `try` /
`rescue` / `fail`, and combinators are for outcomes-as-data** (results
held in collections, returned by `Task.await`, or stored in fields,
the places propagation cannot reach).

That retires the railway group. `then` is monadic bind, and a `then`
chain is do-notation with the names stripped out (the same
readability failure as a deeply stacked Elixir `with`). `try` is the
flattened, named-bindings replacement. A corpus scan (2026-07-30)
found one real `Result.then` call site (a trivial `try` rewrite) and
zero real `map_err` call sites. The corpus had already voted.
**This feature deletes `Result.then`.** (`Option.then` stays until
the deferred `try`-on-`Option` question settles, since chained
lookups have no substitute yet.)

The data-view group survives:

- `ok?` / `err?`: predicates for `filter` and tests.
- `ok` / `err`: `Option` bridges.
- `or`: terse default, symmetric with `Option.or`.
- `unwrap`: explicit crash-channel crossing when failure is a bug,
  with `rescue e -> Kernel.panic(...)` as the with-context form.
- `map`: transforms stored results.
- `map_err`: the only way to convert the error type of a _held_
  `Result`, since `rescue` produces `T`, not `Result<T, F>`.

Document `map` and `map_err` as data-view tools.

## Interactions and open questions

- **Exhaustiveness over unions.** `match` on `Result.Err` of a union
  hits the known nested-pattern coverage gap
  ([GAPS.md](GAPS.md#inference-and-ergonomics-warts-from-the-pooler-build)):
  splitting an outer variant by payload does not combine into coverage
  of the variant. That wart becomes a blocker once error unions are
  idiomatic, so fixing it should ride with or precede this feature.
- **Ok-wrapping implicitness.** See the dial noted under
  [Ok-wrapping](#ok-wrapping).
- **`try` on `Option`** in `Option`-returning functions: deferred,
  severable.
- **Keyword impact.** `try`, `fail`, and `rescue` become reserved
  words (pre-1.0 breakage is acceptable, and `rescue` permanently
  closes the door on Ruby block-rescue, which is intended).
- **Display convention.** `T ! E` and `Result<T, E>` are two spellings
  of one type. Docs, diagnostics, and the formatter need a convention
  for which renders where, likely `!` whenever the type appears as a
  declared return and `Result` elsewhere.
- **Generics.** Error unions are concrete at monomorphization time, so
  widening inside generic functions should fall out. The interaction
  between `try` and an error type parameter (`! E` where `E` is a type
  parameter) needs a worked rule.

## Out of scope

State-threading returns like
`query(self, sql) -> (Connection, Result<QueryResult, Error>)` get
nothing from this design: the connection must come back even on
failure, so the error channel cannot carry it. That is the
anonymous-records problem (see the `{}` glyph reservation) and stays
separate.
