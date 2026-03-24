# Binaries and Bits

Expo's binary system enables bit-level data construction and pattern matching --
the same capability that makes Erlang/Elixir dominant in protocol work, compiled
to native shift-and-mask operations with zero overhead.

---

## Types

Three distinct types for text and raw data:

- **`String`** -- valid UTF-8 text. Codepoint-aware operations (`.length()`,
  `.trim()`, `.split()`). No byte-level methods.
- **`Binary`** -- a sequence of bytes (bit count is always a multiple of 8).
  The common case: network packets, file contents, serialized data.
- **`Bits`** -- a sequence of arbitrary bits. Used for sub-byte protocol fields
  where the data doesn't land on byte boundaries (e.g., HPACK header
  compression in HTTP/2).

All three are **distinct types** with no subtype relationships. Moving between
them requires explicit conversion:

```expo
# Widening (always succeeds, zero-cost)
bytes = text.to_binary()           # String → Binary
bits = bytes.to_bits()             # Binary → Bits

# Narrowing (validates, returns Result)
text = String.from_binary(bytes)   # Binary → String (validates UTF-8)
bytes = bits.to_binary()           # Bits → Binary (validates byte alignment)
```

Widening (shedding a guarantee) always succeeds. Narrowing (adding a guarantee)
requires validation and can fail.

All three types use move semantics. Assignment moves ownership; function
parameters borrow by default. See `archive/20260323-MEMORY.md` for the full ownership model.

### `<<>>` type inference

The `<<>>` literal infers its type from the total bit count of its segments:

- Total bits is a multiple of 8 → **`Binary`**
- Total bits is not a multiple of 8 → **`Bits`**

```expo
<<0xFF, 0x00>>              # 16 bits → Binary
<<1::1, 0::1, 1::1>>       # 3 bits → Bits
<<flag::3, data::5>>       # 8 bits → Binary (byte-aligned total)
<<>>                        # 0 bits → Binary
```

All segment sizes are integer literals, so the compiler always knows the total
at compile time. To force `Bits` on a byte-aligned literal, use a type
annotation or a trailing `_: Bits` in patterns:

```expo
# Type annotation in construction
my_bits: Bits = <<flag::3, data::5>>

# Trailing rest capture in patterns
match data
  <<flag::3, data::5, _: Bits>> -> ...
end
```

---

## Construction

The `<<segments...>>` syntax constructs binary and bitstring values:

```expo
<<0x48, 0x65, 0x6C, 0x6C, 0x6F>>   # 5-byte Binary: "Hello" in ASCII
<<1::1, 0::1, 1::1, 0::1>>          # 4-bit Bits value: 0b1010
<<0xFF, 0x00, length::16>>          # 4-byte Binary: two literal bytes + a 16-bit integer
```

### Segment forms

Each segment in `<<>>` takes one of these forms:

- **`value`** -- 8 bits (one byte), the default size.
- **`value::N`** -- exactly N bits.
- **`value::N byte`** -- exactly N bytes (N \* 8 bits).

When no size is specified, a segment is 8 bits. `<<42>>` and `<<42::8>>` are
identical.

### Overflow

Literal values that don't fit in their segment size are compile errors:

```expo
<<256>>           # compile error: 256 does not fit in 8 bits
<<-1>>            # compile error: -1 does not fit in 8 unsigned bits
<<256::16>>       # ok: 256 fits in 16 bits
```

### Modifiers

Modifiers are lowercase, space-separated, and appear after the size specifier:

- **`signed`** / **`unsigned`** -- interpret the bits as a signed or unsigned
  integer. Default: **unsigned**.
- **`big`** / **`little`** -- byte order for multi-byte segments.
  Default: **big-endian** (network byte order).

```expo
<<temperature::16 signed big>>     # 16-bit signed big-endian integer
<<port::16 unsigned big>>          # 16-bit unsigned big-endian integer
<<value::32 little>>               # 32-bit value in little-endian byte order
```

### Type annotations

Segments can carry a type annotation with `: Type` syntax. The type name
determines the segment size -- no separate `::N` specifier needed. Only concrete
fixed-width types are valid; union types like `Int` or `Float` are not allowed
because the compiler cannot determine their bit width.

