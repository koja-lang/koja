# Standard Library Design

Design notes for Expo's standard library architecture: package hierarchy,
auto-import rules, networking stack, HTTP vocabulary types, randomness,
cryptographic primitives, and the 20-year rule for stdlib inclusion.

---

## The problem

`Global.*` is flat and overloaded. Adding networking, HTTP, TLS, and JSON to
the same auto-imported package doesn't scale. Folding `HTTP`, `TLS`, `JSON`
into `Global` would imply they're as fundamental as `Option` or `IO`, but
they're not -- `IO.puts` is used in every program while `TCPSocket` is
domain-specific.

---

## Auto-imported vs qualified

All standard library packages ship with the compiler and are always
available. The distinction is **import behavior**, not where code lives.

### Auto-imported (feel like the language)

These types are used so frequently that qualifying them would be annoying.
The compiler auto-imports them into every module -- no prefix needed:

- **Kernel types**: `Option`, `Result`, `List`, `Map`, `Set`, `Pair`, `Int`,
  `Float`, `Bool`, `String`
- **Process types**: `Process`, `Ref`, `ReplyTo`, `Task`, `Step`,
  `Lifecycle`, `StopReason`, `ExitStatus`, `ExitReason`, `CallError`
- **IO types**: `IO`, `File`, `Fd`, `System`, `DateTime`, `Duration`, `Debug`

### Qualified (domain-specific)

These types require a package-qualified path. Use `alias` to shorten:

```expo
alias Net.TCPSocket
alias HTTP.Request

conn = TCPSocket.connect("example.com", 443)
```

Without alias: `Net.TCPSocket.connect(...)`, `HTTP.Request`, `JSON.Decoder`.

This means the auto-import list is the "things that feel like the language"
list. Everything else uses the package-qualified path. The compiler knows
a fixed set of types to auto-import, regardless of which package they come
from.

---

## `net` package

Networking with TLS as a capability on `TCPSocket`, not a separate type.

### Types

- **`Net.TCPSocket`** -- **implemented** (in `Net.expo`).
  `connect(host, port)` resolves DNS and connects, `connect_addr(addr)`
  for direct connections, `read(count)`, `write(data)`, `close()`.
  TLS via `upgrade_tls(config)` (`move self -> Self`) or the
  convenience factory `connect_tls(host, port, config)` (pending).
  One type handles both plain and encrypted connections.
- **`Net.TCPListener`** -- **implemented** (in `Net.expo`).
  `bind(port)` or `bind_addr(addr)`, `accept()` (returns `TCPSocket`).
  Sets `SO_REUSEADDR` and listens with backlog 128 automatically.
- **`Net.UDPSocket`** -- **implemented** (in `Net.expo`).
  `bind(port)` or `bind_addr(addr)`, `send_to(data, addr)`,
  `recv_from(count)`. Datagram-oriented, no connection ceremony.
- **`Net.TLSConfig`** -- certificate options, verification settings. Passed
  to `upgrade_tls` or `connect_tls`. Pending implementation.
- **`Net.Socket`** -- raw POSIX socket primitives (in `Net.expo`).
  Rarely used directly -- `TCPSocket` and `UDPSocket` wrap it.

### TLS as upgrade, not separate type

A separate `TLSSocket` type would force users to decide upfront whether
they want encryption, duplicate `read`/`write`/`close` across types, and
require every socket-accepting function to handle `TCPSocket | TLSSocket`.

Instead, TLS is a capability added to `TCPSocket`:

```expo
alias Net.TCPSocket
alias Net.TLSConfig

conn = TCPSocket.connect("api.example.com", 443)
conn = conn.upgrade_tls(TLSConfig.default())
conn.write(request_bytes)
```

This mirrors how TLS actually works at the protocol level -- you start with
a TCP connection and negotiate TLS on top. The `move self -> Self` idiom
makes this feel natural in Expo.

### Implementation

`TCPSocket`, `TCPListener`, and `UDPSocket` are implemented in pure Expo
in `Net.expo`, wrapping the raw `Socket` primitives. Access requires
qualified names (`Net.TCPSocket`) or `alias Net.TCPSocket`. TLS wraps a system library
(LibreSSL/OpenSSL/BoringSSL) via C FFI (see [FFI.md](FFI.md)). Programs
that don't call `upgrade_tls` don't pull in TLS dependencies.

---

## `http` package

