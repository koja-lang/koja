# HTTP: A Server, a Client, and One HTTP/2 Core

**Status: draft (2026-11-12).** This document argues a position for
how Koja serves and sends HTTP in production. The standard library
keeps the vocabulary types and the HTTP/1.1 codec. Three packages
carry the rest. `ranger` is the server, `scout` is the client, and
`http2` is the HTTP/2 wire protocol that both of them use. `ranger`
exists today as a vendored package inside remem. `scout` and `http2`
do not exist yet. This document is the contract for that work and for
the standard library changes that must land first.

## Summary

- The standard library `HTTP` package owns the vocabulary types and
  the HTTP/1.1 codec. Bodies are bytes, a request carries a `URI`,
  and the parser takes a buffer that is not yet complete.
- One package, `http2`, owns the HTTP/2 wire protocol. A connection
  is a value that takes bytes and returns events. The package owns
  no sockets and no processes, and it holds no HTTP/1.1 code.
- `ranger` is the server. It owns the listener, one worker for each
  connection, and the drain at shutdown. It starts with HTTP/1.1 and
  cleartext HTTP/2 on one port.
- `trail` keeps its design. A handler stays
  `fn (Request) -> Response` on both protocols, and each request
  runs in its own process.
- `scout` is the client. It pools HTTP/1.1 connections through
  `pooler` and shares HTTP/2 connections between callers. It picks
  the protocol through ALPN from its first release.
- `pooler` gains a fill option, idle expiry, and borrower
  monitoring.
- A handler that answers 101 gives its connection to the
  application, and `scout` can ask for the same on the client side.
  A `websocket` package builds on both and gets its own document.
- Bodies are buffered in this design. The `http2` core handles body
  data as chunks, so streaming can land above it later.
- HTTP/2 over TLS on the server waits for an ALPN callback shim.
- The standard library changes and `http2` can start first. Seven
  positions in the `ranger` and `scout` sections are still open.

The dependencies point one way:

```
trail   -> HTTP
http2   -> HTTP
ranger  -> HTTP, Net, http2
scout   -> HTTP, Net, http2, pooler
```

## What exists today

Reading `lib/http`, `lib/net`, `trail`, `pooler`, and remem's
`lib/ranger` at Koja 0.20.1 showed this.

### The pieces

- **Standard library `HTTP`.** `Request`, `Response`, `Headers`,
  `Method`, and `Status`, a `Parser` for complete HTTP/1.1 messages,
  and a one-shot client (`HTTP.get`, `HTTP.post`, `HTTP.request`).
- **Standard library `Net`.** `TCPSocket`, `TCPListener`, the
  `TCPServer` acceptor process, and TLS on BoringSSL. Every wait
  takes a per-call timeout.
- **`trail` 0.1.0.** Routes an `HTTP.Request` to an `HTTP.Response`.
  It owns no sockets.
- **`pooler` 0.1.0.** A fixed-size resource pool behind one process.
- **`ranger` 0.1.0**, vendored at `lib/ranger` in remem. One `Worker`
  process owns one accepted socket. It reads HTTP/1.1 requests with
  keep-alive, runs the handler inline, and writes the response.

The application owns the accept loop. remem's `App` spawns a
`Net.TCPServer` and spawns a `Worker` for each `Connected` event.
The shortener example in this repo carries its own copy of the
worker. Every Koja service writes this loop again.

remem is also the first client. Its embedder and its OpenTelemetry
exporter both call `HTTP.post`.

### What hurts

The vocabulary types have the shape of HTTP/1.1 text.

- `Request.body` and `Response.body` are `String`. A body that is not
  UTF-8 has no type to live in. `ranger` answers 400 to one, and the
  client fails the whole request.
- `version` is a `String`, `Status` carries a reason phrase, and
  `Response.serialize` writes an HTTP/1.1 message. HTTP/2 has no
  reason phrase and no text form.
- `Request` has a path and no scheme or authority. HTTP/2 sends both.
- `Method` is a closed enum of seven methods. A request with any
  other method fails to parse, so the server answers 400.

The parser counts characters where the wire counts bytes.

- `Parser` slices a `Content-Length` body and each chunk by codepoint
  index. A chunked body with a multibyte character is cut in the
  wrong place.
- `Response.new` sets `Content-Length` from `body.length()`, which is
  codepoints. The header is wrong for any body that is not ASCII.
- `Parser` takes a complete message. It cannot say that it needs
  more bytes, so a caller must frame the message first.

`ranger` works around the parser and still has gaps.

- `Frame` exists only to cut a request out of the byte stream before
  `Parser` sees it. It reads `Content-Length` and nothing else.
- A chunked request body is not read. The handler sees an empty body
  and the worker reads the chunk bytes as the next request.
- No read has a deadline. A client can hold a connection open and
  never send a complete request.
- A shutdown cuts requests in flight. `App` stops on SIGTERM, and a
  parent death kills every worker.
- The README blames the last two on the standard library. Since 0.20
  that is out of date. Reads take a timeout, and `TCPServer` closes
  its listener when it gets a shutdown signal.

The one-shot client is correct for a script and for little else.

- Each request opens a connection, sends `Connection: close`, and
  reads to end of stream.
- No call takes a timeout. `HTTP.Error.Timeout` exists and nothing
  produces it.
- The response is read with `read_string` in 8192-byte reads. A
  multibyte character cut at a read boundary fails the request.
- Every failure below the parser is `ConnectionFailed(String)`. A
  caller cannot tell a refused connection from a TLS failure.

`Net` lacks four things that HTTP needs.

- **ALPN.** `TLSConfig` cannot offer or select a protocol, so no TLS
  connection can agree on HTTP/2. The client side is two plain calls
  into BoringSSL. The server side takes a C callback, and Koja FFI
  cannot pass one.
- **`TCP_NODELAY`.** `Socket` sets `SO_REUSEADDR` and no other
  option. HTTP/2 sends many small frames, and Nagle's algorithm
  delays them.
- **`write_all`.** `IO.Writer.write` can write part of its input.
  `ranger` carries its own loop, and the one-shot client does not
  loop at all.
- **A buffered reader.** Each consumer keeps its own leftover bytes.

## Standard library changes

These land first, in `lib/http` and `lib/net`. Every package below is
written against the types as this section leaves them. The changes
break `trail`, remem, and the shortener example once, in one release.

### Vocabulary

The types stop describing HTTP/1.1 text and describe a message. A
`Request` or a `Response` reads the same whether it came off an
HTTP/1.1 connection or an HTTP/2 stream.

```koja
struct Request
  body: Binary
  headers: Headers
  method: Method
  uri: URI
  version: Version = Version.HTTP11
end

struct Response
  body: Binary
  headers: Headers
  status: Status
  version: Version = Version.HTTP11
end
```

#### Bodies are bytes