Built-in segment types:

- **`: Bool`** -- 1-bit boolean (1 = true, 0 = false).
- **`: Int8`**, **`: Int16`**, **`: Int32`**, **`: Int64`** -- fixed-width integers.
- **`: Float32`** -- 32-bit IEEE 754 single-precision float.
- **`: Float64`** -- 64-bit IEEE 754 double-precision float.

```expo
<<3.14: Float32>>                   # 32-bit IEEE 754 float
<<3.14: Float64>>                   # 64-bit IEEE 754 float
<<padded: Bool, 0::7>>              # 1-bit boolean flag + 7 padding bits
<<port: Int16>>                     # 16-bit integer with Int16 type
```

The `: Type` form and the `::N` form serve different purposes. `::N` extracts N
bits as `Int` (the default integer type) -- convenient for ad-hoc numeric fields.
`: Type` extracts bits as a specific concrete type -- use when the type matters.

### Empty binary

`<<>>` is an empty binary. It is a valid expression anywhere -- you can assign
it, pass it to functions, return it:

```expo
empty: Binary = <<>>
send(<<>>)
```

### Concatenation

The `<>` operator concatenates values of the same type:

```expo
header = <<0x01, 0x02>>
payload = <<0x03, 0x04>>
frame = header <> payload           # <<0x01, 0x02, 0x03, 0x04>>

greeting = "hello" <> " world"      # String <> String → String
```

- `String <> String` → `String`
- `Binary <> Binary` → `Binary`
- `Bits <> Bits` → `Bits`
- Mixing types → **compile error** (explicit conversion required)

`<>` is for combining existing values. `<<>>` is for constructing from segments.
The two syntaxes serve different purposes and don't overlap.

---

## String and Binary

The type distinction keeps operations honest: `String` methods (`.upcase()`,
`.trim()`, `.split()`) are codepoint-aware and cannot corrupt UTF-8. `Binary`
methods (`.take()`, `.drop()`) operate on bytes and cannot produce invalid text.
Each type's API is focused and clear.

In practice, the conversion boundary is architecturally natural. Text-based
protocols (HTTP, JSON, WebSocket) validate UTF-8 once at the boundary, then
work entirely in `String`-land:

```expo
fn handle_request(data: Binary) -> Result<Response, Error>
  text = String.from_binary(data)?
  Json.parse(text)
end
```

Binary protocols (HTTP/2 frames, DNS, TLS) stay in `Binary`-land and extract
`Int` fields via pattern matching -- no string conversion needed.

### String literals in `<<>>`

String literals can appear inside `<<>>` for construction and pattern matching.
The compiler knows the byte length of a string literal at compile time:

```expo
# Construction: embed a string literal in a binary
packet = <<0x01, "hello", 0x00>>

# Pattern matching: match a string literal prefix
match data
  <<"HTTP/1.1 ", rest: Binary>> -> parse_response(rest)
  _ -> error("not HTTP")
end
```

String variables cannot appear in `<<>>` because their length is not known at
compile time. Use `.to_binary()` and `<>` concatenation instead.

### No Char type

Expo has no dedicated character type. A single-codepoint `String` serves the
same purpose -- fractal design means the same type works at every scale.

- `String.at(n)` returns `Option<String>` (a single-codepoint string).
- Classification methods (`is_alpha?()`, `is_digit?()`, `is_whitespace?()`)
  live on `String`. Called on `"a"`, they classify one codepoint. Called on
  `"hello"`, they check whether all codepoints match.
- `String.codepoints()` iterates individual codepoints as single-codepoint
  strings.
- `String.graphemes()` iterates grapheme clusters (what users see as
  "characters") as strings.

String ranges (`"a".."z"`) work as codepoint ranges in match patterns:

```expo
match ch
  "a".."z" | "A".."Z" | "_" -> lex_ident()
  "0".."9" -> lex_number()
  "(" | ")" | "[" | "]" -> single_token(ch)
  _ -> error("unexpected: #{ch}")
end
```

Range endpoints must be single-codepoint string literals. `"a".."z"` matches
any single-codepoint string whose codepoint value falls within the range.
Multi-character string ranges are a compile error.

