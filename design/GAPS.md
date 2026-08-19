# Known Compiler Gaps

Known limitations, bugs, and workarounds in the Koja compiler. New gaps
should be added here as they are discovered through tests, real programs, and
compiler audits.

---

## Iteration protocol limits (`Enumeration<T>`)

`Enumeration<T>` requires `length()` + `get(index)`, locking `for` to
index-based while loops. This precludes lazy iteration, streaming, and any
non-random-access collection (maps, linked lists, generators).

Pre-v1.0, replace with an `Iterator<T>` protocol using
`next(self) -> Option<(T, Self)>`. `get` now returns `Option<T>`.
Codegen change is contained to `compile_for` in `loops.rs`; List/String
impls wrap existing index-based access in iterator state.

The current `for` loop hides the `Option` from the user (unwraps
automatically since iteration is bounds-checked). With lazy iteration,
`Option` becomes the termination mechanism -- `for` desugars to
`loop { match iter.next() ... }` and `None` breaks the loop.

---

## No wrapping-arithmetic escape hatch

Integer arithmetic always traps on overflow (2026-07). There are no
`wrapping_add` / `wrapping_mul` style operations, and the Erlang idiom
of masking after the math does not transfer, since the operation traps
before a `band` can run. Consequence: a 64-bit wrapping multiply is
inexpressible in pure Koja, which locks out most non-cryptographic hash
functions (FNV, xxHash, SplitMix). 32-bit wrapping can be simulated by
computing in `Int` and masking.

**Fix path:** a named-operation family on the integer types, following
the `Bitwise` precedent (the specialized algebra gets words, not
symbols). Both backends already thread the operand type through
`BinaryOp`, so codegen is the existing arithmetic minus the overflow
guard.

---

## `CPtr` float reads bypass the finite-only invariant

A non-finite float returned by an `@extern "C"` call traps at the call
site, but `CPtr<Float64>.read()` (and `Float32`) does not. A NaN or
infinity sitting in a C-filled buffer walks straight into a `Float`,
silently breaking the invariant. This is consistent with the FFI
stance that safety is the wrapper author's responsibility, but it is
currently undocumented rather than decided.

**Fix path options:** guard `read` (costs a check on the bulk-transfer
path), document `CPtr` as an unchecked boundary, or offer both a
checked and an unchecked read. A carrier type for full IEEE values
(the `Binary`-to-`String` pattern applied to floats) would subsume
this: pointer reads and extern returns typed as IEEE floats make no
finiteness promise, and the checked crossing moves to an explicit
conversion.

---

## No exponent notation in numeric literals

The lexer does not accept `1e9` / `1.5e-3`. Large or small float
literals must be written with every digit (the arithmetic-fault test
fixtures write 160-digit literals). `Float.parse("1e999")` handles the
notation at runtime, so the gap is literal syntax only.

**Fix path:** lexer support for an optional exponent suffix on float
literals, plus the same round-to-infinity `OutOfRange` check float
literals already get.

---

## `koja shell` project mode

`koja shell` auto-loads the project in the working directory (its `src`,
path dependencies, and the stdlib prelude) so the REPL can call any
package function. Known limitations:

