# Runtime ABI Contracts

The standing record of load-bearing binary contracts between compiler
backends and the Koja runtime crates. It catalogs values that cross crate or
language boundaries. The final section defines the policy for the larger
runtime extern surface rather than listing every symbol.

## Policy: mirror by spec, never by shared crate

Backends conform to these contracts by reading the specification and matching
it, rather than importing the runtime. There is deliberately **no shared
constants crate**:

- The IR to backend boundary is sealed and intended to remain serializable.
  `IRProgram` serialization has not landed. The native runtime boundary is a
  leaf `staticlib`. A Rust-level dependency from a backend onto the runtime
  couples two sides that the architecture keeps separable.
- The contract must remain language-neutral and independent of the compiler
  implementation language. This document plus the authoritative definition
  sites is the artifact every backend can consume. A shared Rust crate would
  make one implementation convenient by coupling layers that are otherwise
  deliberately separate.

Mirror constants carry an `API contract: MUST equal ...` doc comment
pointing at the authoritative definition and the relevant section
here where practical.

Language fixtures exercise end-to-end behavior on both backends when eligible.
Stdlib package tests and user C FFI tests run through LLVM. Most numeric ABI
constants are not directly compared across crates, so drift is detected
indirectly through process, I/O, memory, and parsing behavior.

**When changing any value below:** update the authoritative site,
every listed mirror, and this document in the same change, then run
`just doit`.

## Heap leaf blocks (`String` / `Binary` / `Bits`)

Every rc-managed leaf heap value is one allocation:

```text
block base                              payload pointer
[ i64 rc ][ i64 bit_length ][ payload bytes ... ][ NUL (String only) ]
          ^ +LENGTH_OFFSET   ^ +BLOCK_HEADER_SIZE
```

SSA values and runtime pointers always address the **first payload byte**. The
headers are reached by negative offsets.

| Constant            | Value      | Meaning                                                                 |
| ------------------- | ---------- | ----------------------------------------------------------------------- |
| `BLOCK_HEADER_SIZE` | 16         | payload → block base distance                                           |
| `LENGTH_OFFSET`     | 8          | payload → `bit_length` word distance                                    |
| `RC_IMMORTAL`       | `i64::MIN` | rodata sentinel, any `rc < 0` is immortal (inc/dec no-ops, never freed) |

- Authoritative: `koja-runtime-posix/src/util.rs` (`BLOCK_HEADER_SIZE`, `LENGTH_OFFSET`, immortality is the `rc < 0`
  test in `koja_rc_inc` / `koja_rc_dec`).
- Mirrors: `koja-ir-llvm/src/emit/heap_layout.rs` (`HEADER_BYTES`, `LENGTH_OFFSET`, `RC_IMMORTAL`),
  `koja-ir-eval/src/abi.rs` (`BLOCK_HEADER_SIZE`, `LENGTH_OFFSET`).
- Human spec for which IR types are heap-backed: `koja-ir/src/types.rs` doc comments.

Runtime functions may transfer a fresh `rc = 1` Binary block through the
package-private `RuntimeBlock.adopt_binary` intrinsic. Adoption consumes
the payload pointer: LLVM returns it as the owned Binary without changing
the refcount, while eval copies the bytes into its value representation
and frees the runtime block. The pointer must not be used or freed after
adoption.

## Socket result buffers

The socket runtime returns raw buffers for DNS results and datagram receives.
It returns null on error and stores the error for `koja_last_error`.

`koja_socket_resolve` returns this buffer:

```text
offset 0        offset 8
[ i64 count ][ Binary payload pointer 0 ][ Binary payload pointer 1 ] ...
```

`koja_socket_recv_from` returns this buffer:

```text
offset 0               offset 8             offset 16
[ data Binary pointer ][ IP Binary pointer ][ i64 port ]
```

The runtime allocates each Binary block with `rc = 1`. It also allocates the
outer result buffer. The backend frees the outer buffer after it reads the
fields.

The backend transfers each Binary pointer into an owned raw Koja value. LLVM
keeps the pointer and refcount. Eval copies the bytes and frees the runtime
block.

The backend must return only `List<Binary>` or `(Binary, Binary, Int)` from
these intrinsics. Standard library Koja code constructs `IPAddress` and
`Socket.Address`.

Backends must not treat the raw buffers as user struct storage. A user struct
layout can change without a runtime ABI change.

- Authoritative: `koja-runtime-posix/src/socket.rs`
  (`koja_socket_resolve` and `koja_socket_recv_from`).