Shared vocabulary types and HTTP/1.1 baseline. The primary goal is
preventing the ecosystem fragmentation that plagued Elixir -- where httpc,
hackney, httpoison, Tesla, mint, finch, and req all invented their own
request/response types.

### Vocabulary types

These are protocol-version-agnostic. A `Request` looks the same whether
it came over HTTP/1.1, HTTP/2, or HTTP/3:

- **`HTTP.Request`** -- method, path, headers, body
- **`HTTP.Response`** -- status code, headers, body
- **`HTTP.Method`** -- enum: `Get`, `Post`, `Put`, `Delete`, `Patch`,
  `Head`, `Options`
- **`HTTP.Status`** -- enum or int with named constants (`Ok`, `NotFound`,
  `InternalServerError`, etc.)
- **`HTTP.Headers`** -- header collection (likely `Map<String, List<String>>` or
  a dedicated type)

If every HTTP package in the ecosystem shares these types, a router
package, a middleware package, and a client pool package all compose
naturally.

### HTTP/1.1 baseline

- **Parser**: request line, headers, chunked transfer encoding
- **Client**: one-shot `Http.get(url)`, `Http.post(url, body, headers)`
  returning `Result<Response, Error>`. Opens a TCP connection, sends
  the request, reads the response, closes. Simple, correct, good enough
  for scripts and low-frequency calls.
- **Server**: listener that accepts connections and spawns a process per
  request. Handler shape: `fn handle(request: Request) -> Response`.
  Functional, stateless, easy to understand.

### What packages add on top

- **Connection pooling**: a pool `Process` manages persistent connections
  to hosts, reuses keep-alive sockets. Different applications want different
  strategies (per-host, global, adaptive sizing) -- not stdlib's job.
- **HTTP/2 transport**: separate package, reuses `Request`/`Response` types.
- **Routing, middleware, frameworks**: packages that compose
  `fn(Request) -> Response` functions.

---

## `json` package -- **DONE** (qualified stdlib package)

Promoted from standalone package to stdlib. Ships with the compiler,
accessed via qualified names (`JSON.Value`) or `alias JSON.Value`.
Implemented in pure Expo with 17 tests covering encoder and decoder.

- `JSON.Value` enum -- `Null`, `Bool`, `Int`, `Float`, `String`, `Array`, `Object`.
  Convenience constructors: `Value.string(s)`, `Value.int(n)`, `Value.object(entries)`, etc.
- `JSON.Encoder` -- compact (`encode`) and pretty-printed (`encode_pretty`) output.
- `JSON.Decoder` -- recursive descent parser. `Decoder.decode(input)` returns `Result<Value, String>`.
- `JSON.StringBuilder` -- efficient string builder used internally by the encoder.

---

## `Random` (implemented -- in `lib/global/src/kernel.expo`)

OS-level randomness. Not crypto-specific -- random numbers are used for games,
tests, shuffling, UUID generation, and any non-deterministic behavior.

Decided against a separate `Random` package -- too small (two functions), too
fundamental. Lives in `lib/global/src/kernel.expo`, auto-imported into every
module.

### API

- **`Random.bytes(count: Int) -> Binary`** -- cryptographically secure random bytes.
- **`Random.int(min: Int, max: Int) -> Int`** -- uniform random integer in
  inclusive range [min, max]. Uses rejection sampling to avoid modulo bias.

### Implementation

Wraps the OS entropy source (`getrandom(2)` on Linux, `getentropy(2)` on macOS)
via runtime intrinsics (`expo_random_bytes`, `expo_random_int`). No userspace
PRNG -- always OS-quality randomness. Programs that never call `Random.*` pay
nothing.

---

## `crypto` package

Stable cryptographic primitives. The building blocks that TLS, HMAC-based
auth, and integrity checks all depend on. These algorithms are decades old
and standardized.

### Types

- **`Crypto.Hash`** -- `sha256(data) -> Binary`, `sha384(data) -> Binary`,
  `sha512(data) -> Binary`. One-shot hashing. Streaming/incremental hashing
  can be added later if needed.
- **`Crypto.HMAC`** -- `sign(algorithm, key, data) -> Binary`,
  `verify(algorithm, key, data, signature) -> Bool`. Keyed message
  authentication.

### What stays in packages

Password hashing algorithms (`argon2`, `bcrypt`) are first-party packages,
not stdlib. They're algorithm-specific, evolve with computing power, and
wrap specific C libraries. The stdlib provides the primitives they're built
on.

