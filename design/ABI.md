# Runtime ABI Contracts

The catalog of binary contracts between compiler backends and
`koja-runtime`. Each contract has one **authoritative definition**
(always on the runtime side, where the bytes are produced or
consumed) and one or more **conforming mirrors** in backend crates
that restate the same values.

## Policy: mirror by spec, never by shared crate

Backends conform to these contracts the way a client conforms to an
API spec — by reading the spec and matching it, not by importing the
server. There is deliberately **no shared constants crate**:

- The IR → backend boundary is a sealed, serializable handoff, and
  the runtime is a leaf `staticlib`. A Rust-level dependency from a
  backend onto the runtime couples two sides that the architecture
  keeps separable.
- Self-hosting removes the option anyway: a backend written in Koja
  cannot import a Rust crate. The contracts below must survive as a
  language-neutral spec, so the spec — this document plus the
  authoritative definition sites — is the artifact worth maintaining.
  A shared crate would be compile-checked comfort with an expiration
  date.

Mirror constants carry an `ABI contract: MUST equal ...` doc comment
pointing at the authoritative definition and the relevant section
here. Drift is caught by the integration suites (`tests/lang`, the
stdlib package tests), which exercise every contract end-to-end on
both backends.

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

SSA values and runtime pointers always address the **first payload
byte**; the headers are reached by negative offsets.

| Constant            | Value      | Meaning                                                                 |
| ------------------- | ---------- | ----------------------------------------------------------------------- |
| `BLOCK_HEADER_SIZE` | 16         | payload → block base distance                                           |
| `LENGTH_OFFSET`     | 8          | payload → `bit_length` word distance                                    |
| `RC_IMMORTAL`       | `i64::MIN` | rodata sentinel; any `rc < 0` is immortal (inc/dec no-ops, never freed) |

- Authoritative: `koja-runtime/src/util.rs` (`BLOCK_HEADER_SIZE`, `LENGTH_OFFSET`; immortality is the `rc < 0`
  test in `koja_rc_inc` / `koja_rc_dec`).
- Mirrors: `koja-ir-llvm/src/emit/heap_layout.rs` (`HEADER_BYTES`, `LENGTH_OFFSET`, `RC_IMMORTAL`),
  `koja-ir-eval/src/intrinsics/cptr.rs` and `binary.rs`
  (`BLOCK_HEADER_SIZE`, `LENGTH_OFFSET`).
- Human spec for which IR types are heap-backed: `koja-ir/src/types.rs` doc comments.

## Closure environment blocks

A closure env block carries a 24-byte header instead of the leaf header — drop and copy glue replace the length word:

```text
[ i64 rc ][ ptr drop_fn ][ ptr copy_fn ][ capture 0 ][ capture 1 ] ...
          ^ +8            ^ +COPY_FN_OFFSET (16)
```

- Authoritative: `koja-runtime/src/util.rs` (`COPY_FN_OFFSET`; the drop_fn offset reuses `LENGTH_OFFSET`).
- Mirror: `koja-ir-llvm/src/types.rs` (`CLOSURE_ENV_HEADER_FIELDS = 3` — the same three words expressed
  as LLVM struct fields).
- Eval does not mirror this: it represents closures as Rust values, not raw blocks.

## Message envelope wire format

A mailbox message is a tag header followed by the payload:

```text
offset 0                  offset TAG_HEADER_SIZE (8)
[ tag: u8 | padding ... ][ payload ... ]
```

| Constant                  | Value |
| ------------------------- | ----- |
| `TAG_BUSINESS`            | 0     |
| `TAG_LIFECYCLE`           | 1     |
| `TAG_IO_READY`            | 2     |
| `TAG_REPLY`               | 3     |
| `TAG_HEADER_SIZE`         | 8     |
| `LIFECYCLE_BUF_SIZE`      | 16    |
| `IO_READY_BUF_SIZE`       | 24    |
| `IO_READY_VARIANT_OFFSET` | 8     |
| `IO_READY_FD_OFFSET`      | 16    |

- Authoritative: `koja-runtime/src/wire.rs` (the module doc there is
  the long-form spec, including which tags can surface in `receive`
  arms).
- Mirror: `koja-ir/src/function.rs` (`ReceiveTag::wire_byte` —
  `Business == 0`, `Lifecycle == 1`); the LLVM backend consumes the
  payload at offset 0 because the runtime strips the tag header
  before delivery.

## Numeric parse helper return codes

`koja_int_parse` / `koja_float_parse` take a string payload pointer
plus an out-pointer and return a classification code:

| Constant               | Value | Meaning                                                                                     |
| ---------------------- | ----- | ------------------------------------------------------------------------------------------- |
| `PARSE_INVALID_FORMAT` | 0     | malformed text (includes `inf` / `nan` tokens for floats)                                   |
| `PARSE_OK`             | 1     | parsed; value written through the out-pointer                                               |
| `PARSE_OUT_OF_RANGE`   | 2     | well-formed number that does not fit (`Int` overflow, float magnitude rounding to infinity) |

- Authoritative: `koja-runtime/src/parse_text.rs` (codes and the
  classification rules; the C-ABI wrappers live in `string.rs`).
- Mirror: `koja-ir-llvm/src/intrinsics/parse.rs` (`PARSE_OK`,
  `PARSE_OUT_OF_RANGE`; invalid-format is the switch default).
- Eval consumes `parse_text` directly (a real Cargo dependency —
  eval executes runtime logic in-process, so for it the runtime is
  the implementation, not a spec to conform to).

## Kernel enum tag conventions

Backends construct `Result` and `Option` values inside intrinsics
and rely on the declaration order in `lib/global/src/kernel.koja`:

| Enum     | Variant | Tag |
| -------- | ------- | --- |
| `Result` | `Ok`    | 0   |
| `Result` | `Err`   | 1   |
| `Option` | `Some`  | 0   |
| `Option` | `None`  | 1   |

These two enums are the **only** ones whose tags may be hardcoded —
they are pervasive, pinned by `koja-ir`'s elaborate/seal passes, and
reordering them would be a flag-day change. Every other stdlib enum
a backend needs to construct (e.g. `NumericConversionError`) must be
resolved **by variant name** at emit/eval time:
`TypeLayouts::enum_variant_tag` in `koja-ir-llvm`,
`helpers::conversion_error_value`'s decl lookup in `koja-ir-eval`.
Declaration order in stdlib sources is alpha-sorted and free to
change; name lookup makes that a non-event.

## Runtime extern function signatures

The `koja_*` / `koja_rt_*` C-ABI function surface (allocation, rc,
process lifecycle, sockets, parse helpers, ...) is a contract of the
same kind: authoritative at the `#[unsafe(no_mangle)]` definition
sites in `koja-runtime`, mirrored by the declare-on-first-use
helpers in `koja-ir-llvm/src/runtime.rs`. Signatures are matched by
spec; there is no generated header. When adding or changing one,
update both sides and note parameter meaning at the definition site.
