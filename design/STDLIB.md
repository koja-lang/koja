# Standard Library Design

Design notes for Expo's standard library architecture: package hierarchy,
auto-import rules, networking stack, HTTP vocabulary types, and the 20-year
rule for stdlib inclusion.

---

## The problem

`std.*` is flat and overloaded. Adding networking, HTTP, TLS, and JSON to
the same flat namespace doesn't scale. `std.socket`, `std.http`, `std.tls`,
`std.json` all at the same level implies they're equally fundamental, but
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
- **Process types**: `Process`, `Ref`, `ReplyTo`, `Task`, `Lifecycle`,
  `StopReason`, `ExitStatus`, `ExitReason`
- **IO types**: `IO`, `File`, `Fd`, `System`, `DateTime`, `Duration`, `Debug`

### Qualified (domain-specific)

These types require a package-qualified path. Use `alias` to shorten:

```expo
alias net.TCPSocket
alias http.Request

conn = TCPSocket.connect("example.com", 443)
```

Without alias: `net.TCPSocket.connect(...)`, `http.Request`, `json.Decoder`.

This means the auto-import list is the "things that feel like the language"
list. Everything else uses the package-qualified path. The compiler knows
a fixed set of types to auto-import, regardless of which package they come
from.

---

## `net` package

Networking with TLS as a capability on `TCPSocket`, not a separate type.

### Types

- **`net.TCPSocket`** -- `connect(host, port)`, `read(count)`, `write(data)`,
  `close()`. TLS via `upgrade_tls(config)` (`move self -> Self`) or the
  convenience factory `connect_tls(host, port, config)`. One type handles
  both plain and encrypted connections.
- **`net.TCPListener`** -- `bind(port)`, `accept()` (returns `TCPSocket`).
  Same type for server and client connections.
- **`net.UDPSocket`** -- `bind(port)`, `send_to(host, port, data)`,
  `recv_from(count)`. Datagram-oriented, no connection ceremony.
- **`net.TLSConfig`** -- certificate options, verification settings. Passed
  to `upgrade_tls` or `connect_tls`.
- **`net.Socket`** -- raw POSIX socket primitives (current `std.socket`
  internals). Rarely used directly -- `TCPSocket` and `UDPSocket` wrap it.

### TLS as upgrade, not separate type

A separate `TLSSocket` type would force users to decide upfront whether
they want encryption, duplicate `read`/`write`/`close` across types, and
require every socket-accepting function to handle `TCPSocket | TLSSocket`.

Instead, TLS is a capability added to `TCPSocket`:

```expo
alias net.TCPSocket
alias net.TLSConfig

conn = TCPSocket.connect("api.example.com", 443)
conn = conn.upgrade_tls(TLSConfig.default())
conn.write(request_bytes)
```

This mirrors how TLS actually works at the protocol level -- you start with
a TCP connection and negotiate TLS on top. The `move self -> Self` idiom
makes this feel natural in Expo.

### Implementation

`net.TCPSocket` and friends are built entirely in pure Expo on top of
`net.Socket` (the raw POSIX primitives). TLS wraps a system library
(LibreSSL/OpenSSL/BoringSSL) via C FFI. Programs that don't call
`upgrade_tls` don't pull in TLS dependencies.

---

## `http` package

Shared vocabulary types and HTTP/1.1 baseline. The primary goal is
preventing the ecosystem fragmentation that plagued Elixir -- where httpc,
hackney, httpoison, Tesla, mint, finch, and req all invented their own
request/response types.

### Vocabulary types

These are protocol-version-agnostic. A `Request` looks the same whether
it came over HTTP/1.1, HTTP/2, or HTTP/3:

- **`http.Request`** -- method, path, headers, body
- **`http.Response`** -- status code, headers, body
- **`http.Method`** -- enum: `Get`, `Post`, `Put`, `Delete`, `Patch`,
  `Head`, `Options`
- **`http.Status`** -- enum or int with named constants (`Ok`, `NotFound`,
  `InternalServerError`, etc.)
- **`http.Headers`** -- header collection (likely `Map<String, List<String>>` or
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

## `json` package

Promote the existing `json` package to stdlib status. Already implemented
in pure Expo with 17 tests covering encoder and decoder.

- `json.JSONValue` enum (recursive descent)
- `json.Encoder` (compact and pretty-printed)
- `json.Decoder` (planned: combinator API with error accumulation)

---

## The 20-year rule

Stdlib candidates must be based on standards/protocols that have proven
themselves over decades:

| Protocol | Year | Age | Stdlib? |
| -------- | ---- | --- | ------- |
| TCP/UDP  | 1981 | 45  | Yes     |
| HTTP/1.1 | 1997 | 29  | Yes     |
| TLS      | 1999 | 27  | Yes     |
| JSON     | 2001 | 25  | Yes     |
| HTTP/2   | 2015 | 11  | No      |
| QUIC     | 2021 | 5   | No      |
| HTTP/3   | 2022 | 4   | No      |

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
- **Crypto** -- password hashing, HMAC, random bytes. Thin FFI wrappers
  over audited C libraries (libargon2, libsodium).
- **Structured logging, MessagePack, UUID, regex, URL parsing**

---

## Layer diagram

```
net.Socket         raw POSIX primitives (create, bind, connect, etc.)
    |
net.TCPSocket      ergonomic TCP (connect, read, write, close)
net.UDPSocket      ergonomic UDP (bind, send_to, recv_from)
    |
    | .upgrade_tls(TLSConfig)
    v
net.TCPSocket      same type, now encrypted
    |
    +---> http.Client    one-shot HTTP/1.1 requests
    +---> http.Server    spawn-per-connection listener
              |
         http.Request / http.Response   shared vocabulary
```

Each layer builds on the one below. `http.Server` uses `net.TCPListener`;
`http.Client` opens a `net.TCPSocket`. The raw `net.Socket` stays available
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
