# IO: One Story for Descriptors, Files, Sockets, and the Console

**Status: steps 1 and 2 landed (2026-10-04), steps 3 to 5 open.** This
document argues a position for the 0.20 breaking window. Koja gets an
`IO.Reader` and `IO.Writer` protocol pair, typed errors in place of
every `! String`, and text handling written once. It supersedes the
`IO.gets` bullet in [ROADMAP.md](ROADMAP.md), which becomes one step
of this design. The Summary and Design sections describe what landed.
The Migration section says what is left.

## Summary

- `protocol IO.Reader` and `protocol IO.Writer`, nested under `IO`.
  `Fd` implements both, and `TCPSocket` follows in step 3. `read`
  returns bytes and `<<>>` at end of stream.
- Text lives on `IO.Reader` once, as default-bodied methods
  `read_string` and `read_line`. `IO.gets(prompt)` is
  `IO.write(prompt)` plus `STDIN.read_line()`.
- Three errno domains. `IO.Error` for an open stream, `File.Error` for
  a path, `Socket.Error` for connection setup. `TLSError` stays
  separate. Decode failures are `String.ConversionError`.
- `Fd` is the handle. `File.open` returns one, and `File` is the path
  module with statics only. `TCPSocket` wraps an `Fd` for the
  socket-specific surface. Reactor plumbing leaves `Fd` in step 4.
- `IO` is the I/O namespace. Its own functions are the console,
  `puts`, `warn`, `write`, and `gets`, and the protocols and the
  stream error nest under it.
- Timeouts are per call. `IO.Reader.Options` and `IO.Writer.Options`
  carry one, and a wait that passes it fails with `IO.Error.TimedOut`.
- Open: how an `IO.Reader` implementation reports an error wider than
  `IO.Error`, which step 3 needs.

## What was blurred

Reading `lib/global/src/fd.koja`, `lib/global/src/io.koja`, and
`lib/net` before steps 1 and 2 landed showed this.

- `Fd` is three things. The raw descriptor, the user-facing reader and
  writer (`read`, `read_binary`, `write`), and the reactor handle
  (`block`, `watch`, `unwatch`). `IO.Ready`, the reactor event, lives
  in `io.koja` next to `puts`.
- `File` wraps an `Fd` but has no `read` or `write` of its own, so
  callers reach through it: `file.fd.write("...")`. The same `File`
  struct carries path-level filesystem statics (`File.delete`,
  `mkdir_p`, `exists?`, `rename`). Handle and filesystem are one type.
- `Socket` wraps an `Fd`. `TCPSocket` wraps a `Socket` and
  re-implements `read`, `read_binary`, and `write` a third time with
  its own error mapping. `TLSSession.read_binary(self, fd: Fd, count)`
  takes the descriptor as a parameter because nothing names "a
  readable thing".
- Every readable type has a `read` and `read_binary` pair, and each
  does its own UTF-8 decode. Text handling is copied four times.
- `read` returns `""` or `<<>>` at end of stream, so a caller cannot
  tell end of input from an empty read. `IO.gets` inherits the same
  ambiguity and returns `""` for both an empty line and end of input.
- Errors. `Fd` and `Socket` fail with `String`. `TCPSocket` fails with
  `Socket.Error | TLSError`. `Socket.Error` already has `from_errno`,
  `last`, and `message`, so the typed migration started one floor up
  and stopped.

## Why not the Elixir shape

Elixir's `IO` is built on devices being processes. `IO.write(device,
data)` is a message send, `File.open` returns a pid, and `IO.gets`
takes the device as a defaulted first argument. That fits a world
where everything is a process.

Koja has processes, but `Fd` is a value and the reactor is a runtime
service. That puts Koja closer to Rust, Go, and Zig. All three
converge on the same answer: a small reader and writer abstraction,
one error type at the descriptor level, and every text convenience
written once against the reader. Koja has protocols, unions on the
error channel, and `extend`, which is exactly the toolkit that answer
needs.

## Principles

1. Bytes are the primitive. Text is a view over bytes, decoded in one
   place.
2. One abstraction per capability. A thing you can read from
   implements `IO.Reader`. A thing you can write to implements
   `IO.Writer`. Helpers target the protocol, not the type.
3. End of stream is a value, not a sentinel string. `read` returns
   `<<>>`. `read_line` returns `Option<String>`.
4. Errors are typed, and an error enum is one set of causes a caller
   could branch between at one call site. A stream, a path, and a
   connection are three such sets, so errno splits into three enums
   and a composite operation declares the union.
5. The descriptor is the handle. User code holds the `Fd` that
   `File.open` returned, or a `TCPSocket` that wraps one.

## Design

### `IO.Reader` and `IO.Writer`

```koja
struct IO.Reader.Options
  timeout: Option<Duration> = Option.None
