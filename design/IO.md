# IO: One Story for Descriptors, Files, Sockets, and the Console

**Status: draft (2026-09-15). Nothing here is implemented.** This
document argues a position for the 0.19 breaking window: Koja gets a
`Read` and `Write` protocol pair, one descriptor-level error type, and
text handling written once. It supersedes the `IO.gets` bullet in
[ROADMAP.md](ROADMAP.md), which becomes one step of this design.

## Summary

- `protocol Read` and `protocol Write` in `Global`. `Fd`, `File`, and
  `TCPSocket` implement both. `read` returns bytes and `<<>>` at end
  of stream.
- Text lives on `Read` once: `read_string`, `read_line`, `read_to_end`.
  `IO.gets(prompt)` is `IO.write(prompt)` plus `STDIN.read_line()`.
- `IO.Error` is the one errno enum for descriptors, files, pipes, and
  sockets. `Socket.Error` folds into it. `TLSError` stays separate.
- `Fd` is the primitive under `File` and `TCPSocket`, not the API user
  code holds. Reactor plumbing leaves the user-facing surface.
- `IO` shrinks to the console: `puts`, `warn`, `write`, `gets`, and
  the three standard descriptors.
- Open: where filesystem statics live, and how a `Read` implementation
  reports an error wider than `IO.Error`.

## What is blurred today

Reading `lib/global/src/fd.koja`, `lib/global/src/io.koja`, and
`lib/net`:

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
   implements `Read`. A thing you can write to implements `Write`.
   Helpers target the protocol, not the type.
3. End of stream is a value, not a sentinel string. `read` returns
   `<<>>`. `read_line` returns `Option<String>`.
4. Errors are typed at the descriptor level. errno is one domain, so
   it is one enum.
5. The descriptor is the floor, not the surface. User code holds a
   `File` or a `TCPSocket`.

## Design

### `Read` and `Write`

```koja
protocol Read
  @doc """
  Reads up to `count` bytes. Returns fewer bytes when fewer are
  available, and `<<>>` at end of stream.
  """
  fn read(self, count: Int) -> Binary ! IO.Error
end

protocol Write
  @doc """
  Writes `data` and returns the number of bytes written.
  """
  fn write(self, data: Binary | String) -> Int ! IO.Error
end
```

`Fd` implements both as the primitive. `File` implements both by
delegating to its descriptor. `TCPSocket` implements both, routing
through the TLS session when one is active. `UDPSocket` implements
neither. Datagrams are not a stream.

The `read` and `read_binary` pair disappears from every type. `read`
is bytes.

### Text on `Read`, written once

```koja
extend Read
  @doc "Reads up to `count` bytes and decodes them as UTF-8."
  fn read_string(self, count: Int) -> String ! IO.Error

  @doc """
  Reads through the next newline. Returns the line without the
  newline, `Some("")` for an empty line, and `None` at end of stream.
  A partial line at end of stream is returned, and the next call
  returns `None`.
  """
  fn read_line(self) -> Option<String> ! IO.Error

  @doc "Reads until end of stream and decodes the whole as UTF-8."
  fn read_to_end(self) -> String ! IO.Error
end
```

Invalid UTF-8 is `IO.Error.InvalidUTF8`, the same variant
`Socket.Error` has today. `read_line` works on a file, a socket, or
`STDIN`, so the byte-accumulation loops in the postgres driver and the
HTTP parser have one implementation to lean on. A `lines()`
`Enumeration` can follow once the buffered reader question below is
settled.

### `IO.Error`

One enum for the errno domain, modeled on `Socket.Error`:

```koja
enum IO.Error
  AddressInUse
  BrokenPipe
  ConnectionAborted
  ConnectionRefused
  ConnectionReset
  Closed
  HostUnreachable
  Interrupted
  InvalidUTF8
  IsDirectory
  NameNotFound
  NetworkUnreachable
  NotConnected
  NotFound
  PermissionDenied
  TimedOut
  Unknown(Int)
  WouldBlock

  fn from_errno -> IO.Error
  fn last -> IO.Error
  fn message(self) -> String
end
```

This is the Rust position. `std::io::ErrorKind` holds `NotFound` and
`ConnectionRefused` in one enum because `EACCES`, `EBADF`, `EPIPE`,
and `EINTR` do not care whether the descriptor is a file or a socket.
`Socket.Error` becomes `IO.Error`. Network-only variants are part of
the enum, not a sibling.

`TLSError` stays separate. It is not errno. `TCPSocket.connect_tls`
and friends keep `! IO.Error | TLSError`.

Every `! String` in `fd.koja` and `net.koja` becomes `! IO.Error`.
`JSON.decode` and the other string errors outside the I/O domain are a
different cleanup with a different shape (a parse error with a
position) and are not part of this document.

### `Fd` is the floor

`Fd` keeps `close` and its `Read` and `Write` implementations. The
reactor methods `block`, `watch`, and `unwatch`, and the `IO.Ready`
event, are infrastructure. `TCPListener` and the runtime use them.
User code does not. They either move to a `Runtime.Reactor` namespace
or stay on `Fd` with docs that mark them internal. Moving them is
cleaner. Staying is less churn. This document leans to moving.

`STDIN`, `STDOUT`, and `STDERR` stay `Fd` constants. `Fd` implements
`Read` and `Write`, so `STDOUT.write` and `STDIN.read_line()` both
work without a wrapper type.

