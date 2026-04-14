# TLS Support

Design for adding TLS (client and server) to the `net` package.
BoringSSL's `libssl.a` provides the implementation; Expo's C FFI
provides the bindings. No new compiler intrinsics are required.

---

## Architecture overview

TLS is built from four independent changes:

1. **Build system** -- embed `libssl.a` alongside `libcrypto.a`
2. **Runtime** -- export one new C function: `expo_io_block`
3. **Stdlib (`std`)** -- add `Fd.block` so Expo code can suspend on I/O
4. **Stdlib (`net`)** -- add `tls.expo` with BoringSSL FFI bindings,
   and modify `tcp.expo` to integrate TLS into `TCPSocket`

Each piece is described below with exact file paths and code.

---

## 1. Build system: embed `libssl.a`

The compiler already embeds `libcrypto.a` from BoringSSL (built by
`boring-sys`). `libssl.a` lives in the same build directory.

### `expo/crates/expo-driver/build.rs`

Add a second search for `libssl.a`, using the same `find_file` helper
and the same `build_dir` that locates `libcrypto.a`:

```rust
let ssl_lib_path = find_file(&build_dir, "libssl.a").unwrap_or_else(|| {
    panic!(
        "libssl.a not found under {}. boring-sys should have built it.",
        build_dir.display()
    )
});
println!(
    "cargo:rustc-env=EXPO_SSL_LIB_PATH={}",
    ssl_lib_path.display()
);
println!("cargo:rerun-if-changed={}", ssl_lib_path.display());
```

### `expo/crates/expo-driver/src/pipeline.rs`

Embed and write the library at link time, mirroring `EMBEDDED_CRYPTO`:

```rust
const EMBEDDED_SSL: &[u8] = include_bytes!(env!("EXPO_SSL_LIB_PATH"));
```

In the `link` function, after writing `libcrypto.a`:

```rust
fs::write(tmp_dir.join("libssl.a"), EMBEDDED_SSL)
    .expect("failed to write embedded ssl library");
```

The `@link "ssl:SYMBOL"` annotations in Expo code will cause the linker
pipeline to pass `-lssl` automatically (via `collect_link_libraries`).

---

## 2. Runtime: export `expo_io_block`

BoringSSL's `SSL_connect`, `SSL_read`, `SSL_write`, etc. return
`SSL_ERROR_WANT_READ` / `SSL_ERROR_WANT_WRITE` when the underlying
socket isn't ready. The TLS code needs to suspend the Expo process
until the fd is ready, then retry -- exactly what the runtime's
internal `io_block` function does.

### `expo/crates/expo-runtime/src/fs.rs`

Add one function (4 lines) at the end of the file:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn expo_io_block(fd: i64, readable: i64) {
    let interest = if readable != 0 { Interest::Readable } else { Interest::Writable };
    io_block(fd as i32, interest);
}
```

This is a thin C ABI wrapper around the existing `io_block`. Since
`libexpo_runtime.a` is always linked, Expo code can call this via
`@extern "C"` without needing `@link`.

---

## 3. Stdlib: `Fd.block`

### `expo/lib/std/src/fd.expo`

Add to the `impl Fd` block (after `unwatch`):

```expo
fn block(self, readable: Bool)
  expo_io_block(self.descriptor, readable ? 1 : 0)
end

@extern "C"
priv fn expo_io_block(fd: Int64, readable: Int64)
```

**Notes:**

- `@extern "C"` without `@link` works because the symbol is in the
  runtime, which is always linked.
- FFI functions require explicit fixed-width types (`Int64`), not `Int`.
- `Fd.descriptor` is `Int` (which is the same underlying type as `Int64`),
  so passing it directly works.
- The ternary `readable ? 1 : 0` is preferred over `if/else/end` inline.

---

## 4. Stdlib: `net/src/tls.expo`

New file with three sections: config struct, BoringSSL FFI bindings,
and high-level Expo functions.

### 4a. `TLSConfig`

```expo
struct TLSConfig
  cert_path: String
  key_path: String
  is_server: Bool

  fn client -> TLSConfig
    TLSConfig{cert_path: "", key_path: "", is_server: false}
  end

  fn server(cert_path: String, key_path: String) -> TLSConfig
    TLSConfig{cert_path: cert_path, key_path: key_path, is_server: true}
  end