end

protocol IO.Reader
  @doc """
  Reads up to `count` bytes. Returns fewer bytes when fewer are
  available, and `<<>>` at end of stream.
  """
  fn read(self, count: Int, options: IO.Reader.Options = IO.Reader.Options{}) -> Binary ! IO.Error
end

struct IO.Writer.Options
  timeout: Option<Duration> = Option.None
end

protocol IO.Writer
  @doc """
  Writes `data` and returns the number of bytes written.
  """
  fn write(self, data: Binary | String, options: IO.Writer.Options = IO.Writer.Options{}) -> Int ! IO.Error
end
```

The protocols are nouns, like `Enumeration`, `Equality`, and
`Reporter`, and they nest under `IO` the way `Date.Format` nests
under `Date`. Top-level `Global` protocols are language-level ones.
A domain protocol lives under its domain. Nesting also keeps the bare
names free. The alias resolver rejects an alias that would shadow a
`Global` name, so a top-level `Reader` would make `alias CSV.Reader`
an error in every program.

`Fd` implements both. `TCPSocket` implements both in step 3, routing
through the TLS session when one is active. `UDPSocket` implements
neither. Datagrams are not a stream.

The `read` and `read_binary` pair disappears from every type. `read`
is bytes.

### Text on `IO.Reader`, written once

```koja
protocol IO.Reader
  fn read(self, count: Int, options: IO.Reader.Options = IO.Reader.Options{}) -> Binary ! IO.Error

  @doc "Reads up to `count` bytes and decodes them as UTF-8."
  fn read_string(self, count: Int, options: IO.Reader.Options = IO.Reader.Options{}) -> String ! IO.Error | String.ConversionError
    bytes = try self.read(count, options)
    try bytes.to_string()
  end

  @doc """
  Reads through the next newline. Returns the line without the
  newline, `Some("")` for an empty line, and `None` at end of stream.
  A partial line at end of stream is returned, and the next call
  returns `None`.
  """
  fn read_line(self, options: IO.Reader.Options = IO.Reader.Options{}) -> Option<String> ! IO.Error | String.ConversionError
    # loop on read(1, options), stop at <<10>> or <<>>
  end
end
```

The helpers are default-bodied methods inside the protocol, not an
`extend IO.Reader`. The typechecker rejects a `self` method on a
protocol extend, and a default body is what a protocol method with a
body already means. An implementation supplies `read` and gets the
other two.

Invalid UTF-8 is `String.ConversionError.InvalidUTF8`, the error
`Binary.to_string` already fails with, so it leaves every I/O enum and
the decoding methods widen their error channel by it. `read_line`
works on a file, a socket, or `STDIN`, so the byte-accumulation loops
in the postgres driver and the HTTP parser have one implementation to
lean on. `read_to_end` is not in the protocol. `File.read(path)` and
`File.read_binary(path)` cover whole-file reads, and a stream version
can land with `write_all` when a caller needs it. A `lines()`
`Enumeration` can follow once the buffered reader question below is
settled.

### Three error domains

Three enums, each modeled on the `Socket.Error` that existed before
step 1, and each the set of causes one call site could branch between.

```koja
enum IO.Error
  BrokenPipe
  ConnectionReset
  Closed
  Interrupted
  TimedOut
  Unknown(Int)
end

enum File.Error
  AlreadyExists
  DirectoryNotEmpty
  InvalidPath
  IsDirectory
  NotDirectory
  NotFound
  PermissionDenied
  Unknown(Int)
end

enum Socket.Error
  AddressInUse
  ConnectionAborted
  ConnectionRefused
  HostUnreachable
  Interrupted
  NameNotFound
  NetworkUnreachable
  PermissionDenied
  TimedOut
  Unknown(Int)