- Mirrors: `koja-ir-llvm/src/intrinsics/socket.rs` and
  `koja-ir-eval/src/intrinsics/socket.rs`.

## Closure environment blocks

A closure env block carries a 24-byte header instead of the leaf header. Drop
and copy glue replace the length word:

```text
[ i64 rc ][ ptr drop_fn ][ ptr copy_fn ][ capture 0 ][ capture 1 ] ...
          ^ +8            ^ +COPY_FN_OFFSET (16)
```

- Authoritative: `koja-runtime-posix/src/util.rs` (`COPY_FN_OFFSET`, while the drop_fn offset reuses `LENGTH_OFFSET`).
- Mirror: `koja-ir-llvm/src/types.rs` (`CLOSURE_ENV_HEADER_FIELDS = 3`, the same three words expressed
  as LLVM struct fields).
- Eval does not mirror this: it represents closures as Rust values, not raw blocks.

## Message envelope wire format

A mailbox message is a tag header followed by the payload:

```text
offset 0                  offset TAG_HEADER_SIZE (8)
[ tag: u8 | padding ... ][ payload ... ]
```

| Constant                       | Value |
| ------------------------------ | ----- |
| `TAG_BUSINESS`                 | 0     |
| `TAG_LIFECYCLE`                | 1     |
| `TAG_IO_READY`                 | 2     |
| `TAG_REPLY`                    | 3     |
| `TAG_EXIT_SIGNAL`              | 4     |
| `TAG_HEADER_SIZE`              | 8     |
| `LIFECYCLE_BUF_SIZE`           | 16    |
| `IO_READY_BUF_SIZE`            | 24    |
| `IO_READY_VARIANT_OFFSET`      | 8     |
| `IO_READY_FD_OFFSET`           | 16    |
| `EXIT_SIGNAL_BUF_SIZE`         | 40    |
| `EXIT_SIGNAL_PID_OFFSET`       | 8     |
| `EXIT_SIGNAL_REASON_OFFSET`    | 16    |
| `EXIT_SIGNAL_MESSAGE_OFFSET`   | 24    |
| `EXIT_SIGNAL_BACKTRACE_OFFSET` | 32    |

- Authoritative: `koja-runtime-core/src/wire.rs` (the module doc there is
  the long-form spec, including which tags can surface in `receive`
  arms).
- Mirror: `koja-ir/src/function.rs` (`ReceiveTag::wire_byte` for
  business, lifecycle, I/O, and exit-signal tags). The LLVM backend
  consumes the payload at offset 0 because the runtime strips the tag
  header before delivery. `IOReady` arms are synthesized by the
  `elaborate` I/O sub-pass for processes whose message union contains
  `IOReady`. Exit-signal arms are synthesized the same way for
  `Process.ExitSignal`.

An exit-signal payload is the dying `Pid` followed by
`Process.ExitReason`. The reason stores its tag at offset 16. A crash stores
the managed `CrashInfo.message` and `CrashInfo.backtrace` pointers at offsets
24 and 32. Non-crash variants leave those pointers null.

Lifecycle and I/O payloads carry declaration-order variant bytes.

| `Process.Lifecycle` | Byte |
| ------------------- | ---- |
| `Shutdown`          | 0    |
| `Interrupt`         | 1    |
| `Reload`            | 2    |

| `IO.Ready` | Byte |
| ---------- | ---- |
| `Read`     | 0    |
| `Write`    | 1    |
| `Error`    | 2    |

These values are authoritative in `koja-runtime-core/src/protocol.rs` and
`wire.rs`, where unit tests pin each code to its variant name. The
declarations in `lib/global/src/process.koja` and `lib/global/src/io.koja`
must retain the same order because LLVM reads and writes the variant byte
directly. The compiler enforces that order: the wire-contract check in
`koja-ir-llvm` (`layout/wire_contract.rs`) runs after enum registration and
fails the build when a declaration diverges from this catalog. Eval does not
depend on the order. Its scheduler and reactor adapters resolve variants by
name through the core tables.

### Process context

Every envelope carries a `Process.Context` beside the payload, not inside
the byte buffer. The runtime stamps the sender's context onto business and
timer envelopes and installs it on the receiver when a business message is
dequeued. Reply, lifecycle, I/O, and exit-signal envelopes carry the zero
context and install nothing. The same 32 bytes sit in each process table
slot, and `spawn` copies the parent's slot into the child.

| Offset | Field      | Width | Meaning                      |
| ------ | ---------- | ----- | ---------------------------- |
| 0      | `flags`    | 8     | bit 0 is the sampled bit     |
| 8      | `span`     | 8     | span id, zero outside a span |
| 16     | `trace_hi` | 8     | trace id bytes 0 to 7        |
| 24     | `trace_lo` | 8     | trace id bytes 8 to 15       |