`body` is `Binary` on both types. JSON, an image, and a compressed
stream all fit. `text()` decodes the body as UTF-8 and fails with
`String.ConversionError`, so the decode happens once, in the code
that wants text.

Building a message stays as short as it is today. `Response.new` and
the client functions take `Binary | String` for the body, the way
`IO.Writer.write` does, and store bytes.

```koja
response = Response.new(Status.ok(), "hello")
name = try request.text()
```

`body_as_binary` goes away, since the body is the binary.
`Response.new` sets `Content-Length` from the byte size.

#### `uri` replaces `path`

`Request.uri` is a `URI`. It carries the scheme, the host, the port,
the path, and the query, which is what HTTP/2 sends as `:scheme`,
`:authority`, and `:path`. A `URI` holds wire-form text, so nothing
is decoded early. `uri.path` and `uri.query` are what `path` held
before, already split.

- On an HTTP/1.1 server, the parser fills the path and the query
  from the request target, and the host and the port from the `Host`
  header. The server sets the scheme from the connection.
- On an HTTP/2 server, `http2` fills every part from the pseudo
  headers.
- On a client, the URL the caller gave is the `uri`. The writer sends
  the path and the query as the target, and the host as `Host` or as
  `:authority`.

A handler reads `request.uri.host` on both protocols. The `Host`
header stays in `headers` when the client sent one.

Two request targets are not URIs. The parser stores the `*` of
`OPTIONS *` as the path, and it reads a `CONNECT` target as a host
and a port. `fragment` and `userinfo` never go on the wire, and a
writer ignores them.

#### `Method` has ten variants

```koja
enum Method
  Connect
  Delete
  Get
  Head
  Options
  Patch
  Post
  Put
  Query
  Trace
end
```

`Connect` and `Trace` complete the RFC 9110 set. `Query` is the safe
method with a body, for a search that does not fit in a URL.

The enum stays closed. A `match` on it needs no wildcard arm, and
`trail` can still name every method in an `Allow` header. An unknown
method is its own parser error, so a server answers 501 where it
answers 400 today.

#### `Status` is a code

```koja
struct Status
  code: Int

  fn reason(self) -> String
end
```

The reason phrase is no longer a field. RFC 9112 tells a client to
ignore it, and HTTP/2 does not send one. `reason()` looks the phrase
up by code and returns `""` for a code it does not know.
`Status.new` takes the code alone, and two statuses with the same
code are equal.

The named constructors stay. `Response.serialize` writes `reason()`
on the status line, and the parser drops the phrase a server sent.

#### `Version` is an enum

```koja
enum Version
  HTTP10
  HTTP11
  HTTP2
end
```

The field says what a received message arrived as. A writer ignores
it, because the connection decides the wire version and the message
does not. The default is `HTTP11`, so code that builds a message
never names it.

#### `Headers` does not change

Lookups already ignore case. HTTP/2 requires lowercase names on the
wire, and `http2` lowercases them when it encodes.

### The HTTP/1.1 codec

`HTTP.Parser` stays the reader and `serialize` stays the writer.
Both move from text to bytes, and the parser learns to say that it
needs more.

#### `Parser` is incremental

```koja
struct Parser.Limits
  max_body_bytes: Int = 1048576
  max_header_bytes: Int = 16384
end

enum Parser.Outcome<T>
  Complete((T, Binary))
  Incomplete
end

enum Parser.Error
  BodyTooLarge
  HeadersTooLarge
  Malformed(String)
  UnsupportedMethod(String)
end

fn parse_request(buffer: Binary, limits: Parser.Limits)
  -> Parser.Outcome<Request> ! Parser.Error
```

The caller reads from its socket, appends to a buffer, and calls
`parse_request` with all of it. `Incomplete` means read more.
`Complete` carries the message and the bytes that came after it,
which are the start of the next message on a keep-alive connection.
This is the contract `ranger`'s `Frame` has today, so `Frame` goes
away.

Each error is one response a server sends. `BodyTooLarge` is 413,
`HeadersTooLarge` is 431, `Malformed` is 400, and
`UnsupportedMethod` is 501. The parser checks the limits against the
declared length, so it rejects a large body before the bytes arrive.

`parse_response` has the same shape with two more inputs. It takes
the request method, because the response to a `HEAD` has no body. It
also takes a flag that says the stream ended, because a response
with no length runs to the end of the stream.

A response with a 1xx status has no body, so it is complete at the
end of its head. The parser returns it like any other response. The
bytes after a 101 belong to the next protocol, and they come back as
the leftover bytes. A client drops any other 1xx and parses the next
response from the leftover bytes.

The parser works on bytes from end to end.

- It finds the end of the head by byte search and decodes the head
  as UTF-8. A head that does not decode is `Malformed`.
- It cuts a `Content-Length` body and each chunk by byte count.
- It decodes a chunked body into one `Binary` and counts the total
  against `max_body_bytes`. It reads trailers and drops them.

The parser is strict where a loose reading lets two parties disagree
about where a message ends.

- A `Content-Length` that is not a number is `Malformed`. Today it
  counts as zero.
- Two `Content-Length` headers with different values are
  `Malformed`.
- A message with both `Content-Length` and `Transfer-Encoding` is
  `Malformed`.

The parser keeps no state between calls, so each call scans the head
again. The head is at most `max_header_bytes`, which bounds the
cost. A parser that holds its position across calls is faster for a
body that arrives in many reads. It can replace this one behind the
same functions if a benchmark asks for it.

#### `serialize` returns bytes

`Response.serialize` returns a `Binary`, and `Request.serialize`
joins it. The request form writes the path and the query as the
target and adds `Host` from the `uri` when the headers have none.
Both set `Content-Length` from the byte size of the body when the
header is absent. Neither writes a chunked body, because every body
in this design is complete before the write starts.

The `Connection` header is policy, so the caller sets it.

### The one-shot client stays

`HTTP.get`, `HTTP.post`, and the rest stay in the standard library.
A `.kojs` script has no dependencies, so a script that fetches a URL
needs a client that ships with the compiler. It stays one connection
for each request, with no pool and no HTTP/2. A program that makes
more than a few requests uses `scout`.

The client is rebuilt on the pieces above.

- It writes with `write_all` and reads bytes into the incremental
  parser, so a binary body and a multibyte character both survive.
- The body parameters take `Binary | String`.
- Every function takes a trailing
  `timeout: Option<Duration> = Option.None`. It bounds the whole
  request, from the connect to the last byte of the response.
  `HTTP.Error.Timeout` gets its first producer.

`HTTP.Error` keeps its name, and its variants carry causes in place
of strings.

```koja
enum Error
  Connection(SocketError | IOError | TLSError)
  InvalidUrl(String)
  Parse(Parser.Error)
  Timeout
end
```

A caller can then tell a refused connection from a certificate that
failed to verify.

### `Net`

