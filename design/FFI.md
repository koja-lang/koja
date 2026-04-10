# C Foreign Function Interface

Design notes for Expo's C FFI: calling C libraries from Expo code. The
compiler already calls C internally (the runtime is a C library, codegen
emits calls via intrinsics). The FFI exposes this capability to user code.

---

## Goals

- **Primary**: wrapper packages (argon2, sqlite, openssl) where end users
  never touch FFI. The package author writes the bindings; consumers call
  safe Expo functions.
- **Secondary**: inline FFI for any developer calling C directly from
  application code.
- **Non-goal**: calling Expo from C. Callback support (passing Expo
  closures to C functions that expect function pointers) is a separate
  future topic.

---

## Syntax (decided)

FFI declarations use `@extern "C"` and `@link "libname"` annotations on
structs or individual functions. No new keywords. No new block syntax.

### Struct-level: pure C binding namespace

`@extern "C"` on a struct means every function inside is a C declaration.
No function bodies are allowed -- having a body is a compile error. The
struct is purely a namespace for C symbols.

```expo
@link "argon2"
@extern "C"
struct Argon2C
  fn argon2id_hash_encoded(
    t_cost: UInt32, m_cost: UInt32, parallelism: UInt32,
    pwd: CPtr<UInt8>, pwdlen: UInt32,
    salt: CPtr<UInt8>, saltlen: UInt32,
    hashlen: UInt32, encoded: CPtr<UInt8>, encodedlen: UInt32
  ) -> Int32

  fn argon2id_verify(
    encoded: CPtr<UInt8>, pwd: CPtr<UInt8>, pwdlen: UInt32
  ) -> Int32
end
```

`@link "argon2"` on the struct applies to all functions inside. Produces
`-l argon2` at link time.

### Function-level: C bindings alongside Expo code

`@extern "C"` on an individual function marks that specific function as
a C binding. It can coexist with normal Expo functions in the same struct.

```expo
struct Argon2
  @link "argon2" @extern "C"
  priv fn argon2id_hash_encoded(
    t_cost: UInt32, m_cost: UInt32, parallelism: UInt32,
    pwd: CPtr<UInt8>, pwdlen: UInt32,
    salt: CPtr<UInt8>, saltlen: UInt32,
    hashlen: UInt32, encoded: CPtr<UInt8>, encodedlen: UInt32
  ) -> Int32

  @link "argon2" @extern "C"
  priv fn argon2id_verify(
    encoded: CPtr<UInt8>, pwd: CPtr<UInt8>, pwdlen: UInt32
  ) -> Int32

  fn hash(password: String) -> Result<String, String>
    salt = random.bytes(16)
    buf: CPtr<UInt8> = CPtr.alloc(128)
    pwd_c = password.to_cstring()
    result = argon2id_hash_encoded(
      3, 65536, 1,
      pwd_c.ptr, pwd_c.len,
      salt.ptr, salt.len,
      32, buf, 128
    )
    if result == 0
      Result.Ok(buf.to_cstring().to_string())
    else
      Result.Error("argon2 failed")
    end
  end

  fn verify(password: String, hash: String) -> Bool
    pwd_c = password.to_cstring()
    hash_c = hash.to_cstring()
    argon2id_verify(hash_c.ptr, pwd_c.ptr, pwd_c.len) == 0
  end
end
```

### Multiple annotations

Multiple annotations on a declaration, either stacked or inline:

```expo
@link "argon2" @extern "C"
priv fn argon2id_hash_encoded(...) -> Int32

# or stacked:
@link "argon2"
@extern "C"
priv fn argon2id_hash_encoded(...) -> Int32
```

This is a general-purpose annotation feature, not FFI-specific. Works for
any annotation combination (e.g. `@doc "..." @test`). Each `@` is a
self-delimiting boundary -- no separator needed between annotations.

### Rules

- **Struct-level `@extern "C"`**: every function is a C declaration. Bodies
  are a compile error.