`CONTEXT_SIZE` is 32. `koja_rt_context_get(i8* out)` copies the calling
process's slot into `out`, and `koja_rt_trace_install(i8* context)` copies
32 bytes from `context` into the slot. LLVM passes the address of a
`Process.Context` alloca for both, so the Koja struct must stay four 64-bit
words in this order. The wire-contract check pins the field types, and the
Rust side is `koja_runtime_core::context::Context` with a unit test on its
size. Eval builds the struct by field position from the same four words.

### Trace spans and export

The stdlib `Trace` module keeps each open span's record on the calling
process, and finished records in one bounded export queue. The record type
is a stdlib struct the runtime never reads. It crosses the boundary the way
a message payload does, as bytes plus a length plus drop glue, and comes
back as bytes into a caller-owned slot of the same size.

| Function                 | Arguments                        | Returns            |
| ------------------------ | -------------------------------- | ------------------ |
| `koja_rt_span_id`        | none                             | fresh non-zero id  |
| `koja_rt_span_open`      | `record, len, drop_glue`         | handle             |
| `koja_rt_span_take`      | `handle, out, out_cap`           | none, fills `out`  |
| `koja_rt_span_put`       | `handle, record, len, drop_glue` | none               |
| `koja_rt_span_close`     | `handle, out, out_cap`           | none, fills `out`  |
| `koja_rt_export_push`    | `record, len, drop_glue`         | none               |
| `koja_rt_export_pop`     | `out, out_cap`                   | 0 filled, -1 empty |
| `koja_rt_export_dropped` | none                             | drop count         |

A handle is the record's index on the process's open span stack. `take`
leaves the slot empty and `put` refills it, so a span handle can edit its
record in place. `close` pops the innermost record, which must be `handle`.
The runtime aborts on a stale or out-of-order handle, because that is a
stdlib bug, not user input. Records left open when a process exits run
their drop glue. The export queue holds `EXPORT_CAPACITY` (4096) records.
`push` past capacity drops the new record, runs its glue, and bumps the
counter `koja_rt_export_dropped` reads.

### Log slot

The stdlib `Log` module reads its configuration from the calling process.
Each process table slot holds a log floor word and a busy word, and the
execution state holds an optional configuration payload. The payload is a
stdlib struct the runtime never reads. It crosses the boundary as bytes
plus a length plus two by-pointer shims, the drop glue and a deep-copy
glue. The runtime clones the payload by memcpy and then calls the copy
glue over the new bytes, which replaces every heap pointer the memcpy
duplicated with a fresh block.

| Function                | Arguments                                  | Returns            |
| ----------------------- | ------------------------------------------ | ------------------ |
| `koja_rt_log_level`     | none                                       | floor word         |
| `koja_rt_log_configure` | `config, len, drop_glue, copy_glue, level` | none               |
| `koja_rt_log_config`    | `out, out_cap`                             | 0 filled, -1 unset |
| `koja_rt_log_enter`     | none                                       | 1 entered, 0 busy  |
| `koja_rt_log_leave`     | none                                       | none               |

`spawn` copies the floor word into the child and clones the payload
through the copy glue. The busy word starts clear in the child. The floor
word is the rank the stdlib `Log.Level` passes with each configure, and a
fresh slot holds `DEFAULT_LOG_LEVEL` (1, the rank of `Info`). `config`
fills `out` with an independent copy the caller owns, so `out_cap` must
cover the whole value, and the runtime aborts when it does not.

## Numeric parse helper return codes

`koja_int_parse` / `koja_float_parse` take a string payload pointer
plus an out-pointer and return a classification code:

| Constant               | Value | Meaning                                                                                     |
| ---------------------- | ----- | ------------------------------------------------------------------------------------------- |
| `PARSE_INVALID_FORMAT` | 0     | malformed text (includes `inf` / `nan` tokens for floats)                                   |
| `PARSE_OK`             | 1     | parsed, value written through the out-pointer                                               |
| `PARSE_OUT_OF_RANGE`   | 2     | well-formed number that does not fit (`Int` overflow, float magnitude rounding to infinity) |

- Authoritative: `koja-runtime-posix/src/parse_text.rs` (codes and the
  classification rules, with C-ABI wrappers in `string.rs`).
- Mirror: `koja-ir-llvm/src/intrinsics/parse.rs` (`PARSE_OK`,
  `PARSE_OUT_OF_RANGE`, with invalid format as the switch default).