#### ALPN

`TLSConfig` gains a protocol list, and a connected socket reports
what the handshake selected.

```koja
config = TLSConfig.client().with_alpn(["h2", "http/1.1"])
socket = try TCPSocket.connect_tls_with(host, 443, config, timeout)
protocol = socket.alpn().or("http/1.1")
```

`with_alpn` takes the protocol names in order of preference. The
default is an empty list, which sends no ALPN extension, so nothing
changes for a caller that does not ask. `TCPSocket.alpn()` returns
`Option.None` on a cleartext socket and when the peer selected
nothing.

The names are strings because the ALPN registry is open. A closed
enum would make every new protocol wait for a compiler release.

The client half needs no callback. It sets the list on the context
before the handshake and reads the selection after it. The server
half does. BoringSSL asks a server to choose through
`SSL_CTX_set_alpn_select_cb`, and Koja FFI cannot pass a function to
C. The fix is one runtime entry point that takes the server's list
and installs a fixed callback. The callback picks the first protocol
in the server's list that the client also offers.

`with_alpn` is the same call on both sides, so the shim changes no
signature when it lands. The client half ships first.

#### `TCP_NODELAY`

Every TCP connection has `TCP_NODELAY` set, from `connect` and from
`accept`. `TCPSocket.set_nodelay(false)` turns it off.

This is Go's default, and it fits how Koja programs write. A process
builds a whole message, a response or a frame or a query, and writes
it once. Nagle's algorithm adds nothing to that pattern except a
delay when a small write waits for an acknowledgment. The Postgres
driver gets the same fix with no change of its own.

[IO.md](IO.md) expected a `TCPListener.Options` for options that an
accepted connection inherits. A default needs no options struct, so
that type stays out until an option needs a choice at the listener.

#### `write_all`

IO.md left `write_all` as an open question that waited for a caller.
HTTP is the caller.

```koja
protocol IO.Writer<E>
  fn write(
    self,
    data: Binary | String,
    options: WriterOptions = WriterOptions{},
  ) -> Int ! E

  @doc """
  Writes all of `data`. Calls `write` until every byte is out.
  """
  fn write_all(
    self,
    data: Binary | String,
    options: WriterOptions = WriterOptions{},
  ) ! E
    # loop on write, advance past the bytes each call accepted
  end
end
```

It is a default body, so every writer gets it. `options.timeout`
bounds each `write`, not the whole call, which is the rule IO.md set
for every wait. A failure can follow a partial write.
`ranger`'s private `write_all` goes away, and the one-shot client
stops trusting one `write` to send a whole request.

#### No buffered reader

HTTP does not need one. The parser above takes the bytes a caller
has so far and hands back what it did not use. The buffering
question in IO.md stays open on its own terms.

## The `http2` package

`http2` is the HTTP/2 wire protocol from RFC 9113 and the header
compression from RFC 7541. Its namespace is `HTTP2`. It depends on
the standard library `HTTP` package and on nothing else.

### A connection is a value

The package opens no socket, spawns no process, and reads no clock.
A connection is a value. Its owner gives it the bytes that arrived,
and it gives back events and the bytes to send.

```koja
conn = ServerConnection.new(Settings{})

(conn, events) = conn.receive(bytes)
conn = try conn.respond(stream, response)
(conn, output) = conn.take_output()
```

Every function returns the next connection, as every Koja value
does. The owner writes `output` to its socket when it chooses to.

This shape is what lets one package serve both `ranger` and `scout`.
The two differ in how they own sockets and processes, and the
protocol between the bytes and the events is the same. It also makes
the protocol testable with no network. A test feeds a client's
output into a server and the server's output back.

### Two connection types

`ClientConnection` and `ServerConnection` are separate public types
over one private core. A client sends requests and a server sends
responses, so each type has only the functions its end can call.
Sending a request from the server end is a compile error, not a
runtime check.

```koja
struct ClientConnection
  fn new(settings: Settings) -> ClientConnection
  fn request(self, request: Request)
    -> (ClientConnection, StreamID) ! Error
end

struct ServerConnection
  fn new(settings: Settings) -> ServerConnection
  fn respond(self, stream: StreamID, response: Response)
    -> ServerConnection ! Error
end
```

Both types share the rest.

| Function              | Effect                                   |
| --------------------- | ---------------------------------------- |
| `receive(bytes)`      | Take bytes from the peer, return events  |
| `take_output()`       | Return the bytes to send and clear them  |
| `reset(stream, code)` | Cancel one stream                        |
| `ping()`              | Queue a PING                             |
| `shut_down()`         | Queue a GOAWAY and accept no new streams |

`StreamID` is its own type, so a stream cannot be confused with any
other integer. `HTTP2.PREFACE` is the 24-byte client preface, which
a server reads to tell HTTP/2 from HTTP/1.1.

### Events

Each connection type has its own event enum, nested under it.

```koja
enum ServerConnection.Event
  Closed(ErrorCode)
  Data((StreamID, Binary))
  End(StreamID)
  Pong
  Request((StreamID, Request))
  Reset((StreamID, ErrorCode))
  UnsupportedMethod((StreamID, String))
end

enum ClientConnection.Event
  Closed(ErrorCode)
  Data((StreamID, Binary))
  End(StreamID)
  Pong
  Reset((StreamID, ErrorCode))
  Response((StreamID, Response))
end
```

`Request` and `Response` carry the head of a message with an empty
body. The body follows as `Data` events, and `End` says the peer
finished the stream. `Reset` says the stream is gone and no more
events come for it. `Closed` says the same for the connection.

`UnsupportedMethod` is a request whose method is not in `Method`.
The connection cannot build a `Request` for it, so the owner answers
it. It matches `Parser.Error.UnsupportedMethod` on the HTTP/1.1
side.

### Sending is whole messages for now

`request` and `respond` take a complete message, body included. The
connection cuts the body into DATA frames no larger than the peer
allows. It holds back the bytes that the peer's flow control window
does not cover and queues them as the window opens.

The functions that send a head, then chunks, then an end land with
streaming. They add to this surface and change none of it.

### `Collector` buffers a body once

Both `ranger` and `scout` want a complete message, and both would
write the same code to build one from `Data` events. `Collector`
is that code.

```koja
collector = collector.start(stream, head)
collector = try collector.append(stream, chunk)
(collector, message) = collector.finish(stream)
```

A `Collector<T>` holds the unfinished messages of one connection by
stream. `append` fails when a body passes `max_body_bytes`, and the
owner then resets the stream or answers 413. `finish` returns the
head and the body together. A `Reset` drops the stream's entry.

The collector is the only part of the package that assumes a
buffered body. A streaming owner skips it and reads the `Data`
events itself.

The connection returns flow control credit as soon as `receive`
hands the bytes over. That is correct while the owner buffers,
because the body cap bounds the memory. A streaming owner must say
when it has used the bytes, and that function lands with streaming.

