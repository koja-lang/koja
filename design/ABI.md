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
`wire.rs`. The declarations in `lib/global/src/process.koja` and
`lib/global/src/io.koja` must retain the same order. LLVM reads and writes the
variant byte directly. Eval maps the same indices through its scheduler and
reactor adapters.

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

Backends construct `Result` and `Option` values inside intrinsics
and rely on the declaration order in `lib/global/src/result.koja` and
`lib/global/src/option.koja`:

| Enum     | Variant | Tag |
| -------- | ------- | --- |
| `Result` | `Ok`    | 0   |
| `Result` | `Err`   | 1   |
| `Option` | `Some`  | 0   |
| `Option` | `None`  | 1   |

Enum tags are dense declaration-order indices. Reordering a variant changes its
tag.

`Result` and `Option` are pervasive hardcoded conventions pinned by the IR
elaborate and seal passes. LLVM also hardcodes the two
`Process.CallError` tags:

| `Process.CallError` | Tag |
| ------------------- | --- |
| `Timeout`           | 0   |
| `ProcessDown`       | 1   |

Lifecycle, I/O readiness, and exit-reason tags are wire contracts cataloged in
this document. Other stdlib enums that a backend constructs must resolve their
tags by variant name at emit or eval time, through
`TypeLayouts::enum_variant_tag` in `koja-ir-llvm` or declaration lookup in
`koja-ir-eval`.

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
| `koja_rt_call_receive`     | -1              | timeout                                     |
| `koja_rt_reply`            | 0               | caller still waiting                        |
| `koja_rt_reply`            | 1               | caller expired                              |
| `koja_rt_is_process_alive` | 0 or 1          | dead or alive                               |
| `koja_rt_parent`           | 0 or parent PID | entry process or parent                     |
| `koja_rt_spawn`            | 0 or child PID  | refused spawn or created child              |

`Ref.call` distinguishes timeout from `ProcessDown` after a `-1` result by
querying target liveness. LLVM resolves the corresponding
`Process.CallError` tag using the hardcoded convention above.

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

[CONFORMANCE-HEADERS.md](CONFORMANCE-HEADERS.md) proposes a generated
language-neutral surface that would make this larger symbol set mechanically
checkable. Until that lands, source definitions and LLVM declarations must
change together.