end
```

Each has `from_code(code) -> Option<Self>`, a total `last()`, and
`message()`. `IO.Error` is what `read`, `write`, and `close` do to an
open descriptor. `File.Error` is what `open`, `delete`, `mkdir`,
`rmdir`, and `rename` do to a path. `Socket.Error` is what `connect`,
`bind`, `listen`, `accept`, and `resolve` do, and it survives in
`lib/net/src/error.koja` minus the stream and decode causes it had
absorbed. A function fails with the domain of the failure, not the
domain of its owner, so `File.open` fails with `File.Error` and
`Fd.read` with `IO.Error`.

Rust keeps one `ErrorKind` for all of errno. Koja's error channel is
a union, and ERROR-HANDLING.md says a composite operation declares the
union of what it does. `File.read(path)` opens, reads, and closes, so
it fails with `File.Error | IO.Error | String.ConversionError`, and a
caller that matches on it sees each cause under the enum that owns it.
One enum would put `NotFound` on a socket read and `ConnectionRefused`
on a file open, causes that cannot happen there.

Two `IO.Error` variants are Koja events rather than POSIX codes.
`Interrupted` means a system message woke the parked process, and the
caller must return to its run loop. `Closed` means the reactor woke a
waiter whose descriptor another process closed, so it observes
`EBADF`. `WouldBlock` never escapes the readiness wait and
`NotConnected` is unreachable on a connected `TCPSocket`, so neither
has a variant.

`TLSError` stays separate. It is not errno. `TCPSocket.connect_tls`
and friends keep `! Socket.Error | TLSError`.

Every `! String` in `fd.koja`, `file.koja`, and `net.koja` is gone.
`JSON.decode` and the other string errors outside the I/O domain are a
different cleanup with a different shape (a parse error with a
position) and are not part of this document.

### `Fd` is the handle

`Fd` keeps `close` and carries the `IO.Reader` and `IO.Writer`
implementations. It is what `File.open` returns, what `STDIN`,
`STDOUT`, and `STDERR` name, and what the socket types wrap. The
reactor methods `block`, `watch`, and `unwatch`, and the `IO.Ready`
event, are infrastructure. `TCPListener` and the runtime use them.
User code does not. They either move to a `Runtime.Reactor` namespace
or stay on `Fd` with docs that mark them internal. Moving them is
cleaner. Staying is less churn. This document leans to moving.

`Fd` implements both protocols, so `STDOUT.write` and
`STDIN.read_line()` work without a wrapper type, and so does the
descriptor a file open returned.

### `File` is the path module

```koja
struct File
  fn open(path: String, mode: File.Mode) -> Fd ! File.Error
  fn read(path: String) -> String ! File.Error | IO.Error | String.ConversionError
  fn read_binary(path: String) -> Binary ! File.Error | IO.Error
  fn write(path: String, content: Binary | String) ! File.Error | IO.Error
  fn delete(path: String) ! File.Error
  fn rename(source: String, destination: String) ! File.Error
  fn mkdir(path: String) ! File.Error
  fn mkdir_p(path: String) ! File.Error
  fn rmdir(path: String) ! File.Error
  fn exists?(path: String) -> Bool
  fn dir?(path: String) -> Bool
end
```

`File` has no fields and no instance methods. Every function names a
file or directory by its path, and `open` hands back the `Fd`. The
first draft of step 2 had a `struct File{fd: Fd}` that implemented
both protocols by delegating to its descriptor, and the filesystem
statics in a separate `FileSystem` module. Writing it showed that the
value type was a one-field wrapper whose every method forwarded to
`fd`, and that the module split moved `open` away from the operations
it belongs with. The descriptor already is the handle, so the wrapper
went and the statics stayed under `File`.

The registry made the point first. It keys a function by owner, name,
and arity with no static or instance axis, so `File.write(path,
content)` and the protocol adapter `write(self, data)` collided at
arity two and the adapter was dropped without a diagnostic. That is a
compiler gap recorded in GAPS.md, and it is also a sign that one type
should not carry both a path API and a handle API under the same
names.

### `IO` is the I/O namespace

```koja
struct IO
  fn puts(message: String)
  fn warn(message: String)
  fn write(message: String)
  fn gets(prompt: String) -> Option<String> ! IO.Error | String.ConversionError