### What is inside

One type for each file, none of them public.

- **`Frame`.** Encodes and decodes the ten frame types. The 9-byte
  frame header is one binary pattern.
- **`HPACK`.** The static table, the dynamic table, integer and
  string coding, and the Huffman code. It is new code. Kadabra used
  an Erlang library for this.
- **`Window`.** Flow control arithmetic for a connection and for a
  stream.
- **`Stream`.** The state machine from RFC 9113 section 5.1.
- **The core.** Settings exchange, the stream table, and the output
  queue.

```koja
match buffer
  <<length::24, kind::8, flags::8, _reserved::1, stream::31,
    rest: Binary>> ->
    # read `length` bytes of payload from `rest`
  _ ->
    # fewer than 9 bytes, wait for more
end
```

HPACK stays private because nothing else would use it. HTTP/3
compresses headers with QPACK, which is a different format.

### Mapping to `Request` and `Response`

HTTP/2 carries the request line and the status line as pseudo
headers. The package converts in both directions, so its owners see
only `HTTP.Request` and `HTTP.Response`.

- `:method`, `:scheme`, `:authority`, and `:path` map to `method`
  and `uri`. `:status` maps to `status`.
- Header names are lowercased when they are sent.
- `Connection`, `Keep-Alive`, `Proxy-Connection`,
  `Transfer-Encoding`, and `Upgrade` have no meaning in HTTP/2. The
  connection drops them when it sends. A peer that sends one gets a
  stream error.
- A client sends the host as `:authority` and drops `Host`.
- The pieces of a split `cookie` header are joined when they arrive.
- A `content-length` that does not match the DATA bytes is a stream
  error.
- Trailers are decoded, because HPACK state depends on them, and
  then dropped. Responses with a 1xx status are dropped as well.

### Settings and limits

`Settings` holds what this end announces to its peer.

```koja
struct Settings
  header_table_size: Int = 4096
  initial_window_size: Int = 65535
  max_concurrent_streams: Int = 100
  max_frame_size: Int = 16384
  max_header_list_size: Int = 16384
end
```

A peer can also attack with frames that every setting allows. The
connection counts these and closes with `ENHANCE_YOUR_CALM` when a
count passes its cap.

- Streams that the peer opens and resets at once (the 2023 rapid
  reset attack).
- PING and SETTINGS frames that each demand a reply.
- CONTINUATION frames that grow one header block past
  `max_header_list_size`.

### Errors

`receive` does not fail. When the peer breaks the protocol, the
connection still has a frame to send, and a failed call would lose
the value that holds it.

- A stream error queues RST_STREAM and returns a `Reset` event.
- A connection error queues GOAWAY and returns a `Closed` event.
  The owner writes the output and closes the socket.

The functions that send do fail, with an `Error` that says why the
connection cannot take the message.

```koja
enum Error
  Closed
  GoingAway
  StreamClosed
  StreamsExhausted
  TooManyStreams
end
```

`TooManyStreams` means the peer's stream limit is reached, so a
client waits or uses another connection. `GoingAway` and
`StreamsExhausted` mean this connection takes no new streams, so a
client opens a new one.

`ErrorCode` is the fourteen codes from RFC 9113 section 7 and an
`Unknown(Int)`. When a peer sends GOAWAY, each stream it did not
process arrives as a `Reset` with `RefusedStream`. A client can send
those requests again on a new connection.

### Left out

- **Server push.** A client connection announces `ENABLE_PUSH = 0`,
  and a server connection never pushes. Browsers removed support
  for it.
- **Priority.** RFC 9113 deprecated the priority scheme. PRIORITY
  frames are read and ignored.
- **Timers.** The package has no clock. The owner decides when to
  send a PING and how long to wait for the `Pong`.

## Ranger

`ranger` grows from a connection worker into a server. It owns the
listener, the workers, and the shutdown. An application gives it a
handler and a port.

Five positions in this section are proposals and not decisions. Each
is marked **Open**, and the open questions at the end say what would
settle it. None of them is taken from how remem works today.

### `Server` is a handle

```koja
server = try Server.start(Config{handler: handler, port: 8080})

# later, from the application's shutdown path
try server.stop(timeout)
```

`Server.start` binds the port in the calling process and then spawns
the listener process. A port that is in use fails `start` with a
`SocketError`, at once, where the caller can see it. `stop` drains
the server and returns when the drain is done.

This is the shape of `Pool.start` and `pool.stop`. The application
stays an ordinary process, so it decides the shutdown order. It
stops the server first and the database pool after.

**Open.** The other shape is a blocking `serve` that runs the accept
loop in the calling process. It can be built on the handle later.

### The listener process

The listener owns the `TCPListener`. It watches the descriptor,
accepts every pending connection on each wakeup, and spawns one
worker for each. `Net.TCPServer` does the same and then sends each
socket to an owner. `ranger` drops that hop.

The listener monitors every worker, so it knows how many are alive.
At `max_connections` it stops watching the descriptor until a worker
exits. New connections wait in the kernel backlog. A refusal that
needs a process for each connection would defeat the cap.

**Open.** Stop accepting is the Ranch behavior. The other choices
accept the connection and then close it, with or without a 503.

### A worker picks the protocol

A worker owns one connection from accept to close.

1. If the server has a `TLSConfig`, the worker runs the TLS
   handshake under a deadline. The listener never waits for a
   handshake.
2. The worker reads the first bytes. If they are the start of
   `HTTP2.PREFACE`, the connection is HTTP/2. If they are not, it is
   HTTP/1.1.

The preface starts with `PRI`, which is no HTTP/1.1 method, so a few
bytes decide. One port serves both protocols.

On a TLS connection the ALPN result decides, once the server shim
exists. Until then a TLS connection is HTTP/1.1.

### One handler model

The handler stays `fn (Request) -> Response`. `trail` gives it one
through `router.dispatch`, and it does not know which protocol the
request came over.

Each request runs in its own process on both protocols. The worker
spawns the process, monitors it, and waits for one of two messages.

- The response. The worker writes it.
- An exit signal with a crash. The worker answers 500 and reports
  the panic message and the backtrace through a hook.

Koja cannot catch a panic inside a process. A process boundary is
the only way a handler crash becomes a 500 and a log line. With the
handler inline, a panic kills the worker, the client sees a dropped
connection, and the access log shows nothing. `koja test` isolates
each test the same way for the same reason.

HTTP/2 needs the process in any case. Its worker must keep reading
frames while handlers run. Using it on HTTP/1.1 as well gives one
model to document, test, and reason about.

The runtime copies the process context and the log configuration at
`spawn`. A trace and a log line follow the request into its process
with no code in `ranger`.