- **No explicit project selector.** The shell detects the project from
  the current directory only; there is no `-S <path>` flag yet to point
  it elsewhere. The [roadmap](ROADMAP.md#ecosystem-validation) treats
  this as an optional improvement driven by use.
- **Whole-program re-check per input.** Each prompt re-runs the entire
  baseline (stdlib + project + history) through the pipeline — the
  existing whole-program model, fine for small projects but linear in
  session length.
- **No FFI from the prompt.** Calling an `@extern "C"` function errors
  with `RuntimeError::Unsupported`; the interpreter has no FFI, same as
  `koja run --backend=interpreter`.
- **`Global` self-edit inconsistency.** `ProjectLoader` skips any stdlib
  package whose name matches the project (its `seen_packages` set), so a
  project named like a stdlib package — even `Global` — does not
  double-load. The one residual edge: running the REPL _inside_
  `koja/lib/global` loads the qualified stdlib packages (`Crypto`,
  `HTTP`, …, baked against the published `Global`) alongside the edited
  `Global`, since `ProjectLoader` does not replicate the
  `bundle_with_autoimport` rule that drops qualified sources on a
  `Global` self-compile. Only reachable when editing the stdlib itself.

---

## Inference and ergonomics warts from the pooler build

Found 2026-07-15 while building the `pooler` package (a generic
`Process` implementation, the first real one outside the stdlib). The
blocking bug is fixed. `spawn` on a generic process target was not
substituting the call site's type args into the conformance's `M`/`R`,
and the monomorphizer skipped `LValue.head_resolved_type` on field
assignments (regression coverage in
`tests/lang/generics/generic_process_spawn.kojs`). Also fixed
2026-07-16: `priv fn` helpers inside `impl Protocol for Type` blocks
were rejected despite LANGUAGE.md allowing them; the conformance check
now skips private members and only rejects public extras (regression
coverage in `tests/lang/protocols/priv_impl_helper.kojs`). Three
non-blocking warts remain, each with a workaround.

- **Generic enum unit variants don't infer from parameter types.**
  `consume(Signal.Done)` fails with "cannot infer type parameter `T`
  from unit variant `Done`" even though `consume`'s parameter is
  `Signal<T>` and `T` is bound in the enclosing scope. Payload
  variants at the same call site infer fine. Workaround is binding
  with an annotation first (`done: Signal<T> = Signal.Done`).
- **`x = match … end` doesn't cross-infer generic payloads.** Arms
  building `Result.Ok(true)` / `Result.Err("nope")` each fail to
  infer the sibling's type parameter when the match is assigned to a
  local, while the same match as a trailing expression (with the
  function return type as the expected hint) compiles. The arms could
  unify against each other. Workaround is restructuring so the match
  is in return position, or annotating the binding.
- **Nested enum patterns defeat exhaustiveness.** Splitting
  `Result.Err` by payload (`Result.Err(CallError.Timeout)` +
  `Result.Err(CallError.ProcessDown)`) reports "missing variant
  `Err`" because the checker doesn't combine nested coverage into
  coverage of the outer variant. Workaround is a `Result.Err(_)`
  catch-all arm with an inner match on the payload.

---

## Runtime: adjacent issues from the worker-migration TLS audit

Found while root-causing the 2026-07 Linux shutdown crash (a process
resuming on a different worker thread after socket I/O switched through
the old worker's cached TLS base; fixed with `#[inline(never)]`
barriers, see the note in `koja-runtime-posix/src/scheduler.rs`). One
neighbor remains open:

- **Reduction counter writes can land on the wrong worker (x86-64
  only).** Compiled x86_64 process code decrements the C thread-local
  `koja_reductions_left` inline, and LLVM may cache its address across
  a suspension point, so a migrated process keeps decrementing the
  previous worker's counter until the next runtime call. Consequence
  is mistimed yield checks (never memory unsafety). aarch64 closed
  this on 2026-08-04 by moving the budget into the reserved register
  `x26` (`koja-ir-llvm/src/reductions.rs`), which rides the process
  context through migration and also removed the macOS `tlv_get_addr`
  cost that made yield checks half of `fib(35)`'s runtime. The x86-64
  register fix waits on LLVM's `+reserve-r8..r15`, which landed after
  the LLVM 22 branch.

---

## Builtins inherit zero-field struct derives

Found 2026-08-08 while auditing the `builtin` migration.
`derive_debug` and `derive_equality` synthesize the same impls for a
`builtin` declaration that a zero-field struct gets. Builtins with
explicit stdlib impls (the scalars, `String`, container and `CPtr`
`Debug`) never hit the synthesis, but the holes are live:

- `List`, `Map`, `Set`, and `CPtr` have no explicit `Equality` impl,
  so the derived `eq` compares zero fields and returns `true` for
  every pair: `[1, 2] == [3]` evaluates to `true` today. This
  pre-dates the `builtin` keyword, since the types were zero-field
  structs before.
- `Int64`, `Float64`, `Never`, and `Unit` lean on synthesized derives
  only for conformance. At runtime the IR's `Int64`-onto-`Int` method
  collapse routes to the real intrinsic impls.

**Fix path:** delete the builtin arms from both derive passes and add
explicit stdlib impls. Collection `eq` needs an element-wise walk and
a `T: Equality` requirement on the impl target, which the language
cannot spell yet. The conformance-only holes need explicit impls or
a rule that a builtin satisfies bounds by shape.

---

## Protocol conformances stop at the package boundary

Found 2026-08-09 while designing an `Encodable` protocol for the
`messagepack` package. Two restrictions in `register_impl`
(`koja-typecheck/src/pipeline/collect.rs`) block the serde pattern,
where a codec package defines a protocol, implements it for the
stdlib vocabulary types, and applications implement it for their own
types:

- `impl P for T` requires `T` to live in the impl's package. A local
  protocol cannot be implemented for `String`, `Int`, or any other
  foreign type. The inverse direction (foreign protocol, local type)
  already works and carries every `Process` conformance.
- Generic impl targets are rejected outright, even with concrete
  arguments (`impl Encodable for List<Value>`).

`extend` accepts cross-package targets but grants methods, not
conformance facts, so bounds like `T: Encodable` never see them.
Without the stdlib impls the protocol is useless, so every
codec-shaped package (`MessagePack.Encodable`, a future
`JSON.Encodable`, an ORM's `Row`) is blocked on the same wall. The
collection-`eq` hole in the builtin-derives entry above waits on the
same missing spelling.

**Fix path,** in three steps of increasing size:

1. Relax the orphan rule to Rust's: an impl is legal when the
   protocol or the target is local. Coherence survives, since only
   two packages can ever write a given `(protocol, type)` impl, and
   the whole-program conformance table detects that collision at
   typecheck time.
2. Accept concrete generic impl targets, keying conformance facts by
   instantiation.
3. Conditional conformance (`impl Encodable for List<T>` requiring
   `T: Encodable`), discharged per instantiation during
   monomorphization like existing function bounds.

Step 1 needs a policy for dot-call ambiguity when two packages add
same-named protocol methods to one foreign type. Resolving through
the bound only (no bare dot-call on foreign conformances) is the
conservative default.

---

## Aggregate arguments ride LLVM's unstable first-class ABI

Found 2026-08-04 when the yield-check register intrinsics made union
fixtures fail at `-O0` on aarch64. Compiled functions pass every
struct, tuple, enum, and union as a first-class LLVM aggregate value.
LLVM lowers such an argument by splitting it into one piece per leaf
field, and that lowering is a codegen convention, not a stable ABI.
Two consequences:

- **Correctness (mitigated).** GlobalISel and SelectionDAG disagree on
  the stack placement of byte-sized pieces on Darwin (1-byte slots vs
  4-byte slots). At `-O0` LLVM picks GlobalISel per function and falls
  back to SelectionDAG for functions it cannot select, so one module
  could mix both and corrupt aggregates at call boundaries. The old
  union type `{ i8, [N x i8] }` split entirely into byte pieces and
  was the visible casualty. `object.rs` now pins `-global-isel=0` so
  every function uses one selector. Any type with a `Bool`, `Unit`, or
  `Int8` field still produces byte pieces, so the pin must stay until
  the ABI changes.
- **Cost (mostly mitigated).** Splitting is wasteful for byte-layout
  aggregates. Under the old union shape, a non-inlined call passing
  an 18-byte union spent roughly 25 instructions scattering bytes
  into eight registers and ten stack slots, and the callee
  reassembled them one `ldrb` at a time. The 2026-08-05 reshape to
  `{ i64, [M x i64] }` cut that to a few word moves. Other aggregates
  with byte-sized fields still split poorly.

**Fix path:** lower aggregate arguments in our emit layer instead of
leaning on LLVM's splitting, the way clang lowers C structs. Coerce
small aggregates to `[N x i64]` chunks and pass large ones indirectly
through a caller-owned temporary. The interim union-only step landed
2026-08-05: union outers are now `{ i64, [M x i64] }` (tag widened to
a word, payload chunked to words), which removed the worst splitter
and aligned payload accesses. The selector pin and the byte-piece
hazard for other types remain until the general lowering lands.

---

## `Fd` lacks random access, durability, and locking

Found 2026-08-09 while building embedded storage. `Fd` reads only move
forward: there is no `seek` or positioned read, no `stat` or file size,
and no `truncate`. Any page-oriented file format (an on-disk B-tree, an
archive reader, a large-file parser) must instead load the whole file
with `File.read_binary`, which caps the dataset at available memory.
Two adjacent holes make the durability story worse:

- **No `sync()`.** A store that commits cannot flush the OS page cache
  without an FFI `fsync`. The stdlib should own this one because the
  obvious FFI call is wrong on macOS, where `fsync(2)` does not reach
  stable storage and the real flush is the `F_FULLFSYNC` fcntl. Every
  independent wrapper will miss that.
- **No advisory locks.** A store cannot enforce single-process
  ownership of its file. Two processes opening the same database
  corrupt it silently, and `flock` is unreachable without FFI.

**Fix path:** one "Fd random access and durability" pass. Runtime
shims for `pread`/`lseek`, `fstat`, `ftruncate`, `fsync` (with the
Darwin fcntl behind it), and `flock`, surfaced as `Fd.read_at`,
`Fd.size`, `Fd.truncate`, `Fd.sync`, and `Fd.lock`/`try_lock`.

---

## No ordered map, and `Map` cannot be iterated

`Map` exposes `get`, `put`, `remove`, `has?`, `length`, and `empty?`.
There is no `keys`, `entries`, or fold, so a value stored in a `Map`
can never be enumerated (the iteration half is the `Enumeration<T>`
gap above wearing a different hat). And nothing in the stdlib keeps
keys sorted, so ordered workloads (range scans, expiry queues,
schedulers, routing tables) each hand-roll a persistent balanced tree.
Writing one in user code works, the AA tree is about 200 lines, but
every package that needs ordering will duplicate it.

**Fix path:** give `Map` an iteration surface once the iterator
protocol lands, and incubate a `SortedMap` (persistent balanced tree,
`Comparable` keys, range selection) as a package with an eye toward
stdlib promotion after a second consumer appears.

---

## `Binary` has no ordering and no endian helpers

`Binary` derives `eq` and `hash` but no comparison, so bytewise key
ordering is a manual `at`-loop in user code. The same codecs that need
ordering also re-roll big-endian integer packing: an append-N-bytes
helper and an accumulate-N-bytes reader now exist in at least two
packages, character for character. The float side of the same story is
the pure-arithmetic IEEE 754 decomposition workaround: without
`Float.to_bits`/`Float.from_bits` intrinsics, any binary format that
carries floats ships a hand-written bit-extraction module.

**Fix path:** `compare` on `Binary` (and a `Comparable` conformance
when the protocol exists), `Int.to_be_bytes(width)` with a matching
`Binary.read_be(offset, width)`, and `Float.to_bits`/`from_bits`
intrinsics. All are small, self-contained stdlib additions.

---

## No non-cryptographic checksum

`Crypto` covers SHA256 and HMAC, but storage and wire formats want a
cheap integrity check (CRC32, CRC32C, xxHash) for frame validation.
Table-driven CRC32 is easy to write in Koja and every format
re-implements it, byte-looped and slow compared to hardware CRC32C or
a slice-by-8 implementation the runtime could provide. xxHash is out
of reach entirely until wrapping arithmetic lands (see the
wrapping-arithmetic gap above).

**Fix path:** a `Checksum` module (or a `Crypto` sibling) with CRC32
and CRC32C backed by runtime intrinsics. Revisit xxHash once wrapping
multiplication exists.

---

## Sockets have no deadlines

Found 2026-08-10 while building a TCP request-response protocol.
`TCPSocket.read_binary`, `write`, `connect`, and `TCPListener.accept`
block with no timeout parameter and no way to bound the wait. The
only escapes are `try_accept` (accept only) and restructuring around
`Fd.watch`. Consequences for any wire protocol:

- A peer that stalls mid-frame (or a half-open connection after a
  crash) blocks the owning process forever. There is no way to
  express "read, but give up after N ms".
- `connect` to a black-holed address waits for the OS-level timeout,
  which can be minutes.
- Every timeout strategy degenerates to dedicating a process to the
  blocking call and abandoning it, which leaks the process and the
  socket.

**Fix path:** deadline variants on the socket surface
(`read_binary(count, timeout_ms)`, `connect(host, port, timeout_ms)`,
`accept(timeout_ms)`), shimmed on `SO_RCVTIMEO`/`SO_SNDTIMEO` and a
nonblocking connect with a poll. `Fd.watch` already proves the
runtime can wait on readiness with a bound.

---

## `DateTime` has no calendar formatting or parsing

Found 2026-08-10 while building a JSON API that exchanges RFC 3339
timestamps. `DateTime` carries epoch milliseconds and arithmetic,
but there is no way to render a calendar date ("2026-08-10T14:00:00Z")
or parse one back. Any service with a JSON surface needs both
directions on day one, so the civil-calendar math (days-to-date,
leap years, month lengths, UTC offsets) gets re-implemented from
Howard Hinnant's algorithms in user code, along with a hand-rolled
parser and its validation table.

**Fix path:** `DateTime.to_rfc3339()` and `DateTime.from_rfc3339(text)`
in the stdlib, over an internal civil-date conversion. A general
format-string API can wait. RFC 3339 alone covers the JSON world.

---

## No UUID generation

Found 2026-08-10. Public APIs hand out UUIDs as resource
identifiers, and every service re-rolls v4 generation from
`Random.bytes(16)` plus manual hex slicing to place the version and
variant bits. The pieces exist (`Random.bytes`, `Base.encode16`),
but the assembly is fiddly enough to deserve one blessed
implementation.

**Fix path:** `UUID.v4() -> String` (and a `UUID.v7()` sibling for
sortable identifiers) in the stdlib, either under `Random` or as a
small `Global` type.

---

## `IPAddress` has no string rendering

Found 2026-08-10 while running DNS peer discovery inside Docker.
`Net.Socket.resolve` returns `IPAddress` values, but the type offers
no way to render one as a dialable string: no `to_string`, and
string interpolation falls back to the derived `Debug` format, so
`"#{ip}:9993"` produces `IPAddress{bytes: <<192, 168, 228, 2>>}:9993`.
Nothing can dial that, and the mistake type-checks: the code reads
fine and fails only at runtime, off the happy path. Every consumer
of `resolve` re-implements dotted-quad and colon-hex rendering from
the raw bytes.

**Fix path:** `IPAddress.to_string()` in the stdlib: dotted-quad for
v4, RFC 5952 for v6. Its inverse (`IPAddress.parse(text)`) is the
natural sibling.