### `File` is a handle

```koja
struct File
  fd: Fd

  fn open(path: String, mode: File.Mode) -> File ! IO.Error
  fn close(self) ! IO.Error
end

impl Read for File
impl Write for File
```

`file.read(n)` replaces `file.fd.read(n)`.

The filesystem statics (`File.read(path)`, `write(path, content)`,
`delete`, `mkdir`, `mkdir_p`, `rmdir`, `rename`, `exists?`, `dir?`)
are path operations, not handle operations. Two places they could go:

- A new `FS` module: `FS.read(path)`, `FS.write(path, content)`,
  `FS.delete(path)`. This is Go's `os` and Rust's `std::fs`.
- Stay on `File` as statics, with only the instance `read` and `write`
  added. `File.read(path)` and `file.read(n)` then share a name with
  different receivers, which Koja allows by dispatch.

The first is cleaner. The second breaks nothing that reads a config
file. This is the open question most likely to be decided on churn
rather than purity.

### `IO` is the console

```koja
struct IO
  fn puts(message: String)
  fn warn(message: String)
  fn write(message: String)
  fn gets(prompt: String) -> Option<String> ! IO.Error
end
```

`gets` writes `prompt` to `STDOUT` and returns `STDIN.read_line()`.
There is no `from:` parameter. A caller with another reader calls
`read_line` on it directly. The `io_gets` lang fixture becomes a
stdlib test that opens a temp file and calls `file.read_line()`.

`IO.Ready` leaves `IO`.

## Migration

Each step is one MR with one breaking line in the changelog.

1. `IO.Error`. Add the enum. Replace `! String` across `fd.koja` and
   `net.koja`. `Socket.Error` becomes an alias for `IO.Error` for one
   release, then goes. `TCPSocket` signatures read
   `! IO.Error | TLSError`.
2. `Read` and `Write`. Add the protocols. `Fd` and `File` implement
   them. `extend Read` gains `read_string`, `read_line`, and
   `read_to_end`. `Fd.read_binary` and `File.read_binary` go.
   `IO.gets` returns `Option<String>` over `read_line`. The lang
   fixture moves to `lib/global/test`.
3. `TCPSocket` onto the protocols. Its `read`, `read_binary`, and
   `write` become the two protocol methods. `TLSSession` reads and
   writes take a `Write` or `Read` value, or stay on `Fd` as an
   internal detail.
4. Reactor plumbing off `Fd`, `IO.Ready` off `IO`.
5. Filesystem statics, if `FS` wins the open question.

Steps 1 and 2 carry most of the value. Steps 4 and 5 are cleanup and
can slip to 0.20 without weakening the story.

## Rejected

- **A `from: Fd = STDIN` parameter on `IO.gets`.** Considered as the
  Elixir-shaped fix for the `gets` gap. It solves one function. The
  protocol solves every reader, and `gets` becomes two lines.
- **`Fd.read` returning `Option<Binary>`.** `<<>>` at end of stream is
  what Rust and Go do (`Ok(0)`, `io.EOF` after zero bytes), and a
  caller that wants to loop already tests for empty. `Option` belongs
  on `read_line`, where an empty line and end of stream are both
  legitimate and distinct.
- **`File.Error` as a sibling of `Socket.Error`.** Two errno enums
  with overlapping variants, and every function touching both declares
  the union. Rust tried per-domain kinds early and collapsed them.
- **A `Reader<T>` struct wrapping any `T: Read` for buffering.**
  Buffering is needed eventually, since `read_line` over unbuffered
  one-byte reads is slow. It is a later layer on top of `Read`, not a
  reason to shape `Read` differently.

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

1. **Wider errors in a `Read` implementation.** Today an `impl` method
   must match the protocol's return type exactly.
   `lift_signatures/impls.rs` checks `types_equivalent` on the full
   `Result`, so `TCPSocket` cannot implement `Read` with
   `! IO.Error | TLSError`. Three ways out, and a decision is needed
   before step 3. The payload variant is the pragmatic pick. The type
   parameter is the principled one.
   - `protocol Read<E>`, implemented as `Read<IO.Error | TLSError>` on
     `TCPSocket`. Helpers become `extend Read<E>` and fail with the
     union of `E` and `IO.Error`. Generic, and every signature grows a
     type parameter.
   - An `IO.Error.TLS(TLSError)` payload variant, so TLS failures fit
     the one enum. Lossless, and the protocol stays simple. It makes
     `IO.Error` know about TLS, which is a layering wrinkle.
   - Allow an `impl` to declare a wider error union than the protocol.
     A caller through the protocol sees the protocol's type, so this
     is unsound unless the compiler narrows dispatch to the concrete
     type. Not viable as stated.
2. **Buffering.** `read_line` over `Fd.read(1)` is one syscall per
   byte. A `BufferedReader<T: Read>` that itself implements `Read` is
   the usual answer and can land after step 2 without changing the
   protocol. Whether `File.open` returns a buffered handle by default
   is a separate choice.
3. **`FS` or `File` statics.** See above.
4. **`Fd.block` and friends.** Move to `Runtime.Reactor` or stay with
   internal docs.
5. **`Write.write` return.** Bytes written, or unit with a fail on
   short write. Rust returns the count and offers `write_all`. Go
   returns the count and an error on short write. Keeping the count
   and adding `write_all` on `extend Write` matches both.