The cost is one spawn for each request, and three copies. The
handler closure and the request copy into the process, and the
response copies back. A spawn makes two system calls today.
[RUNTIME-GAPS.md](RUNTIME-GAPS.md) plans stack pooling, which will
remove them.

The boundary reports a crash. It does not clean up after one. A
handler that panics while it holds a pool lease or an open socket
leaks it, the same as a killed process does.

**Open.** The other choice keeps the handler inline on HTTP/1.1 and
accepts that a panic drops the connection.

### The HTTP/1.1 worker

The worker keeps a buffer and loops.

1. Read into the buffer until `Parser.parse_request` returns
   `Complete`.
2. Spawn the handler and wait for its response.
3. Write the response with `write_all`.
4. If the connection stays open, keep the leftover bytes and go to
   step 1.

Three deadlines bound a slow client.

| Setting         | Default | Bounds                                     |
| --------------- | ------- | ------------------------------------------ |
| `idle_timeout`  | 60 s    | The wait for the first byte of a request   |
| `read_timeout`  | 30 s    | The first byte of a request to its last    |
| `write_timeout` | 30 s    | Each write of a response                   |

A request that passes `read_timeout` gets a 408. A `Parser.Error`
gets the status that the codec section lists. Both go through
`error_response` and `on_reject`, and both close the connection.

The rules that `ranger` has today stay. `Connection: close` and
HTTP/1.0 end the connection, a connection serves at most
`max_requests_per_connection` requests, and pipelined bytes wait in
the buffer. A response to a `HEAD` request goes out with no body.

### Upgrade

Some protocols start as an HTTP/1.1 request and then take the
connection. WebSocket is the one that matters. `ranger` has one path
for all of them and names none of them.

A handler asks for an upgrade when it answers 101. The handler type
does not change. `trail` routes the request and its middleware runs,
so an upgrade route gets the same authentication as any other route.

```koja
struct Upgrade
  leftover: Binary
  request: Request
  response: Response
  socket: TCPSocket
end

config = Config{handler: handler, port: 8080}
config = config.with_on_upgrade(&Chat.join/1)
```

When a handler answers 101, the worker does four things.

1. It checks that the request asked for an upgrade and that the
   server has an `on_upgrade` function. If not, it answers 500.
2. It writes the 101.
3. It calls `on_upgrade` with the `Upgrade`, in the worker process.
4. When the function returns, the worker exits. It does not close
   the socket.

From step 3 the socket belongs to the function. `leftover` holds the
bytes that the client sent after its request. They are the first
bytes of the new protocol.

The function can use the socket in two ways.

- **Run the session in place.** The function loops on the socket
  until the session ends. The worker process is the session. This
  fits a session that only answers its peer. The connection still
  counts toward `max_connections`, and the shutdown signal still
  reaches it.
- **Hand the socket to an application process.** The function sends
  the `Upgrade` to a process that the application owns, and returns.
  That process spawns the session as its child. The session has the
  application's message type, so it can receive a broadcast or a
  timer as well as `IO.Ready`.

A session that needs its own mailbox cannot be a child of the
worker. A child dies with its parent, and the worker is about to
exit.

After a handoff the connection is outside `ranger`. `ranger` does
not count it, drain it, or close it. The application owns its limit
and its shutdown.

A handoff copies the socket across a process boundary.
`Net.TCPServer` does this today with every socket that it accepts. A
TLS session crosses as a pointer, which is safe because the worker
does not touch the socket again. No test covers a TLS session that
moves this way.

The handler runs in the request process and `on_upgrade` runs in the
worker. The function gets the request and the response and nothing
else from the handler. A session that needs the caller's identity
reads it from the request again.

HTTP/2 has no 101. A handler that answers 101 on an HTTP/2
connection gets a 500. WebSocket over HTTP/2 uses a different
mechanism, which the Later section describes.

**Open.** Two other shapes exist. A separate hook can decide before
the handler runs, which takes the route out of `trail`. The handler
can return `Response | Upgrade`, which changes the type that `trail`
produces. The 101 signal is the one shape that changes neither.

### The HTTP/2 worker

The worker holds the socket, a `ServerConnection`, and a
`Collector`. Its mailbox carries three kinds of message.

- `IO.Ready`, when the socket has bytes.
- A response from a handler process.
- An exit signal from a handler process.

On `IO.Ready` the worker reads until a read with a zero timeout
finds nothing. A TLS session can hold decrypted bytes that the
descriptor does not show, so one read for each wakeup is not enough.
It gives the bytes to `receive`, acts on the events, writes
`take_output()`, and watches the descriptor again.

| Event               | The worker                                    |
| ------------------- | --------------------------------------------- |
| `Request`           | Starts a collector entry                      |
| `Data`              | Appends, or answers 413 past the body cap     |
| `End`               | Finishes the entry and spawns the handler     |
| `UnsupportedMethod` | Answers 501                                   |
| `Reset`             | Drops the entry, marks the stream as dropped  |
| `Closed`            | Stops reading                                 |

A response from a handler goes to `respond`. A crash is a 500 on
that stream, and the other streams on the connection do not notice.

Every write has `write_timeout`. A write blocks the worker, and a
blocked worker reads nothing. A peer that does not read for that
long loses the connection.

When a connection has no open stream for `idle_timeout`, the worker
calls `shut_down()` and closes.

### A canceled request runs to its end

A client can reset one stream, or drop the connection, while
handlers run. The worker does not kill them. Each handler runs to
its end and the worker drops its response.

A killed Koja process runs no cleanup. A socket that it opened stays
open, a TLS session that it holds is never freed, and a pool lease
that it borrowed is never returned. Until a kill can clean up, the
only safe cancel is no cancel.

Two rules keep this from becoming an attack.

- A stream counts toward `max_concurrent_streams` until its handler
  exits, not until the stream closes. A client that resets and opens
  again gets `RefusedStream`. It cannot run more handlers than the
  limit.
- A worker whose socket is gone stays alive until its handlers exit.
  It still counts toward `max_connections`.

`ranger` sets no deadline on a handler, because a deadline would
need a kill. The handler's own bounds apply: a pool checkout
timeout, a `scout` request timeout.

**Open.** Killing is the better answer once it is safe. That needs
either cleanup on kill in the runtime, or resource owners that
monitor their borrowers.

### Shutdown

`server.stop(timeout)` drains in three steps.

1. The listener closes the listening socket.
2. The listener sends every worker a shutdown signal.
3. `stop` returns when every worker has exited. If `timeout` passes
   first, it fails with a timeout and the caller decides.

A worker that gets the signal finishes what it has and takes nothing
new.

- An HTTP/1.1 worker between requests closes at once. A read that is
  parked on an idle connection returns with `IOError.Interrupted`.
  A worker with a request in progress answers it with
  `Connection: close` and then closes.
- An HTTP/2 worker calls `shut_down()`, which sends GOAWAY. It
  answers the streams that have handlers and then closes.