- **Function-level `@extern "C"`**: this function is a C declaration. Can
  coexist with Expo functions in the same struct.
- **Visibility**: author's choice. `fn` (public) or `priv fn` (hidden),
  same as any other function.
- **Protocol conflicts**: if a function name in an `@extern "C"` struct
  conflicts with a protocol default method, it's a compile error.
- **`@link`**: on the struct applies to all functions; on a function
  applies to that function. Function-level overrides struct-level.

### Why structs, not blocks

Types are the namespace boundary in Expo. Files are transparent -- all
types are collected into a flat namespace. Extern functions must live on
a type to be referenceable from other files. A standalone `extern "C"`
block would produce orphaned functions with no namespace.

The `@extern "C"` struct approach also aligns with the now-implemented
inline functions in struct bodies, where `impl` becomes an extension
mechanism (like Swift extensions) rather than the primary way to attach
functions to types.

---

## String interop (decided)

Explicit `CString` type with manual conversion. No automatic marshaling.

- `string.to_cstring() -> CString` -- copies and null-terminates
- `cstring.to_string() -> String` -- copies until null byte, produces
  Expo String

```expo
pwd = "hunter2"
pwd_c = pwd.to_cstring()       # Expo String -> CString
c_result = some_c_function(pwd_c.ptr())
expo_str = c_result.to_string() # CString -> Expo String
```

### `CString` ownership (decided)

- `to_cstring()` allocates with `malloc` (C-compatible). The caller
  is responsible for freeing via `CString.free()` (which calls
  `CPtr.free()` on the underlying pointer).
- C-returned pointers: wrap in a `CString` struct and call `.free()`
  when done, or call `ptr.free()` directly on the `CPtr<UInt8>`.
- `CString` lives in auto-imported stdlib (`std.cstring`). Always
  available, since it's a type like any other.

---

## Implementation phasing (decided)

### Phase 1: minimal FFI -- **DONE**

- ~~`@extern "C"` annotation on structs and functions~~ **Done**
- ~~`@link "libname"` annotation~~ **Done**
- ~~Pass and return primitives only (`Int32`, `UInt32`, `Float`, `Bool`)~~ **Done**
- ~~`-l` flag wired through to linker~~ **Done**
- ~~Multiple annotations per declaration (space-separated or stacked)~~ **Done**

### Phase 2: pointers and strings -- **DONE**

- ~~`CPtr<T>` type with core methods (`null`, `alloc`, `free`, `offset`, `read`, `write`, `is_null?`)~~ **Done**
- ~~`CString` struct (`ptr: CPtr<UInt8>`, `len: Int`) with `to_cstring()` / `to_string()` / `free()`~~ **Done**
- ~~`CPtr<T>` accepted in `@extern "C"` signatures~~ **Done**
- ~~`alloc` / `free` backed by C `malloc` / `free`~~ **Done**

### Phase 3: struct interop

- `@compat "C"` for C-compatible struct layout
- Callback function pointers

### Validation target

argon2 wrapper package: `@extern "C"` struct wrapping libargon2, safe
`Argon2.hash(password)` / `Argon2.verify(password, hash)` API.

---

## Safety model (decided)

No `unsafe` keyword. Extern functions are callable anywhere, like any
other function. Safety is the wrapper package's responsibility.

The `@extern "C"` annotation on the declaration is the signal -- if
you're calling a function marked `@extern "C"`, you know it's C code
and you need to get the arguments right. The compiler can't help you
with pointer validity or memory ownership across the FFI boundary.

### Precedent: Erlang NIFs

Erlang's NIF (Native Implemented Function) system has used this model
for 15+ years. NIFs are C functions callable from Erlang with no
`unsafe` marker. The community convention is "be careful, you might
crash the VM." A buggy NIF crashes the entire BEAM VM -- all processes,
all supervision trees, everything. And yet the ecosystem has been fine
with convention-based safety.