end
```

### 4b. BoringSSL FFI bindings

All C bindings live as `priv fn` inside a `struct TLS`, using the
`@link "ssl:SYMBOL"` convention (same pattern as `@link "crypto:EVP_sha256"`
in the crypto package). Every parameter and return type that maps to C
`int` must be `Int64` (not `Int`).

Required BoringSSL functions:

| C function                         | Purpose                               |
| ---------------------------------- | ------------------------------------- |
| `TLS_method`                       | Get the TLS method struct             |
| `SSL_CTX_new`                      | Create SSL context                    |
| `SSL_CTX_free`                     | Free SSL context                      |
| `SSL_CTX_set_default_verify_paths` | Load system CA certs (client)         |
| `SSL_CTX_use_certificate_file`     | Load cert from PEM file (server)      |
| `SSL_CTX_use_PrivateKey_file`      | Load key from PEM file (server)       |
| `SSL_new`                          | Create SSL object from context        |
| `SSL_free`                         | Free SSL object                       |
| `SSL_set_fd`                       | Attach SSL to a file descriptor       |
| `SSL_set_tlsext_host_name`         | Set SNI hostname (client)             |
| `SSL_connect`                      | Client-side handshake                 |
| `SSL_accept`                       | Server-side handshake                 |
| `SSL_read`                         | Read decrypted data                   |
| `SSL_write`                        | Write data (encrypted by SSL)         |
| `SSL_get_error`                    | Get error code after failed operation |
| `SSL_shutdown`                     | Shut down TLS session                 |

Example binding (follow the sha256.expo pattern):

```expo
@extern "C" @link "ssl:SSL_CTX_new"
priv fn ssl_ctx_new(method: CPtr<UInt8>) -> CPtr<UInt8>

@extern "C" @link "ssl:SSL_read"
priv fn ssl_read(ssl: CPtr<UInt8>, buf: CPtr<UInt8>, num: Int64) -> Int64
```

All SSL/SSL_CTX pointers are represented as `CPtr<UInt8>` (opaque).

### 4c. Constants

Top-level (not inside a struct -- `const` is only valid at top level):

```expo
const SSL_ERROR_WANT_READ: Int64 = 2
const SSL_ERROR_WANT_WRITE: Int64 = 3
const SSL_FILETYPE_PEM: Int64 = 1
```

### 4d. High-level functions

The `TLS` struct provides five public functions. Each function that
retries on `WANT_READ`/`WANT_WRITE` must follow this pattern to avoid
a type mismatch in `cond` arms (all arms must produce the same type):

```expo
# WRONG -- cond arms have different types (() vs Result)
cond
  err == SSL_ERROR_WANT_READ -> fd.block(true)     # returns ()
  err == SSL_ERROR_WANT_WRITE -> fd.block(false)   # returns ()
  else -> return Result.Err("failed")              # returns Result
end

# RIGHT -- guard the error first, then block unconditionally
if err != SSL_ERROR_WANT_READ
  if err != SSL_ERROR_WANT_WRITE
    return Result.Err("failed")
  end