The runtime adds two rules that `ranger` works inside.

- After SIGTERM the runtime refuses every `spawn`. A request that
  finishes arriving after the signal cannot get a handler. It gets a
  503 on HTTP/1.1 and `RefusedStream` on HTTP/2. Both tell a client
  that the request is safe to send again.
- The runtime kills what is left after `KOJA_GRACE_MS`, 30 seconds
  by default. The `timeout` given to `stop` must be shorter.

The application must call `stop` before it returns from its own
shutdown. A process that exits takes its children with it, and the
server is a child.

### Configuration

`Config` keeps its chainable `with_` functions and its three hooks,
and gains two hooks, `on_crash` and `on_upgrade`. `handler` and
`port` have no default. Every default below that `ranger` does not
have today is a first value to tune.

| Field                         | Default    |
| ----------------------------- | ---------- |
| `error_response`              | plain text |
| `idle_timeout`                | 60 s       |
| `max_body_bytes`              | 1 MiB      |
| `max_concurrent_streams`      | 100        |
| `max_connections`             | 10000      |
| `max_header_bytes`            | 16 KiB     |
| `max_requests_per_connection` | 1000       |
| `on_crash`                    | no-op      |
| `on_reject`                   | no-op      |
| `on_request`                  | no-op      |
| `on_upgrade`                  | none       |
| `read_timeout`                | 30 s       |
| `tls`                         | none       |
| `write_timeout`               | 30 s       |

The two size caps apply to both protocols. `ranger` passes them to
`Parser.Limits` on HTTP/1.1, and to `Collector` and
`Settings.max_header_list_size` on HTTP/2. An application sets a
limit once.

`max_connections` leaves room under the runtime's process ceiling,
which RUNTIME-GAPS.md puts near 30000 before tuning. Every
connection is a process and so is every request in progress.

### Behind a proxy

Most Koja services run behind a proxy that ends TLS, and remem is
one. Its proxy is Envoy Gateway. The proxy speaks cleartext to the
pod, and it speaks HTTP/1.1 unless the Service says otherwise. An
`appProtocol` of `kubernetes.io/h2c` on the Service port switches
that hop to HTTP/2 with prior knowledge.

Kubernetes HTTP probes are HTTP/1.1 and go to the same port. This
is why `ranger` serves both protocols on one port and why cleartext
HTTP/2 comes before HTTP/2 over TLS. A service in this setting never
needs the server ALPN shim.

## Scout

`scout` is the client for a program that runs for a long time. It
keeps connections open, shares them between processes, and bounds
every wait.

Three programs call HTTP from Koja today: a trace exporter, an
embedding client, and a Discord client. Each one talks to one origin
and opens a connection for every request.

Two positions in this section are proposals and not decisions. Each
is marked **Open**.

### `Client` is a handle

```koja
client = try Client.start(Config{})

response = try client.get("https://example.com/users/7")
response = try client.post(url, body, headers)
response = try client.send(request)

try client.stop(timeout)
```

`Client.start` spawns the client's processes as children of the
caller. The handle is a small value. A program copies it into every
process that makes requests, the way it copies a `Pool`.

The functions have the names and the parameter order of the one-shot
client in the standard library. Code moves from `HTTP.post` to
`client.post` with no other change. `send` takes a `Request` that
the caller built.

One client has one `Config`. A program that needs other settings for
one service starts a second client. A push provider that wants four
connections to one origin is the example.

**Open.** The handle and the function names are a proposal. The
parts below do not depend on them.

### Origins

An origin is a scheme, a host, and a port. The client keeps one
entry for each origin that it has seen, and it makes the entry on
the first request.

The first connection to an origin fixes its protocol.

- An `https` origin offers `h2` and `http/1.1` through ALPN, and the
  server picks.
- An `http` origin is HTTP/1.1. With `http2_cleartext` set, every
  `http` origin is HTTP/2 with prior knowledge, because nothing
  negotiates it.

An HTTP/1.1 origin gets a pool. An HTTP/2 origin gets shared
connections.

A directory process holds the entries. Each request asks it once for
the route to its origin. After that the caller talks to the pool or
to a connection process.

**Open.** Two parts of this are not settled.

- Every request passes through one directory process. A benchmark
  must show what that costs.
- `pooler` makes its own resources, so the connection that learned
  the protocol cannot join the pool. The client closes it. That is
  one extra handshake for each `https` origin that picks HTTP/1.1.

### HTTP/1.1 origins use `pooler`

Each HTTP/1.1 origin has a `Pool` of sockets. A request runs in the
caller's process.

1. Check out a lease, and wait at most `pool_timeout`.
2. Write `request.serialize()` with `write_all`.
3. Read until `Parser.parse_response` returns `Complete`.
4. If the connection stays open, check the lease in. If not, discard
   it.

The caller does the I/O, so the request and the response cross no
process boundary. Only the socket does. On a TLS connection the
session crosses with it, on every request. This is the same move as
the upgrade handoff in `ranger`, and it needs the same test.

`scout` needs three things that `pooler` does not have today.

- **A fill option.** The pool fills every slot at start, which fits
  a database. A new option picks between that and a fill on demand.
  On demand, the pool makes a resource when a checkout finds none
  and a slot is free. `scout` fills on demand.
- **Idle expiry.** A server closes a connection that stays idle. The
  pool must destroy a resource that has waited longer than a limit.
  `scout` sets that limit to `idle_timeout`.
- **Borrower monitoring.** The pool must monitor the process that
  holds a lease. When that process exits, the pool destroys the
  resource and frees the slot. Today a process that crashes with a
  lease takes one resource out of the pool for good.

Borrower monitoring helps every user of `pooler`. A database pool
has the same leak today.

Idle expiry cannot close every gap. A server can close a connection
at the moment that a request leaves. The retry rule below covers
that case.

### HTTP/2 origins share connections

One HTTP/2 connection carries many requests at once, so no caller
borrows it. A connection process owns the socket, a
`ClientConnection`, and a `Collector`. Its mailbox carries four
kinds of message.

- A request from a caller.
- A cancel from a caller that stopped waiting.
- `IO.Ready`, when the socket has bytes.
- A timer tick.

A caller sends its request with `call`. The connection process opens
a stream and holds the reply until the response is complete. A
thousand callers at once are a thousand streams on one socket.

When the connection is at the peer's stream limit, later requests
wait in the connection process in order. No request waits longer
than its timeout. A caller that stops waiting sends a cancel, and
the connection process resets the stream.

The request and the response each cross one process boundary as a
copy.

The connection process keeps its connection alive.

- It connects in the background. A connect that fails runs again
  after a backoff, with the rule that `pooler` uses. A request that
  arrives before the connection is ready waits for it.
- It sends a PING when no frame has arrived for `ping_interval`. If
  no `Pong` arrives in one more interval, it closes the socket and
  connects again.