OR patterns (`|`) combine multiple patterns in a single match arm. If any
sub-pattern binds a variable, all sub-patterns must bind the same name with
the same type.

### Ranges

Expo has one range operator: `..`, always inclusive on both ends.

```expo
# Pattern matching -- the primary use case
match ch
  "a".."z" -> :lowercase       # includes "z"
  0..255 -> :byte               # includes 255
end

# Iteration
for i in 1..10                  # 1 through 10, ten iterations
for c in "a".."z"               # all 26 lowercase letters

# Exclusive end when needed -- just subtract
for i in 0..n-1                 # 0 through n-1
```

One operator, one behavior. Inclusive ranges are natural for pattern matching
(the most common use case) and for readable numeric sequences (`1..10`).
Numeric loops like `for i in 0..n` are rare in idiomatic Expo -- most loops
iterate collections directly (`for item in list`). The occasional `0..n-1`
is explicit about intent and doesn't justify a second operator.

Integer ranges and single-codepoint string ranges are both valid. Integer
ranges produce `Int` values; string ranges produce single-codepoint `String`
values ordered by codepoint number.

---

## Bitwise operations

Bitwise operations are methods, not symbol operators. Expo uses `<<>>` for
binary literals, and keeping `<<`/`>>` free of any other meaning avoids the
collision that C-family languages have between shift operators and binary
delimiters. All other bit-manipulation symbols (`&`, `|`, `^`, `~`) stay
unused -- `&` doesn't exist in Expo (see `archive/20260323-MEMORY.md`), `|` is reserved for
union types, and `^` is available for a future pin operator.

Instead, bitwise operations are defined by the `Bitwise` protocol:

```expo
protocol Bitwise
  fn band(self, other: Self) -> Self
  fn bor(self, other: Self) -> Self
  fn bxor(self, other: Self) -> Self
  fn bnot(self) -> Self
  fn bsl(self, n: Int) -> Self
  fn bsr(self, n: Int) -> Self
end
```

`Int` is the built-in implementation:

```expo
flags.band(0x01) != 0              # bitwise AND
flags.bor(0x08)                    # bitwise OR
a.bxor(b)                         # bitwise XOR
flags.bnot()                      # bitwise NOT
a.bsl(4)                          # bit shift left
a.bsr(2)                          # bit shift right
```

Because these are protocol methods, any struct can implement `Bitwise`:

```expo
struct Permissions
  value: Int
end

impl Bitwise for Permissions
  fn band(self, other: Permissions) -> Permissions
    Permissions{value: self.value.band(other.value)}
  end
  fn bor(self, other: Permissions) -> Permissions
    Permissions{value: self.value.bor(other.value)}
  end
  fn bxor(self, other: Permissions) -> Permissions
    Permissions{value: self.value.bxor(other.value)}
  end
  fn bnot(self) -> Permissions
    Permissions{value: self.value.bnot()}
  end
  fn bsl(self, n: Int) -> Permissions
    Permissions{value: self.value.bsl(n)}
  end
  fn bsr(self, n: Int) -> Permissions
    Permissions{value: self.value.bsr(n)}
  end
end

read = Permissions{value: 4}
write = Permissions{value: 2}
read_write = read.bor(write)
has_read = read_write.band(read).value != 0
```

This follows the same fractal design as `ListLiteral` for `[]` and
`PairLiteral` for `(a, b)` -- built-in types and user-defined types have
identical capabilities.

---

## Pattern matching

Binary patterns destructure raw data into typed bindings. They are only valid
in `match` arms -- never in bare `=` assignments. This is a deliberate design
choice: binary pattern matching is inherently refutable (the data might not be
long enough), and a statically typed language should not silently panic on a
failed pattern.

### Segment forms in patterns

- **Integer literal** -- matches a specific value: `0::8`, `0xFF::16`.
- **Name with bit size** -- binds a variable as `Int`: `flags::8`, `stream_id::31`.
- **Name with type** -- binds a variable as a concrete type: `padded: Bool`, `port: Int16`.
- **Discard** -- skips bits: `_::1`, `_::24`.
- **Greedy rest** -- captures the remainder: `rest: Binary` or `rest: Bits`.