### Implementation

Wraps system crypto libraries (CommonCrypto on macOS, OpenSSL/libcrypto on
Linux) via C FFI. TLS (`upgrade_tls`) separately wraps `libssl` from the
same OpenSSL distribution -- they're sibling FFI bindings, not layered.
See [FFI.md](FFI.md) for the C interop design.

---

## The 20-year rule

Stdlib candidates must be based on standards/protocols that have proven
themselves over decades:

| Protocol      | Year     | Age | Stdlib?                 |
| ------------- | -------- | --- | ----------------------- |
| TCP/UDP       | 1981     | 45  | Yes                     |
| SHA-2         | 2001     | 25  | Yes                     |
| HMAC          | 1996     | 30  | Yes                     |
| HTTP/1.1      | 1997     | 29  | Yes                     |
| TLS           | 1999     | 27  | Yes                     |
| JSON          | 2001     | 25  | Yes                     |
| OS randomness | OS-level | ∞   | Yes                     |
| bcrypt        | 1999     | 27  | No (algorithm-specific) |
| argon2        | 2015     | 11  | No                      |
| HTTP/2        | 2015     | 11  | No                      |
| QUIC          | 2021     | 5   | No                      |
| HTTP/3        | 2022     | 4   | No                      |

The language spec locks at 1.0. Post-1.0 changes are additive only.
Stdlib inclusion is a permanent commitment -- the 20-year rule ensures
we only commit to things that will still make sense in another 20 years.

---

## What stays external

- **HTTP/2 transport** -- package that adds HTTP/2 negotiation (ALPN),
  multiplexing. Reuses stdlib `Request`/`Response` types.
- **HTTP/3 / QUIC** -- operates over UDP, requires substantial userspace
  protocol implementation. Very different from TCP-based HTTP.
- **WebSocket** -- upgrade negotiation is complex, each connection is
  a process. Natural package.
- **Routing / middleware / frameworks** -- opinionated architectural
  choices that belong in packages.
- **Connection pooling** -- different strategies for different apps.
- **XML** -- complex (namespaces, DTDs, entities), declining usage in new
  systems. The `xmerl` cautionary tale from Erlang.
- **Password hashing** (`argon2`, `bcrypt`) -- algorithm-specific, evolves
  with computing power. First-party packages wrapping audited C libraries.
- **Structured logging, MessagePack, UUID, regex, URL parsing**

---

## Layer diagram

```
Random             OS entropy (bytes, int)    ← Global package, auto-imported
crypto.Hash        SHA-2 family          ← application-level crypto
crypto.HMAC        keyed message auth

net.Socket         raw POSIX primitives (create, bind, connect, etc.)
    |
net.TCPSocket      ergonomic TCP (connect, read, write, close)
net.UDPSocket      ergonomic UDP (bind, send_to, recv_from)
    |
    | .upgrade_tls(TLSConfig)    (calls OpenSSL/LibreSSL C API directly)
    v
net.TCPSocket      same type, now encrypted
    |
    +---> http.Client    one-shot HTTP/1.1 requests
    +---> http.Server    spawn-per-connection listener
              |
         http.Request / http.Response   shared vocabulary
```

`crypto` and TLS are siblings -- both wrap the same system C library
(OpenSSL's `libcrypto` and `libssl` respectively) but have no dependency
on each other at the Expo level.

Each layer builds on the one below. `HTTP.Server` uses `Net.TCPListener`;
`HTTP.Client` opens a `Net.TCPSocket`. The raw `Net.Socket` stays available
for exotic use cases but most code never touches it.

---

## Lesson from Elixir

Erlang/Elixir never defined a shared HTTP request/response type in stdlib.
The result: every library invented its own.

- `httpc` (Erlang stdlib) -- tuple-based, awkward API, everyone ignores it
- `hackney` -- own request/response types, pooling
- `httpoison` -- wrapper around hackney
- `Tesla` -- adapter abstraction over other client abstractions
- `mint` -- ultra-low-level protocol state machines
- `finch` -- combined mint + nimble_pool
- `req` -- high-level wrapper around finch

`Plug.Conn` eventually became the de facto server-side standard, but only
because Phoenix won the framework war -- not because of any stdlib decision.

If Expo's `http` package ships `Request` and `Response` from day one, this
fragmentation never happens. The stdlib defines the vocabulary; packages
define the behavior.