- When the peer sends GOAWAY, it opens a new connection for new
  requests and lets the open streams finish on the old one.

`http2_connections` sets how many connections each HTTP/2 origin
has. The default is one, and the directory gives them out in turn.

A push provider is the case for more than one. It sends thousands of
small requests each second to one origin. One connection is one
process, one TCP stream, and one stream limit, so its throughput has
a ceiling. Each request is a small whole message. The provider needs
many streams and several connections, and it does not need streaming
bodies.

A library can also skip `scout` and own a `ClientConnection` in its
own process. `http2` has no sockets and no processes so that this
stays possible.

### Timeouts

| Setting           | Default | Bounds                                   |
| ----------------- | ------- | ---------------------------------------- |
| `connect_timeout` | 5 s     | The TCP connect and the TLS handshake    |
| `pool_timeout`    | 5 s     | The wait for a lease on HTTP/1.1         |
| `timeout`         | 30 s    | The whole request, to its last byte      |

Each function takes a `timeout` that replaces the default for one
request. No wait in `scout` is without a bound.

### Retries and redirects

`scout` sends a request a second time in two cases, and never a
third time.

- A reused HTTP/1.1 connection fails before the first byte of a
  response, and the method is idempotent. The server closed the
  connection while it was idle.
- An HTTP/2 stream is reset with `RefusedStream`. The server says
  that it did not process the request, so the method does not
  matter.

No other failure is sent again. A 429 or a 503 is a response, and
the caller decides what to do with it.

`scout` does not follow redirects. A 3xx is a response.

### Upgrade

```koja
upgrade = try client.upgrade(request)

struct Upgrade
  leftover: Binary
  response: Response
  socket: TCPSocket
end
```

`upgrade` sends a request that asks for another protocol. It opens a
new connection for it, offers only `http/1.1` through ALPN, and
never pools it. The socket belongs to the caller.

A response that is not a 101 fails with `Error.NotUpgraded`, which
holds the response. The `websocket` package builds the request and
checks the 101.

### Errors

```koja
enum Error
  Connection(SocketError | IOError | TLSError)
  InvalidUrl(String)
  NotUpgraded(Response)
  Parse(Parser.Error)
  PoolTimeout
  Reset(HTTP2.ErrorCode)
  Stopped
  Timeout
end
```

`Connection`, `InvalidUrl`, `Parse`, and `Timeout` mean what they
mean in the one-shot client. `PoolTimeout` means every connection to
the origin stayed busy for `pool_timeout`. `Reset` means the peer
reset the stream or closed the connection with that code. `Stopped`
means the client was stopped.

### Configuration

| Field               | Default              |
| ------------------- | -------------------- |
| `connect_timeout`   | 5 s                  |
| `http2_cleartext`   | off                  |
| `http2_connections` | 1                    |
| `idle_timeout`      | 30 s                 |
| `max_body_bytes`    | 16 MiB               |
| `max_header_bytes`  | 16 KiB               |
| `ping_interval`     | 30 s                 |
| `pool_size`         | 10                   |
| `pool_timeout`      | 5 s                  |
| `timeout`           | 30 s                 |
| `tls`               | `TLSConfig.client()` |

Every default is a first value to tune.

`pool_size` is the number of connections for each HTTP/1.1 origin.
`max_body_bytes` caps a response body on both protocols. `tls`
carries the trust store, and `scout` sets the ALPN names itself.

A client certificate does not work yet. `TLSConfig` has an
`identity` field, but `TLSSession.connect` does not load it. A
service that asks for a client certificate needs that change in
`Net` first.

The default `idle_timeout` is below the 60 seconds that `ranger`
waits. `scout` closes an idle connection before a `ranger` server
does.

### Left out

- **Cookies and proxies.**
- **Compression.** `scout` does not ask for a compressed response
  and does not decode one.
- **A send that does not wait.** A caller that wants many requests
  in flight uses one process for each.
- **Connections that scale with load.** The count for each origin is
  fixed in `Config`.

## Migration

The work lands in this order. Each step in the standard library is
one MR with its breaking lines in the changelog.

Steps 1 to 3 depend on none of the open positions in the `ranger`
and `scout` sections. They are functions over values, with no
process and no policy in them. They can start while the rest is
under review.

1. **`Net`.** `write_all`, `TCP_NODELAY` by default, and the client
   half of ALPN. Nothing breaks.
2. **`HTTP`.** The vocabulary types, the incremental parser,
   `serialize` to bytes, and the one-shot client on top of them.
   This is the breaking step.
3. **`http2`.** A new package. `Frame` and `HPACK` come first, then
   the core, then the two connection types and `Collector`. Its
   tests need no socket. A client connection talks to a server
   connection in one process, and appendix C of RFC 7541 has test
   vectors for HPACK.
4. **`trail`.** Bodies become bytes and `path` becomes `uri`.
5. **`ranger`.** It leaves remem for its own repository and lands in
   three parts. The first is the server handle, HTTP/1.1 on the new
   parser, the deadlines, and the drain. The second is cleartext
   HTTP/2. The third is the upgrade path.
6. **`pooler`.** The fill option, idle expiry, and borrower
   monitoring.
7. **`scout`.** HTTP/1.1 on `pooler` comes first, then HTTP/2
   through ALPN, then `upgrade`.
8. **Adoption.** remem drops `lib/ranger` and its accept loop, and
   its embedder and its exporter can move to `scout`. The shortener
   example drops its copy of the worker.
9. **Server ALPN.** The runtime shim comes first, then HTTP/2 over
   TLS in `ranger`.

Steps 5 and 7 wait for the positions that are still open.

## Later

### WebSocket

A `websocket` package gets its own design document. It has the shape
of `http2`. The frame codec is a value, the handshake is a set of
functions over `Request` and `Response`, and the package owns no
sockets. It names neither `ranger` nor `scout`.

A client-only version exists today, vendored in a Discord bot. It
writes its upgrade request by hand, it does not check
`Sec-WebSocket-Accept`, and it has no fragmentation and no close
handshake.

This document gives the package the two doors that it needs.

- On the server, a handler answers with the 101 that the package
  builds, and `on_upgrade` receives the socket.
- On the client, `scout` sends the upgrade request and returns the
  socket.

WebSocket over HTTP/2 is RFC 8441. The client sends a `CONNECT`
request with a `:protocol` pseudo header, and the stream stays open
in both directions. That needs streaming bodies in `http2` and one
more setting. Until both exist, a WebSocket needs an HTTP/1.1
connection.

This limit reaches the proxy. A proxy that speaks HTTP/2 to the pod
can carry a WebSocket only through RFC 8441. Check the proxy before
a Service that sets `kubernetes.io/h2c` carries WebSocket traffic.

### Streaming bodies

A streaming body arrives or leaves in pieces, and the program acts
on each piece. Four things need it: WebSocket over HTTP/2, a large
upload or download, server-sent events, and gRPC.