end
```

`IO`'s own functions are the console. The protocols `IO.Reader` and
`IO.Writer`, their options structs, and the stream error `IO.Error`
nest under it, so `IO` is the namespace for input and output and the
console is the part of it that needs no handle.

`gets` writes `prompt` to `STDOUT` and returns `STDIN.read_line()`.
There is no `from:` parameter. A caller with another reader calls
`read_line` on it directly. The `io_gets` lang fixture became
`lib/global/test/io/reader_test.koja`, which opens a temp file and
calls `read_line` on the descriptor.

`IO.Ready` leaves `IO` in step 4.

### Timeouts are per call

A timeout is a parameter on the call that waits, carried in an
options struct so the protocol method has a stable shape as options
grow. `IO.Reader.Options` and `IO.Writer.Options` each start with one
field, `timeout: Option<Duration> = Option.None`, and every read and
write takes one as a trailing parameter with an all-default value.

```koja
chunk = try client.read(4096, IO.Reader.Options{timeout: Option.Some(remaining)})
sent = try client.write(frame, IO.Writer.Options{timeout: Option.Some(limit)})
```

This is `gen_tcp`'s shape for `connect`, `accept`, and `recv`. Erlang
puts the send bound on the socket as `send_timeout` because
`gen_tcp:send` hands bytes to a port driver queue. Koja's `write`
parks on the reactor exactly like `read`, so the write bound is per
call too. A wait that passes its bound fails with `IO.Error.TimedOut`.
`Option.None` waits without limit.

The first draft put the timeout on the socket as `read_timeout` and
`write_timeout` fields set by `with_read_timeout` and
`with_write_timeout`, the Rust `set_read_timeout` position, so that
`read(self, count)` could stay a one-parameter protocol method. The
options struct removes that constraint. Socket state also meant a
caller could not tell from a call site what bound applied, and the
listener needed its own `TCPListener.Options` to seed each accepted
socket. Both went with step 2. A `TCPListener.Options` can come back
under the same name when a real socket option such as `nodelay` or
`keepalive` exists, since those are state a connection inherits.

The timeout is relative and restarts on every call, which bounds a
silent peer but not a slow one. A peer that sends one byte every four
seconds never trips a five second read timeout. Bounding a whole
request is the caller's job, with an `Instant` from
[TIME.md](TIME.md).

```koja
deadline = Instant.now().plus(Duration.new(5, Duration.Unit.Seconds))
loop
  remaining = deadline.since(Instant.now())
  if remaining.zero?()
    fail IO.Error.TimedOut
  end
  chunk = try client.read(4096, IO.Reader.Options{timeout: Option.Some(remaining)})
  ...
