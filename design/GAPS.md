# Known Compiler Gaps

Known limitations, bugs, and workarounds in the Koja compiler. New gaps
should be added here as they are discovered through tests, real programs, and
compiler audits.

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

## No carrier type for full IEEE floats

Both FFI boundaries now trap on a non-finite float: an `@extern "C"`
return and `CPtr<Float64>.read()` (and `Float32`). That keeps the
finite-only `Float` invariant, but it makes C APIs that use NaN as a
legitimate value unusable. `strtod` on bad input, statistics
libraries that mark missing data with NaN, and audio or GPU buffers
that carry inf by design all fault before user code can inspect the
value.

**Fix path:** a `CFloat64` / `CFloat32` builtin that carries the raw
IEEE bit pattern, on the `CString`-to-`String` model. It offers
`to_float() -> Float ! NonFinite` as the only way into `Float`, plus
`nan?`, `infinite?`, `bits`, and `from_bits`, with `Equality` and
`Hash` by bit pattern. No arithmetic and no ordering, so it stays a
transport type instead of a second float with NaN semantics. An
extern declared `-> CFloat64` and a `CPtr<CFloat64>.read()` skip the
guard, while the `Float64` spellings keep it. Build it when a real
FFI consumer hits the trap.

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

`koja shell` loads the current project by default. The global `-S <path>`
selector loads another project without changing the working directory. The
REPL loads the project's sources, path dependencies, and stdlib prelude so it
can call any package function. Known limitations:

- **Whole-program re-check per input.** Each prompt re-runs the entire
  baseline (stdlib + project + history) through the pipeline — the
  existing whole-program model, fine for small projects but linear in
  session length.
- **No user FFI from the prompt.** The interpreter dispatches stdlib
  externs through a hand-written table, so stdlib FFI works at the
  prompt. Calling a user-declared `@extern "C"` function errors with
  `RuntimeError::ExternNotSupported`, same as
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

## Protocol conformance residuals

Found 2026-08-09 while designing an `Encodable` protocol for the
`messagepack` package. The package boundary fell on 2026-08-21:
`impl P for T` accepts any protocol and any type, with no orphan
rule. A codec package can implement its protocol for `String` and
`Int`, and an adapter package can conform one dependency's type to
another dependency's protocol (the Elixir
`Jason.Encoder`-glue-package precedent, which a Rust-style
protocol-or-target-local rule would forbid). Coherence is
whole-program collision detection instead of a source restriction:
the conformance table rejects duplicate `(protocol, type)` impls,
and cross-package impl methods register under the target's
namespace like `extend` does, so bare dot-call works and two
packages adding one same-named method to a foreign type collide at
registration. The bound-only resolution policy sketched earlier
would have needed a new identifier shape plus mono routing, so it
lost to the `extend` precedent. Known residual: when two
dependencies both ship the same impl (say a protocol's package
catches up to an adapter package), the app cannot compile the pair
until the adapter updates. An app-level "prefer this impl" override
can close that hole later without changing today's semantics.

Instantiation keying landed the same day: conformance facts carry a
`Parameterized`/`Concrete` scope, so `impl Encodable for
List<Value>` conforms only `List<Value>`, bound discharge matches
the full instantiation, and duplicate detection is per
instantiation. Known residual: two concrete impls of one protocol
for different instantiations of one type still collide on method
names in the flat `[Type, method]` namespace, so one concrete impl
per `(type, protocol)` is the practical limit until methods key by
instantiation too.

Conditional conformance landed 2026-08-21: `impl Encodable for
List<T: Encodable>` attaches per-param bounds to the
`Parameterized` scope, typecheck discharges them per instantiation
and recursively (`List<List<Int>>` follows from `Int`), and the
impl body dispatches through its own condition. Parameterized
targets now also work outside the target's own package. The stdlib
spells `impl Equality for List<T: Equality>` with an element-wise
`equals?`, closing the list equality hole in the builtin-derives
entry above.

**What remains:** targets mixing type parameters with concrete args
(`impl P for Map<String, V>`) are rejected everywhere, and the two
residuals above stay open. The residuals are the same impl arriving from two
dependencies and one concrete impl per `(type, protocol)`.

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
  was the visible casualty. `target.rs` now pins `-global-isel=0` so
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

## No ordered map

`Map` exposes `get`, `put`, `remove`, `has?`, `length`, and `empty?`.
It also supports finite iteration, but it does not keep keys sorted.
Ordered workloads such as range scans and expiry queues need a separate type.

**Fix path:** incubate a `SortedMap` package with comparable keys and range
selection. Consider stdlib promotion after a second consumer appears.

---