- Eval consumes `parse_text` directly as a Cargo dependency. It executes
  runtime logic in-process, so the runtime is the implementation rather than a
  specification to conform to.

## Kernel enum tag conventions

Enum tags are dense declaration-order indices. Reordering a variant changes
its tag.

Every stdlib enum that a backend constructs, including `Result`, `Option`,
and `Process.CallError`, resolves its tags by variant name at emit or eval
time, through `TypeLayouts::enum_variant_tag` in `koja-ir-llvm` or
declaration lookup in `koja-ir-eval`. No backend pins these tags to
declaration order.

One send-path exception remains. The native `Ref.call` / `Ref.cast` envelope
packers stamp the `Option<ReplyTo<R>>` tag word without an enum symbol in
hand, so `Option` must declare `Some` before `None`:

| Enum     | Variant | Tag |
| -------- | ------- | --- |
| `Option` | `Some`  | 0   |
| `Option` | `None`  | 1   |

The `koja-ir-llvm` wire-contract check verifies this order on every compile,
together with `Lifecycle`, `IO.Ready`, and `ExitReason`, so a reorder fails
the build instead of corrupting envelopes.

Lifecycle, I/O readiness, and exit-reason tags are wire contracts cataloged
in this document.

Alpha sorting is a source convention where no semantic order exists. It is not
an ABI rule. Wire-contract enums and semantically ordered enums may use another
order.

The `Priority` enum (`lib/global/src/process.koja`) is resolved by
variant name: the compiler assigns the Koja→wire scheduling weight in
`emit_apply_priority` (`koja-ir`), so its variants stay alpha-sorted.

| Variant  | Wire weight |
| -------- | ----------- |
| `Low`    | 0           |
| `Normal` | 1           |
| `High`   | 2           |

`koja_rt_set_priority(i64 level)` applies it to the _current_ process. `level`
is the wire weight and out-of-range values clamp to
`Normal` (`koja_runtime_core::Priority::from_index`).

`koja_rt_yield_check()` (`void()`) is a cooperative-preemption point. For
regular functions, the compiler inserts `YieldCheck`:

- at loop back-edges
- before each tail call
- after the parameter-promotion prologue at the entry of every
  call-containing function

The top-level script body receives back-edge checks. Functions declared in a
script receive the regular function checks. Leaf functions avoid an entry
check.

Each check spends one reduction from the running process's per-quantum budget
and, when the budget hits zero, re-queues the process so a peer can run. The
budget is granted by priority (`Priority::budget`) and reset when the process
is next scheduled. The interpreter has no extern: `koja-ir-eval` routes
`YieldCheck` through `scheduler::reduce` for the same effect.

| Priority | Reductions |
| -------- | ---------- |
| `Low`    | 1,000      |
| `Normal` | 2,000      |
| `High`   | 4,000      |

This is cooperative preemption. A long foreign call or another region that
does not reach a yield check can occupy its worker beyond one nominal quantum.

Process exit reasons have one shared wire mapping.

| Reason     | Code |
| ---------- | ---- |
| `Normal`   | 0    |
| `Shutdown` | 1    |
| `Killed`   | 2    |
| `Crashed`  | 3    |

`koja_rt_process_exit(i64 reason)` (`void(i64)`) records why the current
process terminated on its control block (read by `ProcessTable`'s
exit-notification seam). The compiler emits a call to `StopReason.code()` in
the process-body tail and passes the resulting `Normal` or `Shutdown` code to
`ProcessExit`. Out-of-range values clamp to `Normal` through
`koja_runtime_core::ExitReason::from_index`. A forced kill and a user crash
record their codes directly through their runtime paths. The interpreter has
no extern:
`koja-ir-eval` routes `ProcessExit` through `scheduler::process_exit`.

## Process runtime status values

Several process externs return compact status values that LLVM interprets.