end
fd.block(err == SSL_ERROR_WANT_READ)
```

#### `TLS.connect(fd, hostname) -> Result<Pair<CPtr<UInt8>, CPtr<UInt8>>, String>`

Client-side handshake:

1. `ssl_ctx_new(tls_method())`
2. `ssl_ctx_set_default_verify_paths(ctx)` -- system CA certs
3. `ssl_new(ctx)`, `ssl_set_fd(ssl, fd.descriptor)`
4. `ssl_set_tlsext_host_name(ssl, hostname)` -- SNI
5. Loop: call `ssl_connect(ssl)`. On success return `Pair{first: ssl, second: ctx}`.
   On `WANT_READ`/`WANT_WRITE`, call `fd.block(...)` and retry.
   On other error, free ssl/ctx and return error.

#### `TLS.accept(fd, config) -> Result<Pair<CPtr<UInt8>, CPtr<UInt8>>, String>`

Server-side handshake:

1. `ssl_ctx_new(tls_method())`
2. `ssl_ctx_use_certificate_file(ctx, config.cert_path, SSL_FILETYPE_PEM)`
3. `ssl_ctx_use_private_key_file(ctx, config.key_path, SSL_FILETYPE_PEM)`
4. `ssl_new(ctx)`, `ssl_set_fd(ssl, fd.descriptor)`
5. Loop: call `ssl_accept(ssl)`. Same retry/error pattern as `connect`.

#### `TLS.read(ssl, fd, count) -> Result<String, String>`

1. `CPtr.alloc(count)` for the buffer
2. Loop: call `ssl_read(ssl, buf, count)`.
   - `ret > 0`: convert to string via `buf.to_binary(ret).to_string()`, free buf, return Ok.
   - `ret == 0`: EOF, free buf, return `Ok("")`.
   - Error: check `ssl_get_error`. `WANT_READ`/`WANT_WRITE` -> block and retry.
     Other -> free buf, return Err.

#### `TLS.write(ssl, fd, data) -> Result<Int, String>`

1. Convert data to `CString`
2. Loop: call `ssl_write(ssl, data_cs.ptr, data_cs.len)`.
   - `ret > 0`: free CString, return `Ok(ret)`.
   - Error: same retry pattern.

#### `TLS.shutdown(ssl, ctx)`

1. `ssl_shutdown(ssl)`
2. `ssl_free(ssl)`
3. `ssl_ctx_free(ctx)`

No retry loop needed for shutdown -- best-effort.

---

## 5. `TCPSocket` integration

### `expo/lib/net/src/tcp.expo`

#### Add fields

```expo
struct TCPSocket
  socket: Socket
  ssl: CPtr<UInt8>
  ssl_ctx: CPtr<UInt8>
```

Initialize both to `CPtr.null()` everywhere a `TCPSocket` is
constructed: `connect`, `connect_addr`, `TCPListener.accept`,
`TCPListener.try_accept`.

#### New functions

```expo
fn connect_tls(host: String, port: Int) -> Result<TCPSocket, String>
```

Convenience: `connect` then `upgrade_tls`.

```expo
fn upgrade_tls(move self, hostname: String) -> Result<TCPSocket, String>
```

Calls `TLS.connect(self.socket.fd, hostname)`. On success, sets
`self.ssl` and `self.ssl_ctx` from the returned pair.

```expo
fn accept_tls(move self, config: TLSConfig) -> Result<TCPSocket, String>
```

Calls `TLS.accept(self.socket.fd, config)`. Same pattern.

```expo
fn tls?(self) -> Bool
```

Returns `not self.ssl.null?()`.

#### Modify existing functions

**`read`**: dispatch based on `self.ssl.null?()`:

```expo
fn read(self, count: Int) -> Result<String, String>
  if self.ssl.null?()
    self.socket.fd.read(count)
  else
    TLS.read(self.ssl, self.socket.fd, count)
  end
end
```

**`write`**: same pattern, dispatch to `TLS.write` or `self.socket.fd.write`.

**`close`**: if TLS is active, call `TLS.shutdown` first, then close
the underlying socket:

```expo
fn close(move self) -> Result<String, String>
  if not self.ssl.null?()
    TLS.shutdown(self.ssl, self.ssl_ctx)
  end
  self.socket.close()
