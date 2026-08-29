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

`koja shell` loads the current project by default. The global `-S <path>`
selector loads another project without changing the working directory. The
REPL loads the project's sources, path dependencies, and stdlib prelude so it
can call any package function. Known limitations:

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

- `CPtr` has no explicit `Equality` impl, so the derived `equals?`
  compares zero fields and returns `true` for every pair.
- `Int64`, `Float64`, `Never`, and `Unit` lean on synthesized derives
  only for conformance. At runtime the IR's `Int64`-onto-`Int` method
  collapse routes to the real intrinsic impls.

**Fix path:** delete the builtin arms from both derive passes and add
explicit stdlib impls. The language can spell the conditional impl
now (see the next entry). The conformance-only holes need explicit
impls or a rule that a builtin satisfies bounds by shape.

---

## Derived protocols do not inspect generic arguments

Found 2026-08-23 while updating `auth_manager` for Koja 0.18.
Derived `Debug` and `Equality` detect a direct opaque field, such as a
function. They do not detect an opaque type inside a generic field,
such as `List<fn (Conn) -> Conn>`. The generated function can reach IR
lowering with an unsupported call on the function type and cause a
compiler panic.

The language must decide whether a derived protocol rejects the type
or omits fields that cannot conform. Omitting a field can make distinct
values compare as equal. Until this policy is defined, types with these
fields must provide explicit protocol functions.

**Fix path:** define the derivation policy, validate each generated
protocol call before IR lowering, and report a source diagnostic when
the type cannot derive the protocol.

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
(`impl P for Map<String, V>`) are rejected everywhere, `CPtr`
equality still rides the zero-field derive, and the two residuals
above stay open. The residuals are the same impl arriving from two
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
shell. That works, but it makes libc the real stdlib for CLI work
and forces the LLVM backend (see the next entry).

**Fix path:** two intrinsic families. `System.cmd(program, args)`
returns captured output plus exit status and must park the calling
process rather than block a scheduler thread. `File.ls(path)` returns
directory entries. Per-entry metadata can ride the `Fd`
random-access pass tracked above, which already owns `stat`.

---

## Projects that declare externs cannot use plain `koja run`

Found 2026-08-28. `koja run` and task execution default to the
interpreter, and the interpreter rejects any extern that is not in
the eval dispatch table, so an FFI project fails at startup. `koja
test` compiles natively, so the same code tests fine, which makes
the run failure surprising. The `koja shell` entry above tracks the
same limitation at the REPL prompt. The error message names the
workaround (`--backend=llvm`), which softens the edge but does not
remove the paper cut: the flag is needed on every `koja run` and
every task invocation in an FFI project.

**Fix path:** select the LLVM backend automatically when the
project (or a loaded dependency) declares an extern outside the eval
dispatch table, or accept a `backend` key in `koja.toml`. Either
way, plain `koja run` should work in every project.

---

## `String.split` is quadratic on large inputs

Found 2026-08-28 when `git_hygiene` appeared to hang. It was
splitting multi-megabyte `git ls-files` output (a full
`node_modules` tree) on newlines. A stack sample put the time in
`String.slice` under `String.split`, consistent with re-slicing the
remaining tail once per delimiter, which is quadratic in input
size. A few megabytes of input turns into minutes of copying.
Value semantics make the trap easy to hit, because the natural
"split then filter" pipeline looks cheap. Workaround: keep large
command output in shell tools (`wc -l` for counts, `grep` for
filtering) so only small strings cross into Koja.

**Fix path:** index-scan the source once and copy each piece
directly, which is linear. Document the copy cost model on `String`
either way.

---

## Toolchain and stdlib nits from the `git_hygiene` build

Found 2026-08-28. None blocking, each with a workaround:

- **`List` has no `sort`.** Ordered output needs a hand-rolled
  insertion sort or a shell-side `sort`. A comparator-closure
  `sort` works today. A `Comparable` conformance can follow when
  the protocol exists (see the `Binary` ordering entry).
- **`IO.gets` cannot distinguish end of input from an empty
  line.** Both return `""`, so a line-oriented filter reading
  stdin cannot terminate correctly. Workaround is reading
  `STDIN` directly and treating the error case as end of input.
  An `Option`-returning variant or an `IO.lines` iterator closes
  it.
- **`koja doc search` matches symbol names only.** Concept
  queries like `Command` or `Shell` return no matches, and the
  absence of a hit cannot distinguish "no such API" from "wrong
  search term". Indexing doc bodies would let the docs answer
  capability questions, like whether subprocess support exists
  at all.
- **`koja new` couples the directory name to the project name.**
  The project name must be snake_case because the code namespace
  derives from it, but repository hosts and checkout conventions
  prefer dashes (`git-hygiene` on GitLab holds package
  `git_hygiene`). Today that means creating the project and then
  renaming the directory by hand. Two fix shapes: an optional
  directory argument (`koja new git_hygiene git-hygiene`, the
  Cargo and Gleam `--name` precedent), or the Cargo default
  inverted for Koja: accept a kebab input as the directory name
  and derive the package (`koja new git-hygiene` creates
  `git-hygiene/` holding package `git_hygiene`, namespace
  `GitHygiene`). Both spellings collapse to one package name, so
  the derivation is unambiguous.