Many requests at once on one HTTP/2 connection are not streaming.
They are in the first version.

The `http2` core is ready for it. `receive` already returns each
`Data` event as it arrives, and `Collector` is the only part that
joins them. Sending needs the functions that send a head, then
pieces, then an end.

HTTP/1.1 needs more. The parser must report a complete head before
the body arrives. `ranger` and `scout` each need a way to give a
body to the application piece by piece.

## Rejected

- **A `Binary | String` body field.** A body that arrives is always
  bytes, so every reader would match on a union to get the one arm
  that occurs. The functions that build a message take the union,
  and the field is `Binary`.
- **An `HTTP.Body` type.** A wrapper with a text form and a byte
  form puts a name between a message and its bytes. `text()` on the
  message does the same work.
- **A reason phrase on `Status`, or a status enum.** With a phrase
  field, two statuses with one code can differ. An enum cannot be
  closed, because any three-digit code is legal.
- **`path` with new `scheme` and `authority` fields.** `URI` already
  has these parts, with a parser and a writer. A second set of
  fields gives two names to one thing.
- **An `Other(String)` method.** Every `match` on a method would
  need an arm for it, and `trail` could no longer list every method
  in an `Allow` header.
- **HTTP/2 in the standard library.** HPACK and the stream state
  machine are large, and they would change only with a compiler
  release. The one-shot client does not need them.
- **One shared package for both protocols.** The HTTP/1.1 codec is
  small, and the one-shot client needs it in the standard library.
  The two codecs share the vocabulary types and no code.
- **One connection type with a role field.** A server that sends a
  request would fail when the program runs. With two types it does
  not compile.
- **Processes inside `http2`.** Kadabra ran a process for each
  stream. In Koja every message between processes is a copy, and a
  package that spawns must choose who owns the socket. A value
  leaves that choice to `ranger` and to `scout`, and it tests
  without a network.
- **`Upgrade: h2c`.** RFC 9113 deprecated this path from HTTP/1.1
  to cleartext HTTP/2. Proxies use prior knowledge.
- **A parser that keeps its position.** It is faster for a body that
  arrives in many reads, and it makes the parser a value that every
  caller must hold. It can come later behind the same functions.
- **`TCP_NODELAY` that a caller turns on.** Every protocol package
  would have to remember it. A package that forgets gets a delay
  that is hard to find.
- **A pool of workers in `ranger`.** A pool bounds a resource that
  is expensive to make. A Koja process is not. The connection limit
  and the stream limit bound the load directly.
- **A pool lease for an HTTP/2 connection.** A lease gives one
  borrower the whole resource. An HTTP/2 connection exists to carry
  many requests at once.
- **Settings for each origin in one `scout` client.** A second
  client does the same work with no new type.
- **No one-shot client.** A script has no dependencies, so the
  client that a script uses must ship with the compiler.

## Prior art

- **Erlang and Elixir.** Ranch and Thousand Island own a listener
  and run one process for each connection. Ranch stops accepting at
  its connection limit. Cowboy runs one process for each request on
  both protocols, and Bandit runs one for each HTTP/2 stream. Mint
  is a connection as a value, with no process. Finch pools Mint
  connections for HTTP/1.1 and shares one connection process for
  HTTP/2, with a count for more. Kadabra and Pigeon are the HTTP/2
  client and the push library that came before this design. Pigeon
  moved from Kadabra to Mint.
- **Python.** `h2` and `h11` are protocol libraries with no I/O. The
  owner gives `h2` the bytes that arrived, gets events, and asks for
  the bytes to send. `http2` has that shape.
- **Rust.** The `http` crate holds the vocabulary types that every
  server and client shares. `httparse` returns partial or complete.
  `h2` is HTTP/2 alone, and `hyper` builds a server and a client on
  the three.
- **Go.** `net/http` has one handler type for both protocols and
  runs each HTTP/2 request in its own goroutine. `Server.Shutdown`
  closes the listeners, closes the idle connections, and waits for
  the rest. `Hijacker` lets a handler take the connection, which is
  how WebSocket libraries work there. `TCP_NODELAY` is on by
  default.

## Open questions

Questions 1 to 7 are the positions that the `ranger` and `scout`
sections mark **Open**.

1. **A handle or a blocking `serve`.** Writing remem's `App` against
   the handle settles it. If every application writes the same wait
   for a signal and the same `stop`, `serve` earns its place.
2. **What happens at the connection limit.** A proxy sees a
   connection that waits in the backlog differently from one that
   gets a 503. A test behind Envoy shows which one it handles
   better.
3. **A process for each request on HTTP/1.1.** A benchmark of the
   shortener example with the handler inline and with a process
   gives the cost. The gain is a 500 and a log line for a crash.
4. **Killing a canceled handler.** This waits for the runtime.
   Borrower monitoring in `pooler` makes a kill safe for a lease. It
   does not make a kill safe for a socket that the handler opened.
5. **The upgrade signal.** Writing the server half of the
   `websocket` package against the 101 signal settles it.
6. **The `scout` handle and its function names.**
7. **The `scout` directory process.** A benchmark gives the cost of
   one lookup for each request. A way to give `pooler` a resource
   that already exists would remove the extra handshake.
8. **A TLS session that changes process.** The HTTP/1.1 pool in
   `scout` and the upgrade handoff in `ranger` both move one. If the
   test fails, each pooled connection in `scout` needs a process of
   its own, the way an HTTP/2 connection has one.
9. **The cost of a connection value.** A `ServerConnection` holds a
   stream table and an output buffer, and it changes on every frame.
   [GAPS.md](GAPS.md) records that a field write builds a new
   struct. A benchmark of `receive` must show that this is cheap
   enough.
10. **Several acceptors.** One listener process accepts every
    connection. Ranch and Thousand Island run many acceptors on one
    socket. Whether several Koja processes can watch one descriptor
    is not known.
11. **`Expect: 100-continue`.** A client that sends it waits for a
    100 before it sends the body. `ranger` cannot answer, because
    the parser reports only a complete message. A function that
    parses the head alone fixes this. Streaming needs the same
    function.
12. **Header names by protocol.** `Headers` keeps a name as it
    arrived. An HTTP/2 request has lowercase names and an HTTP/1.1
    request has the names that the client wrote. Code that looks a
    header up does not notice. Code that walks the list does.
13. **The shape of `HTTP.Error`.** `Connection` wraps a union of
    three causes. A flat union on the error channel is the other
    shape.
14. **When the server ALPN shim ships.** It can ship with the client
    half or wait for a service that ends TLS itself.
15. **Client certificates.** `TLSSession.connect` does not load an
    identity. No caller needs one until a push library does.

Questions 11 to 13 touch the standard library. Each one adds to the
surface in this document or changes one small type.