## `Binary` has no ordering and no endian helpers

`Binary` derives `equals?` and `hash` but no comparison, so bytewise key
ordering is a manual `at`-loop in user code. The same codecs that need
ordering also re-roll big-endian integer packing: an append-N-bytes
helper and an accumulate-N-bytes reader now exist in at least two
packages, character for character. The float side of the same story is
the pure-arithmetic IEEE 754 decomposition workaround: without
`Float.bit_pattern`/`Float.from_bit_pattern` intrinsics, any binary format that
carries floats ships a hand-written bit-extraction module.

**Fix path:** `compare` on `Binary` (and a `Comparable` conformance
when the protocol exists), `Int.to_be_bytes(width)` with a matching
`Binary.read_be(offset, width)`, plus `Float.bit_pattern` and
`Float.from_bit_pattern` intrinsics. The `Float32` forms use `UInt32` instead of
`UInt64`. All are small, self-contained stdlib additions.

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

## No subprocess execution and no directory listing

Found 2026-08-28 while building `git_hygiene`, a CLI that shells out
to `git`. The runtime has no intrinsic to start a child process and
none to read a directory. `koja_file_*` covers read, write, mkdir,
rename, delete, and existence checks, but not readdir. `System`
covers env and hostname only. Consequence: a tool that orchestrates
other programs or walks a file tree is inexpressible in pure Koja.
The workaround is `@extern "C"` bindings to `popen`, `fread`, and
`pclose`, with directory walking pushed into `find` through the
shell. That works, but it makes libc the real stdlib for CLI work.

**Fix path:** two intrinsic families. `System.cmd(program, args)`
returns captured output plus exit status and must park the calling
process rather than block a scheduler thread. `File.ls(path)` returns
directory entries. Per-entry metadata can ride the `Fd`
random-access pass tracked above, which already owns `stat`.

---

## Unique composites are rebuilt on field write

Found 2026-09-03 while making `<>` consuming. `p.x += dx` on a struct
or enum builds a fresh composite and releases the old one even when
the old value provably dies at the write and no other binding shares
it. Small structs copy cheaply, but a struct holding a heap field, or
an enum variant with a large payload, pays an allocation and a
release per field write in a rebind loop.

**Fix path:** the two building blocks exist. `ConsumingSite` in
`koja-ir/src/elaborate/consume.rs` matches an instruction whose
receiver dies there (collection mutators and byte `<>` today), and
`grow_unique_block` in `koja-runtime-posix/src/util.rs` is the
`rc == 1` gate that lets a runtime helper reuse a block in place. A
`FieldSet` site would add the third arm: flag the instruction when the
base value dies there, and have the backend write the field into the
existing block when its refcount is one.

---

## Toolchain and stdlib nits from the `git_hygiene` build

Found 2026-08-28. None blocking, each with a workaround:

- **`List` has no `sort`.** Ordered output needs a hand-rolled
  insertion sort or a shell-side `sort`. A comparator-closure
  `sort` works today. A `Comparable` conformance can follow when
  the protocol exists (see the `Binary` ordering entry).
- **`IO.gets` cannot distinguish end of input from an empty
  line.** Both return `""`, so a line-oriented filter reading
  stdin cannot tell where input stops. Workaround is reading
  `STDIN` directly and treating an empty read as end of input.
  0.19 changes `gets` to return `Option<String>` over a
  caller-supplied reader, which also moves the `io_gets` lang
  fixture into the stdlib test suite.

---

## Function references cannot be default field values

Found 2026-09-01 in the same `trail` conversion. Default field
values are limited to literals, negated numerics, unit enum
variants, binary literals, and literal collections. A `fn` typed
field cannot default to a named function:

```koja
struct Config
  error_response: fn (Status, String) -> Response = &plain_error/2
  # error: default field values are limited to literals ...
end
```