Expo's situation is strictly better: a bad FFI call crashes one OS
process, not a shared VM. If "write good wrappers, test thoroughly"
works for Erlang where the blast radius is the entire runtime, it works
for Expo.

### Why not `unsafe`

Rust's `unsafe` blocks in FFI wrapper code are noise. The wrapper author
already knows they're doing something dangerous -- they're the one who
wrote the `@extern "C"` declaration. Forcing `unsafe` at the call site
inside the wrapper doesn't prevent bad pointer math; it just adds
ceremony. The real safety boundary is the public API the wrapper exposes,
not the internal call sites.

### Convention

Package authors typically make raw C bindings `priv fn` and expose safe
Expo functions as the public API. This is convention, not enforcement --
authors who want to expose raw bindings can use `fn` deliberately.

---

## Open: Ownership boundary

Expo has ownership + move semantics. C has neither. The FFI boundary is
where these models collide.

### Passing Expo values to C

Primitives (`Int32`, `UInt32`, `Float`, `Bool`) are straightforward --
they're passed by value, same as C. No ownership questions.

For pointers (`CPtr<T>`): the pointer value is copied (it's `Copy`). The
Expo side retains no ownership of what's behind the pointer.

For strings: `to_cstring()` creates a copy via `malloc`. The original
Expo `String` is unaffected. The `CString` is a new allocation that
must be freed via `CString.free()` or `cs.ptr.free()`.

### Receiving values from C

Primitives: returned by value, no ownership issues.

Pointers: C returns a `CPtr<T>`. Who allocated the memory? The C library
(via `malloc`). Who frees it? The Expo code must call `ptr.free()` (which
calls `free()`). The Expo runtime will not auto-free memory behind a
`CPtr<T>`.

### Key principle

`CPtr<T>` is an explicit opt-in to manual memory management. It's the
escape hatch. Normal Expo code never touches `CPtr` -- only FFI wrapper
authors do.

### Open questions

- What does `Ptr.read()` return for move types? A copy? Should it be
  restricted to `Copy` types?
- Should there be a `Ptr.read_copy()` vs `Ptr.read_move()` distinction?
- How does `@compat "C"` struct passing work? By value (copy the struct
  onto the C stack) or by pointer?

---

## Open: Type mapping

### Direct mappings

Expo already has fixed-width integer types that map directly to C:

| Expo      | C                   | Size          |
| --------- | ------------------- | ------------- |
| `Int8`    | `int8_t`            | 1 byte        |
| `Int16`   | `int16_t`           | 2 bytes       |
| `Int32`   | `int32_t`           | 4 bytes       |
| `Int`     | `int64_t`           | 8 bytes       |
| `UInt8`   | `uint8_t`           | 1 byte        |
| `UInt16`  | `uint16_t`          | 2 bytes       |
| `UInt32`  | `uint32_t`          | 4 bytes       |
| `UInt64`  | `uint64_t`          | 8 bytes       |
| `Float32` | `float`             | 4 bytes       |
| `Float`   | `double`            | 8 bytes       |
| `Bool`    | `_Bool` / `uint8_t` | 1 byte        |
| `()`      | `void`              | 0 bytes       |
| `CPtr<T>` | `T*`                | pointer-sized |

### The `Int` footgun

Expo's `Int` is 64-bit. C's `int` is 32-bit. A developer wrapping a C
function that takes `int` might reach for `Int` (the Expo default) and
silently pass a 64-bit value. The compiler should warn or error when
`Int` is used in an `@extern "C"` signature.

### Open: platform-dependent C types

C has platform-dependent types (`int`, `long`, `size_t`, `ssize_t`,
`ptrdiff_t`) whose sizes vary by platform and data model (LP64, LLP64).

Options:

- **Fixed-width only**: the developer picks `Int32` or `Int64` to match
  the C API. Simple, explicit, but requires the developer to know the
  target platform's data model.
- **C type aliases**: add `CInt`, `CLong`, `CSize`, `CSSize` that resolve
  to the correct width per platform. Convenient, but adds types that only
  matter for FFI.
- **Hybrid**: provide aliases but recommend fixed-width for portability.

### Open: function pointers as callback params

Expo has `fn(T) -> U` as a closure type (fat pointer: function + env).
C function pointers are thin (just the function address). Passing an Expo
closure to C doesn't work without a trampoline.

Options:

- Defer callbacks entirely (Phase 3+)
- Allow only bare function references (no captures) as C callback params
- Generate trampolines automatically for closures with captures

### Open: C enums

C enums are integers. Expo enums are tagged unions. No direct mapping.

Options:

- Don't support C enum types -- use `Int32` and define Expo constants
- Support a `@compat "C"` enum that's just a named integer (no payloads)
- Defer

---

## `CPtr<T>` design (implemented)

Raw pointer type. `Copy` (just a machine word). No ownership tracking.
The Expo compiler will not auto-free memory behind a `CPtr<T>`.

Named `CPtr` (not `Ptr`) to signal that this is a C interop type --
it uses `malloc`/`free`, not the Expo runtime allocator. Normal Expo
code never touches `CPtr` -- only FFI wrapper authors do.

### API

```expo
CPtr.null()          # -> CPtr<T>    null pointer
CPtr.alloc(count)    # -> CPtr<T>    malloc(count * sizeof(T))
ptr.free()           # free(ptr)     move self, no return
ptr.offset(n)        # -> CPtr<T>    pointer arithmetic
ptr.read()           # -> T          read value at pointer
ptr.write(value)     #               write value at pointer
ptr.is_null?()       # -> Bool       null check
```

All methods are compiler intrinsics backed by LLVM IR generation.
`CPtr<T>` is represented as `Type::Pointer(Box<Type>)` in the AST
and maps to LLVM's opaque pointer type.

### Remaining questions

- Should `CPtr<T>` be restricted to primitives and `@compat "C"`
  structs, or remain generic over all types?
- Should `read()` be restricted to `Copy` types? What about reading a
  struct that contains a `String` (heap-allocated)?
- Null checking: `is_null?()` returns `Bool`. Should there also be a
  `to_option()` -> `Option<CPtr<T>>` for idiomatic handling?

---

## Open: `@compat "C"` struct layout

Opt-in C-compatible memory layout. Only needed for structs that cross
the FFI boundary as values (not pointers to opaque types).

`@compat` is a general-purpose annotation: `"C"` means C-compatible
memory layout, but the annotation is extensible to other formats
(e.g. `@compat "packed"` for wire-format structs, `@compat "json"` for
derived serialization). `@extern "C"` structs get C layout implicitly.
Expo methods on `@compat "C"` structs go in `impl` blocks -- the struct
body is C territory, `impl` extends it with Expo behavior.

```expo
@compat "C"
struct Timespec
  tv_sec: Int
  tv_nsec: Int
end
```

### Semantics

- Fields are laid out in declaration order with C padding rules
- Compiler-chosen layout is the default (and may reorder fields)
- `@compat "C"` structs can be passed to/from C by value or by pointer
- `Debug.format()` auto-derive still works
- Move semantics still work
- `Copy` types remain `Copy`

### Open questions

- Should `@compat "C"` be allowed on enums? C enums are just ints --
  could support `@compat "C"` on enums with no payloads (unit variants
  only) as named integer constants.
- Interaction with generics: `@compat "C"` on `Ptr<T>` is fine (pointer
  is always pointer-sized), but `@compat "C"` on a generic struct with
  a type parameter is questionable.

---

## Open: Linking

### `@link` annotation

`@link "libname"` on a struct or function produces `-l libname` at link
time. Multiple `@link` annotations link multiple libraries.

### `expo.toml` `[link]` table

For library search paths, static archives, and platform-specific options
that don't belong in source code:

```toml
[link]
search_paths = ["/usr/local/lib", "/opt/homebrew/lib"]
static = ["libargon2.a"]
frameworks = ["Security"]  # macOS
```

### Open questions

- Is `@link` on the source sufficient, or is the `[link]` table in
  `expo.toml` also needed? (Probably both -- `@link` for the common
  case, `expo.toml` for search paths and platform specifics.)
- macOS frameworks (`-framework Security`) need special handling.
- Static vs dynamic linking: always dynamic by default? Allow
  `@link static "argon2"`?

---

## Prior art

### Rust

`extern "C" { fn ... }` blocks with `unsafe` at every call site. Pros:
the compiler enforces FFI boundary awareness. Cons: pervasive `unsafe`
blocks in wrapper code are noisy, and safety is really the wrapper's
responsibility anyway -- `unsafe` at the call site doesn't prevent bad
pointer math.

### Go (cgo)

Magic comments above `import "C"`. C code is literally embedded in Go
comments. Automatic type marshaling. Pros: low-friction for simple cases.
Cons: magic comments, slow compilation (invokes a C compiler), hard to
debug, implicit conversions hide costs.

### Zig

`@cImport` / `@cInclude` directly parses C headers and generates
bindings. Pros: zero boilerplate, always in sync with the C library.
Cons: requires a C parser in the compiler, C preprocessor complexity,
breaks with C++ headers.

### Swift

C interop via bridging headers. Objective-C types automatically bridge.
Pros: seamless for Apple platform libraries. Cons: tightly coupled to
the Apple ecosystem, bridging header complexity.

### What Expo takes

- **From Rust**: explicit declarations (no magic comments or header
  parsing). The developer writes the binding signature.
- **From all of them**: wrapper packages as the primary pattern. End
  users don't touch FFI.
- **Expo's own path**: annotations on structs instead of keyword blocks.
  Types as namespaces. No `unsafe` (open question, but leaning away).
  Multiple annotations per declaration (stacked or inline).

---

## Codegen notes

The compiler already does everything needed for extern function calls.
`builtins.rs` uses `module.add_function(name, fn_type, None)` to emit
LLVM `declare` for runtime intrinsics. User `@extern "C"` functions
follow the same pattern:

1. Parse the extern function signature (no body)
2. Type-check parameter and return types against FFI-compatible types
3. Codegen: `module.add_function("argon2id_hash_encoded", fn_type, None)`
   with unmangled C name (no Expo name mangling)
4. Call sites emit a normal LLVM `call` to the declared function
5. Linker: collect all `@link` libraries, pass as `-l` flags to `cc`

The extern function name in LLVM IR must match the C symbol exactly.
Expo's name mangling (which prefixes module/type names) is skipped for
extern declarations.

---

## Open: conditional `@extern` (`@when`)

`@extern "C"` on its own cannot express different C symbols or signatures
per target OS or per compiler version. The same problem exists for any
library that wants to ship one package that works on multiple platforms
or across a range of Expo releases.

**Planned direction:** a single conditional mechanism, working name `@when`,
attachable to declarations (functions, structs, `impl` items, etc.) with
predicate expressions evaluated at **compile time**:

- **Target:** `target_os`, `target_arch`, and related triple fields, so
  Linux can declare `getrandom` and macOS can declare `getentropy` (or
  each links a thin shim) without runtime probing.
- **Toolchain:** `expo_version` / compiler version with semver ranges, so
  package maintainers can ship alternate implementations or shims for
  breaking API changes (same idea as `rust-version` in Cargo, but at the
  declaration level).

**Alternatives** (see also [STDLIB.md](STDLIB.md) for the “system data”
story): file-level or module-level inclusion by target; a tiny C/Rust shim
with one stable symbol so Expo only ever sees one `extern`; both can
coexist with `@when`.

Exact grammar, predicate set, and error messages when no arm matches are
TBD. This section is a placeholder so the FFI doc does not pretend that
one undecorated `extern` block solves platform-specific C APIs.
