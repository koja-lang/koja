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

## `koja shell` project mode

`koja shell` loads the current project by default. The global `-S <path>`
selector loads another project without changing the working directory. The
REPL loads the project's sources, path dependencies, and stdlib prelude so it
can call any package function. Known limitations:

- **Whole-program re-check per input.** Each prompt re-runs the entire
  baseline (stdlib + project + history) through the pipeline — the
  existing whole-program model, fine for small projects but linear in
  session length.
- **User FFI from the prompt needs a shared library.** The interpreter
  resolves a user-declared `@extern "C"` through the dynamic loader, so a
  `@link` library works at the prompt when it is a shared library on the
  loader's search path (`DYLD_LIBRARY_PATH` or `LD_LIBRARY_PATH`). The
  shell does not add the project root to that search the way `koja run`
  does. A library that exists only as a static archive does not load, and
  calling one of its externs errors with `RuntimeError::ExternUnresolved`,
  same as `koja run --backend=interpreter`.

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

## `IO.Descriptor` lacks random access, durability, and locking

Found 2026-08-09 while building embedded storage. `IO.Descriptor` reads
only move forward: there is no `seek` or positioned read, no `stat` or file size,
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

**Fix path:** one "descriptor random access and durability" pass.
Runtime shims for `pread`/`lseek`, `fstat`, `ftruncate`, `fsync` (with
the Darwin fcntl behind it), and `flock`, surfaced as
`IO.Descriptor.read_at`, `IO.Descriptor.size`,
`IO.Descriptor.truncate`, `IO.Descriptor.sync`, and
`IO.Descriptor.lock`/`try_lock`.

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
directory entries. Per-entry metadata can ride the `IO.Descriptor`
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

---

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

---

## Numeric literals are typed in two places

An integer or float literal gets its type from two mechanisms that do
not know about each other.

The literal resolver (`literals/scalar.rs`) takes an expected type, but
for an integer literal it only acts when that type is a user type that
implements `IntLiteral`. Then it dispatches through `from_int`. When the
expected type is a builtin width such as `UInt32`, the literal resolves
as `Int` and the expected type is ignored.

The slot owner then runs `check_compatible` (`coercion.rs`). It sees a
literal whose value fits the slot's width and stamps
`NumericLiteralWidth` on the node, so lowering emits that width while
`resolution` still says `Int`. Annotated locals, `==` and arithmetic in
`binary_type`, argument checks, and return checks each call it.

So a literal's type is decided by its consumer for the width types and
by the literal for `IntLiteral` carriers. Every new consumer has to
remember the second step. The `assert` operand bindings are the latest
site to do so (`resolve_comparison_operands` checks each operand for
`Compatible::Coerced` against the other operand's type).

**Fix path:** make the expected type the single owner of literal
typing. `resolve_scalar_literal` treats the builtin width types the way
it treats `IntLiteral` carriers: an expected `UInt32` yields resolution
`UInt32`, the range check runs there, and the node carries the same
`NumericLiteralWidth` stamp lowering already reads. `Compatible::Coerced`
then only serves consumers that resolve a literal before they know the
slot type, and each of those can thread an expected type and drop the
arm. `resolve_comparison_operands` loses its coercion check once this
lands.

The further step, a provisional `{integer}` type that unifies with the
first concrete width it meets and defaults to `Int` at the end of the
function, would remove hints and coercions both. Koja's resolver is a
single pass and local types are fixed at declaration, so that is a
different resolver, not a change to this one.

---

## `rescue` handlers cannot `return`

Found 2026-10-04 while splitting `File` out of `fd.koja`.
LANGUAGE.md says a `rescue` handler must produce the success type or
diverge, and names `fail` and a panic as the diverging forms. `return`
is the third diverging form and the compiler rejects it there with
"`return` is only valid as a statement". The parser reads the handler
with `parse_expr_bp(BP_RESCUE_R)` in `koja-parser/src/expr.rs`, so it
is one expression and never statement position, even though resolve
lowers it to a `match` arm body where `return` would be legal. The
same limit keeps a handler to one expression, so a handler that must
release a resource before it fails has no place to do so.

Consequence: `File.dir?` and `File.exists?` keep a four-line `match`
to turn a bad path into `false` where `rescue _ -> return false` reads
as the intent. `File.rename` keeps a `match` so it can free the first
`CString` before it fails on the second.

**Fix path (agreed 2026-10-04, to land as one branch):** the model
is the closure pair, a short form and a block form. The `->` in a
short closure and in `rescue e -> handler` introduces a one-statement
body, so both take an expression or a diverging `return`, `break`, or
`fail`. `fail` is "return on the error channel" and becomes
`Statement::Fail`, rejected by the parser in expression position the
way `return` is, instead of an `ExprKind` that typecheck rejects
late. `body_tail_type` treats `break` as divergent like `return`, so
a `match`, `if`, or `cond` arm may end in `break`. A block handler
for the multi-statement case comes after, with its own terminator
design. A stash on `fix/nested-type-unions` holds a first cut of the
`rescue` half (handler as `Box<Statement>`, parser dispatch on
`return` and `break`, tests, docs).

## `Map` iteration order differs between the backends

Found 2026-10-07 while writing the `tests/lang/io/log.kojs` golden.
`Map.next` documents its order as unspecified. The interpreter stores
a `Map` as a list of pairs in insertion order
(`koja-ir-eval/src/value.rs`). The native backend stores it as an
open-addressed hash table and walks the slots
(`koja-ir-llvm/src/intrinsics/map.rs`), so it iterates in hash order.
The two orders agree only for a map with one entry.

```koja
m = ["job": 1, "rows": 2, "host": 3, "zone": 4]
for (k, v) in m
  IO.write("#{k}=#{v} ")
end
# interpreter: job=1 rows=2 host=3 zone=4
# native:      rows=2 zone=4 job=1 host=3
```

Consequence: any output that walks a map differs by backend. The lang
golden suite compares stdout on both backends, so a golden that
prints a map must hold one key, and `tests/lang/io/log.kojs` does. A
`Log.Text` line with two attributes prints them in a different order
under `koja run` and under a built binary, so a log line is not
stable text across the backends and neither is a `Map` rendered
through `Debug`.

**Fix path:** decide whether the order is part of the language. If it
is, the native table gains an insertion-ordered entry list beside the
slot array, the shape `IndexMap` uses, and `next` walks the list. The
interpreter already has that order. If it is not, the `Debug` and
`Log.Text` renderers sort their keys so rendered text is stable, and
the golden rule stays. Either way the `Map` doc should say which.