Consequence: a config struct that carries function hooks cannot use
the struct-literal-with-defaults idiom, so it keeps a `new` plus
`with_` builder only to supply hook defaults. Pure data configs get
the literal idiom (`port: Int = 5432` in postgres-koja), so the
construction idiom splits on whether a struct holds functions. The
other spelling, hooks as `Option<fn>` fields defaulting to
`Option.None`, works since
[#94](https://github.com/koja-lang/koja/issues/94) closed, at the
cost of a `match` at every call site.

**Fix path:** allow `&name/arity` references as default field
values. A function reference resolves statically to a known
function, carries no evaluation order questions, and keeps the
"defaults are data" property.

---

## `koja doc` does not render protocol conformances

Found 2026-09-07 in a stdlib doc audit. Doc extraction skips
`impl Protocol for Type` blocks entirely, so any `@doc` inside one
(for example the case-sensitivity note on `impl Equality for URI`)
never reaches the HTML, `koja doc search`, or `koja doc <symbol>`.
Header conformances (`struct Point: Hash`) are not listed on the
type page either, and the requirement functions declared in the
type body render as ordinary functions with no link to the protocol
that owns their contract.

Consequence: a reader of `Float` cannot see that it conforms to
`Equality`, and a reader of `URI.equals?` cannot find the doc that
explains its behavior. The stdlib convention that follows from this
is that conformance functions carry no `@doc` unless the type's
behavior has a detail the protocol requirement cannot state.

**Fix path:** render a "Conforms to" section on each struct, enum,
and builtin page that lists every conformance from the header and
from `impl` blocks. Under each protocol, list the requirement
functions, use the implementation's `@doc` when it has one, and
fall back to the protocol requirement's `@doc` otherwise. Index
those entries in search so `koja doc URI.equals?` resolves.

---

## Enum variant patterns do not match through a union subject

Found 2026-09-07 while flattening test matches after structural
exhaustiveness landed. A variant pattern against a subject whose type
is a union that contains the variant's enum is rejected, even when
the variant name resolves to exactly one member:

```koja
match socket.upgrade_tls(host, config) # ! SocketError | TLSError
  Result.Err(TLSError.VerificationFailed(_)) -> ()
  # error: match arm pattern targets `Net.TLSError`, but the subject
  # has type `Error | TLSError`
  Result.Err(e) -> fail net_message(e)
end
```

Consequence: code that routes on one variant of one member of an
error union needs a typed member pattern first
(`Result.Err(tls_error: TLSError)`) and an inner `match` on the
binding. This is the same two-level shape that structural
exhaustiveness removed for plain enum payloads.

**Fix path:** when the subject is a union, resolve a variant pattern
to the member that declares that enum and narrow to it before the
variant test. Coverage then treats the arm as a partial cover of that
member, so `Result.Err(e)` after it still reads as the remaining
cases.

## `try` on a self-call is a tail call only at the return statement

Found 2026-09-07 while fixing stack growth in the JSON decoder. In a
`-> T ! E` function, `try X` at a return site (explicit `return try X`
or the trailing statement) forwards to `X` when `X` already produces
the function's own `Result<T, E>`, so a `try self(...)` there becomes
a real tail call. Two neighbouring shapes still desugar to the
unwrap-then-rewrap `match`, which hides the call from the tail-call
pass and grows the stack per step:

- `try self(...)` as the trailing expression of a `match` or `if` arm.
  A plain self-call in an arm is already a tail call, so adding `try`
  is what breaks it.
- A callee whose error type is a strict subset of the caller's. The
  widen is a real `UnionWrap`, so the site is not the identity.

Consequence: the failure mode is a data-dependent stack overflow that
compiles without a warning.

**Fix path:** resolve the arm case at the desugar by treating a `try`
that is the value of a return-position arm as a return site. For the
widening case either widen the `Result` as a whole with an operation
the tail-call pass recognizes, or teach the pass to see through a
`UnionWrap` on the error payload. A diagnostic for the remaining
shapes is the fallback if either proves large.

## Eval registers outlive their last use

Found 2026-09-11 while making tail-recursive accumulators linear
under the interpreter. An IR register is an SSA borrow with no
lifetime. An eval register owns an `Rc` clone of its value and stays
in the frame until the frame ends. The consuming twins gate in-place
mutation on `Rc::strong_count == 1`, so every register that still
holds the accumulator after its last use is a reason to copy.

The slot side is solved. Consume fusion emits `ConsumeLocal` before
the site and the interpreter clears the slot there. The register side
is covered by two rules in `release_dead_registers` in
`koja-ir-eval/src/interpreter.rs`, both scoped to a consuming site:

- Registers defined at or after the site in the same block are stale
  copies from an earlier pass over a loop body.
- In a block that ends in `Return` or `TailCall`, registers that
  share the receiver's storage and that nothing from the site onward
  reads. In `f(n - 1, acc.append(n))` these are the param register
  and its promotion `Clone`.

Consequence: a consuming site whose block does not exit the frame,
and whose stale holder was defined in an earlier block, still copies
under eval. Compiled code is unaffected. The rules also scan the
frame once per consuming site, which is a cost the IR does not have.

**Fix path:** a per-function last-use table for registers whose uses
all sit in their defining block, computed once and cached by symbol.
At the last use `lookup` becomes `remove`. The param register then
dies at its `Clone`, the clone at its `LocalWrite`, and the receiver
at the call, so the count is exactly one with no site-specific rules
and both rules above delete.