end
```

---

## 6. HTTP client HTTPS support

### `expo/lib/http/src/client.expo`

In `Http.request`, after parsing the URL: if the scheme is `https://`,
use `TCPSocket.connect_tls` instead of `TCPSocket.connect`. The URL
parser already detects `https://` and sets port 443. You need to
thread the `host` through to `connect_tls` for SNI.

```expo
socket =
  if url.starts_with?("https://")
    match TCPSocket.connect_tls(host.clone(), port)
      Result.Ok(s) -> s
      Result.Err(e) -> return Result.Err(Error.ConnectionFailed(e))
    end
  else
    match TCPSocket.connect(host.clone(), port)
      Result.Ok(s) -> s
      Result.Err(e) -> return Result.Err(Error.ConnectionFailed(e))
    end
  end
```

---

## Gotchas and conventions

### FFI types

`@extern "C"` functions **must** use explicit fixed-width types. `Int`
is not allowed, even though `Int` and `Int64` are the same underlying
type. Use `Int64` for all C `int`, `long`, and `size_t` parameters
(this matches the crypto package convention).

### `@link` syntax

`@link "ssl:SSL_connect"` means: link against `libssl.a` (produces
`-lssl`) and use `SSL_connect` as the C symbol name. The part before
the colon is the library name; the part after is the symbol. This is
the same convention used by `@link "crypto:EVP_sha256"`.

### `cond` arm types

`cond` is value-producing. All arms must evaluate to the same type.
A `return` statement in one arm doesn't make it compatible with `()`
arms. Avoid mixing `fd.block()` (returns `()`) with `return Result.Err(...)`
in the same `cond`. Use the guard-then-block pattern shown above.

### Constants must be top-level

`const` declarations cannot appear inside struct or impl blocks. Place
them at the file's top level.

### `Fd.descriptor` is `Int`, not `Int64`

`Fd.descriptor` is typed `Int`. Changing it to `Int64` would be a
large refactor across the codebase. Since `Int` and `Int64` are the
same type, passing `self.descriptor` to an `@extern "C"` function
expecting `Int64` works without conversion. This is a known wart;
future work may unify `Int`/`Int64` in FFI contexts.

### Hostname for `CString`

When passing hostnames to `ssl_set_tlsext_host_name`, convert with
`hostname.to_cstring()`, pass `.ptr`, and `free()` after the call
returns. Same pattern as the crypto package.

---

## Testing

Tests go in `expo/lib/net/test/` and `expo/lib/http/test/`. Possible
test cases:

- **HTTPS GET**: `Http.get("https://httpbin.org/get")` returns 200
  (requires network; may need to be skipped in CI).
- **TLS connect/read/write**: `TCPSocket.connect_tls("example.com", 443)`,
  write an HTTP request, read the response.
- **Server-side TLS**: spin up a `TCPListener`, accept a connection,
  `accept_tls` with a self-signed cert/key, verify handshake completes.
  Can be tested locally without network.

After implementation, run:

- `cargo fmt` (Rust formatting)
- `cargo clippy --workspace` (zero warnings)
- `expo format` (Expo formatting, if applicable)

---

## File summary

| File                                      | Change                                                            |
| ----------------------------------------- | ----------------------------------------------------------------- |
| `expo/crates/expo-driver/build.rs`        | Find `libssl.a`, set env var                                      |
| `expo/crates/expo-driver/src/pipeline.rs` | Embed + write `libssl.a`                                          |
| `expo/crates/expo-runtime/src/fs.rs`      | Add `expo_io_block` (4 lines)                                     |
| `expo/lib/std/src/fd.expo`                | Add `Fd.block` + extern decl                                      |
| `expo/lib/net/src/tls.expo`               | **New file**: TLSConfig, FFI bindings, TLS operations             |
| `expo/lib/net/src/tcp.expo`               | Add ssl/ssl_ctx fields, TLS methods, dispatch in read/write/close |
| `expo/lib/http/src/client.expo`           | Use `connect_tls` for https:// URLs                               |