| Function                   | Value           | Meaning                                     |
| -------------------------- | --------------- | ------------------------------------------- |
| `koja_rt_receive`          | message tag     | delivered lifecycle, business, I/O, or exit |
| `koja_rt_receive`          | -1              | empty wake, an invariant fallback           |
| `koja_rt_receive_timeout`  | message tag     | delivered message                           |
| `koja_rt_receive_timeout`  | -1              | timeout                                     |
| `koja_rt_call_receive`     | 0               | matching reply delivered                    |
| `koja_rt_call_receive`     | -1              | timeout, or the callee died with no reply   |
| `koja_rt_reply`            | 0               | caller still waiting                        |
| `koja_rt_reply`            | 1               | caller expired                              |
| `koja_rt_is_process_alive` | 0 or 1          | dead or alive                               |
| `koja_rt_parent`           | 0 or parent PID | entry process or parent                     |
| `koja_rt_context_get`      | none            | writes 32 bytes, see Process context        |
| `koja_rt_trace_install`    | none            | reads 32 bytes, see Process context         |
| `koja_rt_export_pop`       | 0 or -1         | record written, or queue empty              |
| `koja_rt_spawn`            | 0 or child PID  | refused spawn or created child              |
| `koja_rt_mailbox_depth`    | -1 or depth     | dead pid, or queued message count           |
| `koja_rt_process_state`    | -1 or state     | dead pid, or `Process.State` wire index     |

The observability externs (`koja_rt_process_count`,
`koja_rt_process_count_by_state`, `koja_rt_scheduler_count`,
`koja_rt_self_mailbox_depth`, `koja_rt_mailbox_depth`,
`koja_rt_process_state`) are declared in `lib/global/src/runtime.koja`
and read the runtime without side effects. The state wire index is a named
mapping (0=Blocked, 1=Created, 2=Runnable, 3=Running, 4=WaitingIO), not the
`Process.State` declaration order. The Rust side is
`koja_runtime_core::ProcessState::wire_index` and its inverse, pinned by a
unit test. The Koja side is the `state_wire_index` /
`state_from_wire_index` pair in `lib/global/src/runtime.koja`, pinned by a
package test.

`Ref.call` distinguishes timeout from `ProcessDown` after a `-1` result by
querying target liveness. LLVM resolves the corresponding
`Process.CallError` tag by variant name.
`koja_rt_call_receive(i64 token, i8* out, i64 out_cap, i64 timeout_ms,
i64 target_pid)` takes the callee's PID so the runtime returns `-1` as
soon as the callee dies with no reply slotted, instead of waiting out
the timeout.

## Crash unwind ABI

Native user panic containment depends on these contracts.

- Compiled `Kernel.panic` calls `__koja_panic(payload_ptr)`. Its Rust
  definition and the scheduler's `ProcessFn` entry type use
  `extern "C-unwind"`.
- LLVM declares the symbol as an ordinary extern. Unwind behavior relies on
  the linked Rust definition and the unwind metadata on compiled Koja frames,
  not a mirrored LLVM calling-convention attribute.
- Compiler-defined Koja bodies, glue, intrinsics, closures, and entry wrappers
  receive `frame-pointer=all` and async `uwtable`. Foreign declarations,
  runtime extern declarations, libc helpers, and raw envelope drop shims do
  not.
- The native process trampoline catches the unwind and records
  `ExitReason::Crashed` with `CrashInfo`.

`koja_panic_backtrace(*const c_char)` is a separate C-string entry point. It is
not the primary panic call emitted by the compiler.

Unwind tables permit traversal and containment. They do not run Koja drop glue.
LLVM does not currently emit cleanup landing pads for managed frame locals.
See [MEMORY-MODEL.md](MEMORY-MODEL.md#failure-and-forced-termination).

The diagnostic renders once before `resume_unwind` carries the structured
crash to the trampoline. A crashing native entry process forces a nonzero OS
exit.

Eval uses no native unwind ABI for an ordinary Koja panic. `Kernel.panic`
produces `RuntimeError::Panicked`. Spawned process futures catch that error and
record a crashed death. Entry and script bodies propagate it to the driver for
a nonzero exit. `EvalExecutor::resume` catches unexpected Rust host panics as a
backstop. Eval currently records an empty `CrashInfo.backtrace`.

- Native authority: `koja-runtime-posix/src/panic.rs` and
  `scheduler.rs::process_trampoline`.
- LLVM mirror: `koja-ir-llvm/src/ctx.rs::set_frame_pointer` and the
  `__koja_panic` declaration in `runtime.rs`.
- Eval behavior: `koja-ir-eval/src/intrinsics/kernel.rs`,
  `interpreter.rs::build_spawn_future`, and `scheduler.rs`.

## Runtime extern function signatures

The `koja_*` / `koja_rt_*` C-ABI function surface (allocation, rc,
process lifecycle, sockets, parse helpers, ...) is a contract of the
same kind: authoritative at the `#[unsafe(no_mangle)]` definition
sites in `koja-runtime-core` and `koja-runtime-posix`, mirrored by the
declare-on-first-use
helpers in `koja-ir-llvm/src/runtime.rs`. Signatures are matched by
specification. There is no generated header. When adding or changing one,
update both sides and note parameter meaning at the definition site.