end
```

A `TimedOut` on write can follow a partial write. `IO.Writer.write`
returns the count for this reason, and the doc comment on the error
says that a prefix may have been sent.

`connect` and `accept` are not stream operations, and the handshakes
are not either. Each takes its bound as a trailing
`timeout: Option<Duration> = Option.None`. `TCPSocket.connect(host,
port, timeout)` bounds the TCP handshake. `TCPListener.accept(timeout)`
bounds the wait for a connection. `upgrade_tls(host, config, timeout)`
and `accept_tls(config, timeout)` bound the TLS handshake, and
`connect_tls` applies its one bound to the TCP handshake and then,
measured anew, to the TLS handshake. `try_accept` stays until step 4
swaps it for `accept` with a zero bound, because `TCPServer` uses it.
There is no process-wide or runtime-wide default. Behavior that
depends on ambient state a reader cannot see at the call site is
rejected.

Underneath, every one of these is a bounded reactor wait, the
mechanism `receive ... after` and `Fd.block` already use. Sockets are
non-blocking on both backends, so no socket option is involved. One
runtime entry point, a bounded `Fd.block`, is the whole runtime change.
`Fd.block` returns `Bool`, `true` on the timeout, until step 4 moves
the readiness wait off `Fd`.

## Migration

Each step is one MR with its breaking lines in the changelog.

1. **Done.** `IO.Error`, `File.Error`, and the shrunk `Socket.Error`.
   Every `! String` across `fd.koja`, `file.koja`, and `net.koja`
   replaced. Stream and close errors are `IO.Error` in `tcp`, `tls`,
   `udp`, and the HTTP client. Connection errors stay `Socket.Error`,
   so `TCPSocket.connect_tls` reads `! Socket.Error | TLSError`.
2. **Done.** `IO.Reader` and `IO.Writer` with their options structs.
   `Fd` implements them, `read_string` and `read_line` are default
   bodies, and `Fd.read_binary` is gone. `File` is the path module
   and `File.open` returns an `Fd`. `IO.gets` returns `Option<String>`
   over `read_line`. The socket timeout state from #135 became per-call
   options, and the lang fixture moved to `lib/global/test/io`.
3. `TCPSocket` onto the protocols. Its `read`, `read_binary`, and
   `write` become the two protocol methods. `TLSSession` reads and
   writes take an `IO.Writer` or `IO.Reader` value, or stay on `Fd` as
   an internal detail. Needs open question 1.
4. Reactor plumbing off `Fd`, `IO.Ready` off `IO`, `try_accept` folded
   into `accept` with a zero bound.

Steps 1 and 2 carry most of the value. Step 4 is cleanup and can slip
without weakening the story. The former step 5, filesystem statics in
an `FS` module, is resolved by `File` staying the path module.

## Rejected

- **A `from: Fd = STDIN` parameter on `IO.gets`.** Considered as the
  Elixir-shaped fix for the `gets` gap. It solves one function. The
  protocol solves every reader, and `gets` becomes two lines.
- **`Fd.read` returning `Option<Binary>`.** `<<>>` at end of stream is
  what Rust and Go do (`Ok(0)`, `io.EOF` after zero bytes), and a
  caller that wants to loop already tests for empty. `Option` belongs
  on `read_line`, where an empty line and end of stream are both
  legitimate and distinct.
- **One `IO.Error` for all of errno.** The first draft folded
  `Socket.Error` into one enum with every file and network cause, the
  Rust `ErrorKind` position. A caller matching a socket read would see
  `NotFound` and `IsDirectory` as arms it must name or wildcard, and a
  file open would see `ConnectionRefused`. Three enums keep each match
  to the causes that can happen, and the union on the error channel
  composes them where an operation does both.
- **A `FileSystem` module for the path statics.** Tried during step 2
  and reverted. It left `File` as a one-field wrapper around `Fd`, and
  it moved `open` away from `delete`, `rename`, and `mkdir`, the
  operations it shares a path argument with. `File` as the path module
  with `open -> Fd` keeps them together and needs no second type.
- **Timeouts as socket state.** `read_timeout` and `write_timeout`
  fields set by `with_read_timeout` and `with_write_timeout`, and a
  `TCPListener.Options` to seed accepted sockets. Shipped in #135 and
  replaced in step 2 by the options structs. See "Timeouts are per
  call".
- **A `Reader<T>` struct wrapping any `T: IO.Reader` for buffering.**
  Buffering is needed eventually, since `read_line` over unbuffered
  one-byte reads is slow. It is a later layer on top of `IO.Reader`,
  not a reason to shape `IO.Reader` differently.

## Prior art

- **Rust.** `std::io::Read` and `Write`, `io::Error` with one
  `ErrorKind` for files and sockets, `BufRead::read_line` returning
  `Ok(0)` at end of stream. `File` and `TcpStream` implement both
  traits. Filesystem operations live in `std::fs`.
- **Go.** `io.Reader` and `io.Writer` as one-method interfaces,
  `io.EOF` as a sentinel error, `bufio.Scanner` for lines. `os.File`
  and `net.Conn` implement both. Filesystem operations live in `os`.
- **Zig.** `std.io.Reader` and `Writer` as generic interfaces with
  `readUntilDelimiter`, one `anyerror` set per operation.
- **Elixir.** Devices are processes. `IO.read(device, :line)` and
  `IO.gets(device \\ :stdio, prompt)` return `:eof` as an atom. The
  model depends on every device being a process, which Koja's `Fd`
  is not.

## Open questions

1. **Wider errors in an `IO.Reader` implementation.** Today an `impl`
   method must match the protocol's return type exactly.
   `lift_signatures/impls.rs` checks `types_equivalent` on the full
   `Result`, so `TCPSocket` cannot implement `IO.Reader` with
   `! IO.Error | TLSError`. Three ways out, and a decision is needed
   before step 3. The payload variant is the pragmatic pick. The type
   parameter is the principled one.
   - `protocol IO.Reader<E>`, implemented as
     `IO.Reader<IO.Error | TLSError>` on `TCPSocket`. The default
     bodies fail with the union of `E` and `IO.Error`. Generic, and
     every signature grows a type parameter.
   - An `IO.Error.TLS(TLSError)` payload variant, so TLS failures fit
     the one enum. Lossless, and the protocol stays simple. It makes
     `IO.Error` know about TLS, which is a layering wrinkle.
   - Allow an `impl` to declare a wider error union than the protocol.
     A caller through the protocol sees the protocol's type, so this
     is unsound unless the compiler narrows dispatch to the concrete
     type. Not viable as stated.
2. **Buffering.** `read_line` over `Fd.read(1)` is one syscall per
   byte. A `BufferedReader<T: IO.Reader>` that itself implements
   `IO.Reader` is the usual answer and can land without changing the
   protocol. Whether `File.open` returns a buffered handle by default
   is a separate choice.
3. **`Fd.block` and friends.** Move to `Runtime.Reactor` or stay with
   internal docs.
4. **`write_all` and `read_to_end`.** `IO.Writer.write` returns the
   count, the Rust and Go position, and a short write is the caller's
   to notice. A `write_all` default body on `IO.Writer` and a
   `read_to_end` on `IO.Reader` are the obvious additions. They wait
   for a caller, and for the one-shot `File.read` and `File.write` to
   be rebuilt over `open` and the protocols instead of the
   `read_all` and `write_all` runtime entry points they use today.