All names in binary patterns are bindings, never constant lookups. There is no
ambiguity between "match this value" and "bind this name" -- literals match,
names bind.

### Rules

- At most **one greedy segment** per pattern, and it must be the **last**
  segment.
- Segment sizes must be **integer literals** -- no runtime variables. To extract
  a variable-length payload, match the fixed-width header, then use `.take()`
  and `.drop()` on the remainder:

```expo
match buffer
  <<length::24, type::8, rest: Binary>> ->
    payload = rest.take(length)
    remaining = rest.drop(length)
    handle(type, payload)
    process(remaining)
  _ ->
    need_more_data()
end
```

- `: Binary` greedy rest requires the preceding fixed segments to sum to a
  **multiple of 8 bits**. `<<flag::1, rest: Binary>>` is a compile error -- 1
  bit is not byte-aligned. Use `: Bits` for non-aligned captures:
  `<<flag::1, rest: Bits>>`.

- Binary matches always require a **catch-all `_` arm**. Binary data is
  dynamically sized; there is no way to statically guarantee that a pattern
  covers all inputs. The catch-all handles the "not enough data" case
  explicitly.

### Binding types

Each segment form produces a specific type for the bound variable:

- `name::N` -- `Int` (always the default integer type, regardless of N).
- `name::N byte` -- `Binary` (N bytes of raw data).
- `name: Bool` -- `Bool` (1-bit boolean).
- `name: Int8` / `Int16` / `Int32` / `Int64` -- the corresponding integer type.
- `name: Float32` -- `Float32` (32-bit IEEE 754 float).
- `name: Float64` -- `Float64` (64-bit IEEE 754 float).
- `name: Binary` -- `Binary` (greedy rest, byte-aligned).
- `name: Bits` -- `Bits` (greedy rest, arbitrary alignment).

Union types (`Int`, `Float`) are not valid in `: Type` position because they
have no fixed bit width.

---

## Type checking

The type checker enforces structural validity of binary patterns at compile
time. It does not -- and cannot -- verify that the data is long enough at
compile time. Length is a runtime property, handled by the required catch-all
arm.

**Static checks:**

- The match subject must be typed `Binary` or `Bits`.
- Each pattern is well-formed: valid sizes, valid modifier combinations, at most
  one greedy segment at the end.
- `: Binary` greedy rest requires byte-aligned fixed prefix.
- Each bound variable gets the correct type (see binding types above).
- The total fixed bit-width of each pattern is computed at compile time.

**Codegen:**

The compiler computes the total fixed-size prefix of each pattern arm at compile
time. For each arm, it emits a single "is the binary at least N bytes/bits?"
check at the top. If the check fails, the arm is skipped entirely -- no
incremental "do I have enough for the next segment?" logic.

---

## Runtime representation

### Initial implementation

Copies. When a binary pattern captures `rest: Binary`, the runtime allocates a
new buffer and copies the remaining bytes into it. Simple and correct. The API
surface is the same regardless of internal representation.

### Target: arena-backed views

The end state is zero-copy views into arena-allocated memory. A `Binary` value
becomes a triple:

```
Binary = { ptr: *const u8, offset: usize, length: usize }
```

Pattern matching creates a new view triple -- increment offset, decrement
length, done. O(1), no allocation, no copy.

This works because of Expo's process model:

- Each process has its own arena. Binary data received from a socket or read
  from a file is allocated in the process's arena.
- Pattern matching creates views within the same arena. All views share the
  arena's lifetime.
- Cross-process sends copy at the boundary. This is already how Expo handles
  all process-to-process data transfer.
- When the arena is freed (process exits, request completes), all views and
  backing buffers are freed together. No reference counting, no dangling views.

The transition from copies to views is invisible to user code -- no API changes,
no new methods, no behavioral difference. The programmer never sees `compact`,
`slice`, or `view` -- just `Binary`.

---

## Example: HTTP/2 frame parsing

HTTP/2 frames have a fixed 9-byte header: 24-bit length, 8-bit type, 8-bit
flags, 1 reserved bit, 31-bit stream ID. Binary pattern matching parses this
directly, with the frame type as a literal constant in each arm:

```expo
fn process_buffer(socket: Socket, buffer: Binary) -> Unit
  match buffer
    <<_::24, 0::8, flags::8, _::1, sid::31, rest: Binary>> ->
      # DATA frame
      print("DATA stream=#{sid}")
      process_buffer(socket, rest)

    <<_::24, 1::8, flags::8, _::1, sid::31, rest: Binary>> ->
      # HEADERS frame
      print("HEADERS stream=#{sid}")
      process_buffer(socket, rest)

    <<_::24, 4::8, flags::8, _::1, _::31, rest: Binary>> ->
      # SETTINGS frame
      if flags.band(0x01) != 0
        print("SETTINGS ACK")
      else
        print("SETTINGS")
      end
      process_buffer(socket, rest)

    <<_::24, 6::8, _::8, _::1, _::31, _::64, rest: Binary>> ->
      # PING frame (always 8 bytes payload)
      print("PING")
      process_buffer(socket, rest)

    <<_::24, 7::8, _::8, _::1, _::31, _::1, last::31, err::32, rest: Binary>> ->
      # GOAWAY frame
      print("GOAWAY last=#{last} error=#{err}")

    <<_::24, 8::8, _::8, _::1, sid::31, _::1, inc::31, rest: Binary>> ->
      # WINDOW_UPDATE frame
      print("WINDOW_UPDATE stream=#{sid} increment=#{inc}")
      process_buffer(socket, rest)

    <<>> ->
      print("connection closed")

    _ ->
      # incomplete frame -- read more data and retry
      new_data = socket.read()
      process_buffer(socket, buffer <> new_data)
  end
end
```

Each arm matches a specific frame type byte as a literal (`0::8` for DATA,
`1::8` for HEADERS, etc.), so frame dispatch and header destructuring happen in
one step. The `_` catch-all handles incomplete data by reading more from the
socket and recursing. Variable-length payloads are extracted with `.take()` and
`.drop()` on the `rest` binary inside the arm body.

Sub-byte fields work inline. The `_::1, sid::31` sequence extracts a 1-bit
reserved field and a 31-bit stream ID from a single 32-bit word -- the compiler
generates the same shift-and-mask code you'd write by hand.

---

## Deferred features

These are intentionally excluded from the initial implementation. Each has a
clear path to being added later without breaking existing code.

**Variable-size segments in patterns.** Segment sizes like `payload::length byte`
where `length` is a variable bound earlier in the same pattern. Excluded because
the implicit back-reference is subtle and error-prone -- it looks identical to a
literal size but has fundamentally different semantics. If added later, it will
use explicit syntax (e.g., `payload::{length} byte` or `payload::size(length) byte`)
to make the reference visually distinct.

**Constant matching in patterns.** Using named constants as match values (e.g.,
`FRAME_DATA::8` instead of `0::8`). Excluded because distinguishing "match this
constant" from "bind this name" requires either a naming convention the compiler
can't enforce or a pin operator. If demand emerges, a pin syntax like
`^FRAME_DATA::8` is the likely path.

**`BinarySegment` protocol.** A protocol that lets user-defined types participate
in binary pattern matching as `: Type` annotations. A type implementing
`BinarySegment` would define its bit size (known at compile time) and
encode/decode logic, enabling patterns like `<<addr: IPv4, ttl::8, rest: Binary>>`
or `<<id: UUID, version::8, rest: Binary>>`. This subsumes the `FromBinary` /
`ToBinary` serialization protocols mentioned in the roadmap -- it's the general
version, integrated directly with binary literal syntax. Fixed-size types (IPv4,
UUID) are straightforward; variable-size types (UTF-8 codepoints, varints) would
need a separate variable-width variant of the protocol.

**`: UTF8` segment type.** UTF-8 codepoints are variable-width (1-4 bytes), so
they can't have a compile-time-known bit size. This makes `: UTF8` incompatible
with the current design, which requires all non-greedy segments to have fixed
sizes. UTF-8 decoding in binary patterns will be handled through the
variable-width `BinarySegment` protocol when it's designed.

---

See `ROADMAP.md` Phase 3 Track A1 for implementation milestones. See `archive/20260323-MEMORY.md`
for how ownership and arenas interact with binary data.
