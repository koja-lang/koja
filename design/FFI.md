# C Foreign Function Interface

Design notes for Koja's C FFI: calling C libraries from Koja code. The
compiler already calls C internally (the runtime is a C library, codegen
emits calls via intrinsics). The FFI exposes this capability to user code.

---

## Goals

- **Primary**: wrapper packages (argon2, sqlite, openssl) where end users
  never touch FFI. The package author writes the bindings; consumers call
  safe Koja functions.
- **Secondary**: inline FFI for any developer calling C directly from
  application code.
- **Non-goal**: calling Koja from C. Callback support (passing Koja
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

```koja
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

### Function-level: C bindings alongside Koja code

`@extern "C"` on an individual function marks that specific function as
a C binding. It can coexist with normal Koja functions in the same struct.

```koja
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

```koja
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
  coexist with Koja functions in the same struct.
- **Visibility**: author's choice. `fn` (public) or `priv fn` (hidden),
  same as any other function.
- **Protocol conflicts**: if a function name in an `@extern "C"` struct
  conflicts with a protocol default method, it's a compile error.
- **`@link`**: on the struct applies to all functions; on a function
  applies to that function. Function-level overrides struct-level.

### Why structs, not blocks

Types are the namespace boundary in Koja. Files are transparent -- all
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
  Koja String

```koja
pwd = "hunter2"
pwd_c = pwd.to_cstring()       # Koja String -> CString
c_result = some_c_function(pwd_c.ptr())
koja_str = c_result.to_string() # CString -> Koja String
```

### `CString` ownership (decided)

- `to_cstring()` allocates with `malloc` (C-compatible). The caller
  is responsible for freeing via `CString.free()` (which calls
  `CPtr.free()` on the underlying pointer).
- C-returned pointers: wrap in a `CString` struct and call `.free()`
  when done, or call `ptr.free()` directly on the `CPtr<UInt8>`.
- `CString` lives in auto-imported stdlib (`Global.CString`). Always
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

- ~~`CPtr<T>` type with core methods (`null`, `alloc`, `free`, `offset`, `read`, `write`, `null?`)~~ **Done**
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

Koja's situation is strictly better: a bad FFI call crashes one OS
process, not a shared VM. If "write good wrappers, test thoroughly"
works for Erlang where the blast radius is the entire runtime, it works
for Koja.

### Why not `unsafe`

Rust's `unsafe` blocks in FFI wrapper code are noise. The wrapper author
already knows they're doing something dangerous -- they're the one who
wrote the `@extern "C"` declaration. Forcing `unsafe` at the call site
inside the wrapper doesn't prevent bad pointer math; it just adds
ceremony. The real safety boundary is the public API the wrapper exposes,
not the internal call sites.

### Convention

Package authors typically make raw C bindings `priv fn` and expose safe
Koja functions as the public API. This is convention, not enforcement --
authors who want to expose raw bindings can use `fn` deliberately.

---

## Open: Ownership boundary

Koja has value semantics with automatic reference-counted memory. C has
manual memory and raw pointers. The FFI boundary is where these models
meet.

### Passing Koja values to C

Primitives (`Int32`, `UInt32`, `Float`, `Bool`) are straightforward --
they're passed by value, same as C. No ownership questions.

For pointers (`CPtr<T>`): the pointer is a plain machine word, copied by
value like any scalar. The Koja side retains no ownership of what's
behind the pointer.

For strings: `to_cstring()` creates a copy via `malloc`. The original
Koja `String` is unaffected. The `CString` is a new allocation that
must be freed via `CString.free()` or `cs.ptr.free()`.

### Receiving values from C

Primitives: returned by value, no ownership issues.

Pointers: C returns a `CPtr<T>`. Who allocated the memory? The C library
(via `malloc`). Who frees it? The Koja code must call `ptr.free()` (which
calls `free()`). The Koja runtime will not auto-free memory behind a
`CPtr<T>`.

### Key principle

`CPtr<T>` is an explicit opt-in to manual memory management. It's the
escape hatch. Normal Koja code never touches `CPtr` -- only FFI wrapper
authors do.

### Open questions

- What does `CPtr.read()` return for heap-backed types (e.g. a struct
  containing a `String`)? Reconstructing a reference-counted Koja value
  from raw C bytes is fraught -- likely restrict `read()` to scalar /
  `@compat "C"` types.
- How does `@compat "C"` struct passing work? By value (copy the struct
  onto the C stack) or by pointer?

---

## Open: Type mapping

### Direct mappings

Koja already has fixed-width integer types that map directly to C:

| Koja      | C                   | Size          |
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

Koja's `Int` is 64-bit. C's `int` is 32-bit. A developer wrapping a C
function that takes `int` might reach for `Int` (the Koja default) and
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

Koja has `fn(T) -> U` as a closure type (fat pointer: function + env).
C function pointers are thin (just the function address). Passing a Koja
closure to C doesn't work without a trampoline.

Options:

- Defer callbacks entirely (Phase 3+)
- Allow only bare function references (no captures) as C callback params
- Generate trampolines automatically for closures with captures

### Open: C enums

C enums are integers. Koja enums are tagged unions. No direct mapping.

Options:

- Don't support C enum types -- use `Int32` and define Koja constants
- Support a `@compat "C"` enum that's just a named integer (no payloads)
- Defer

---

## `CPtr<T>` design (implemented)

Raw pointer type: a plain machine word, copied by value. No ownership
tracking. The Koja compiler will not auto-free memory behind a `CPtr<T>`.

Named `CPtr` (not `Ptr`) to signal that this is a C interop type --
it uses `malloc`/`free`, not the Koja runtime allocator. Normal Koja
code never touches `CPtr` -- only FFI wrapper authors do.

### API

```koja
CPtr.null()          # -> CPtr<T>    null pointer
CPtr.alloc(count)    # -> CPtr<T>    malloc(count * sizeof(T))
ptr.free()           # free(ptr)     no return
ptr.offset(n)        # -> CPtr<T>    pointer arithmetic
ptr.read()           # -> T          read value at pointer
ptr.write(value)     #               write value at pointer
ptr.null?()          # -> Bool       null check
```

All methods are compiler intrinsics backed by LLVM IR generation.
`CPtr<T>` is represented as `Type::Pointer(Box<Type>)` in the AST
and maps to LLVM's opaque pointer type.

### Remaining questions

- Should `CPtr<T>` be restricted to primitives and `@compat "C"`
  structs, or remain generic over all types?
- Should `read()` be restricted to scalar / `@compat "C"` types? Reading
  a struct that contains a `String` (heap-backed) would need to rebuild a
  reference-counted Koja value from raw bytes.
- Null checking: `null?()` returns `Bool`. Should there also be a
  `to_option()` -> `Option<CPtr<T>>` for idiomatic handling?

---

## Open: `@compat "C"` struct layout

Opt-in C-compatible memory layout. Only needed for structs that cross
the FFI boundary as values (not pointers to opaque types).

`@compat` is a general-purpose annotation: `"C"` means C-compatible
memory layout, but the annotation is extensible to other formats
(e.g. `@compat "packed"` for wire-format structs, `@compat "json"` for
derived serialization). `@extern "C"` structs get C layout implicitly.
Koja methods on `@compat "C"` structs go in `impl` blocks -- the struct
body is C territory, `impl` extends it with Koja behavior.

```koja
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
- Value semantics still apply: the struct stays an independent value;
  only its in-memory layout changes

### Open questions

- Should `@compat "C"` be allowed on enums? C enums are just ints --
  could support `@compat "C"` on enums with no payloads (unit variants
  only) as named integer constants.
- Interaction with generics: `@compat "C"` on `Ptr<T>` is fine (pointer
  is always pointer-sized), but `@compat "C"` on a generic struct with
  a type parameter is questionable.

---

## Linking

### `@link` annotation -- **DONE**

`@link "libname"` on a struct or function produces `-l libname` at link
time. Multiple `@link` annotations link multiple libraries.

#### Symbol naming: `@link "lib:symbol"` -- **DONE**

When the C symbol name differs from the Koja function name, append
`:symbol` to the link string. The part before the colon is the library
name; the part after is the C symbol used in the LLVM `declare`:

```koja
@extern "C" @link "crypto:SHA256"
priv fn sha256_raw(data: CPtr<UInt8>, len: Int64, out: CPtr<UInt8>) -> CPtr<UInt8>
```

This produces `-l crypto` at link time and emits `declare ... @SHA256(...)`
in the LLVM IR, while the Koja function name `sha256_raw` follows
`snake_case` conventions. When the colon is absent (`@link "crypto"`),
the Koja function name is used as the C symbol.

### `koja.toml` `[link]` table

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
  `koja.toml` also needed? (Probably both -- `@link` for the common
  case, `koja.toml` for search paths and platform specifics.)
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

### What Koja takes

- **From Rust**: explicit declarations (no magic comments or header
  parsing). The developer writes the binding signature.
- **From all of them**: wrapper packages as the primary pattern. End
  users don't touch FFI.
- **Koja's own path**: annotations on structs instead of keyword blocks.
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
   with unmangled C name (no Koja name mangling)
4. Call sites emit a normal LLVM `call` to the declared function
5. Linker: collect all `@link` libraries, pass as `-l` flags to `cc`

The extern function name in LLVM IR must match the C symbol exactly.
Koja's name mangling (which prefixes module/type names) is skipped for
extern declarations.

---

## Conditional compilation: `@when`

`@extern "C"` on its own cannot express different C symbols or signatures
per target OS. The same problem exists for any declaration that needs
platform-specific variants -- struct layouts, constants, or function
implementations. `@when` is the conditional compilation mechanism.

### Syntax

`@when` is an annotation that takes a condition expression. The condition
is a simple comparison: a build variable, an operator, and a string literal.

```koja
@when os == "macos"
@extern "C" @link "system"
fn getentropy(buf: CPtr<UInt8>, len: Int64) -> Int32

@when os == "linux"
@extern "C" @link "c:getrandom"
fn getrandom(buf: CPtr<UInt8>, len: Int64, flags: UInt32) -> Int64
```

Supported build variables:

- `os` -- the target operating system: `"macos"`, `"linux"`, `"windows"`,
  `"freebsd"`, etc. Derived from the LLVM target triple.
- `arch` -- the target architecture: `"aarch64"`, `"x86_64"`, `"arm"`,
  etc. Derived from the LLVM target triple.

Supported operators: `==`, `!=`.

`@when` composes with other annotations (stacked or inline):

```koja
@when os == "macos"
@extern "C" @link "system"
fn getentropy(buf: CPtr<UInt8>, len: Int64) -> Int32

@when arch == "aarch64"
const CACHE_LINE_SIZE = 128

@when arch != "aarch64"
const CACHE_LINE_SIZE = 64
```

### Scope

`@when` applies to any top-level item: functions, structs, enums,
constants, type aliases, protocols, impl blocks. It does not apply to
individual struct fields or enum variants -- that is a future extension
if needed.

### Evaluation: early filtering

`@when` is evaluated **early** -- after parsing but before type checking.
Items whose `@when` condition does not match the current build target are
stripped from the AST. By the time the type checker and codegen run, they
don't exist.

This is the same conceptual phase as `@doc` -- metadata consumed before
compilation to shape what enters the type system. The type checker never
sees wrong-platform code, so there are no spurious type errors from
platform-specific struct layouts or missing platform-specific functions.

This matches Rust's `#[cfg]` model, which is battle-tested. The
alternative (late filtering at codegen time) would require the type checker
to reason about conditional types, which adds complexity for no benefit.

### Implementation sketch

**AST**: Add `AnnotationValue::Condition { lhs: String, op: CondOp, rhs: String }`
to the existing `AnnotationValue` enum. `CondOp` is `Eq | NotEq`.

**Parser**: When `parse_annotation` sees `name == "when"`, call a dedicated
parser that reads `ident operator string_lit` instead of the normal
annotation value.

**Build target**: A `BuildTarget` struct holding `os: String` and
`arch: String`, derived from the LLVM target triple at compiler startup.
The triple is already obtained in codegen via `TargetMachine::get_default_triple()`;
extract it earlier in the driver.

**Filtering**: A post-parse pass (`filter_items_for_target`) calls
`Module.items.retain(...)`, keeping items whose `@when` conditions all
match the build target. Called in `resolve.rs` immediately after parsing
each module, before it enters the module graph.

**Grammar**:

```ebnf
annotation_value = string_lit
                 | multiline_string_lit
                 | "false"
                 | condition ;

condition = IDENT , ( "==" | "!=" ) , string_lit ;
```

The `condition` form is only valid for `@when`. Other annotations continue
to accept strings and `false`.

### Future extensions

- **`koja_version`**: `@when koja_version >= "0.9"` for API migration
  across compiler versions. Requires compiler-internal semver comparison
  logic (not the stdlib `Version` type, since annotations are evaluated
  before the type system exists). Additional operators `>=`, `<=`, `>`, `<`
  would be added for version comparisons.
- **Boolean combinators**: `@when os == "macos" and arch == "aarch64"` --
  `and`, `or`, `not` in condition expressions. Not needed for Phase 1.
- **Per-member conditions**: `@when` on individual struct fields or enum
  variants, enabling platform-specific struct layouts without duplicating
  the entire struct. Complex to implement (affects struct layout
  computation) and deferred until a concrete use case demands it.
