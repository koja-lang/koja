# Memory Model — RC rollout (implementation history)

Extracted from `design/MEMORY-MODEL.md` once the drop-glue / RC rollout
(Phases 0-5) landed. This is the historical record of how the
value-semantics reference-counting model was built; the live model
itself lives in `MEMORY-MODEL.md`.

## Implementation tracker (drop-glue / RC rollout)

Compiler-internal `clone_T` / `drop_T` glue (via the `elaborate`
sub-pass), retiring the user-facing `Clone` protocol. Strategy:
reference-counting for heap leaves; synthesized glue for composites
(`List`, `Map`, `Set`, struct, enum, union). User-facing value
semantics: every binding owns an independent value; acquire at
boundaries (`Clone` / `clone_T`), release at scope exit (`Drop` /
`drop_T`). Last-use move elision is a future optimizer — the naive
baseline always acquires at boundaries for now.

### Completed

- **Phase 0 — scaffold.** `is_heap_managed`, `FunctionKind::CloneGlue` /
  `DropGlue`, `elaborate` wiring, glue symbol mangling, seal + backend
  match updates.
- **Phase 1 — struct / enum / union glue.** `elaborate` synthesizes
  aggregate clone/drop IR bodies; LLVM emits them; seal validation +
  unit tests.
- **Phase 2a — IR wiring.** `elaborate` rewrite pass (composite
  `Clone` / `Drop` → glue `Call`); eval short-circuits
  `CloneGlue` / `DropGlue`; LLVM no-glue aggregate arms (rebind /
  no-op).
- **Phase 2b — collection glue bodies.** LLVM `clone_T` / `drop_T` for
  `List`, `Map`, `Set`.
- **Phase 2c — COW correctness.** Shared element acquire/release
  helpers; list insert/append COW; hashtable clone/insert/resize;
  `emit_map_get` must call `acquire_value` on hand-out (fixed).

### Phase 2d — flip lowering to `is_heap_managed`

Flip lowering from `is_heap_leaf` to `is_heap_managed`; end-to-end
value-semantics tests.

**Done in this slice:**

- `materialize_owned`, `emit_slot_drops`, `drop_discarded_temp` use
  `is_heap_managed`.
- `heap_leaf_slots` → `heap_managed_slots`.
- TCO promotion prefix scan is structural (not heap-leaf-specific).
- `IRType::Indirect` is transparent in `elaborate` (no separate glue;
  inner type's glue applies).
- **Loop body scoping fix** (`lower/loops.rs`): bindings declared
  inside a loop body are dropped at the back-edge and excluded from
  function-exit drops (fixes zero-trip loop + uninitialized slot drop,
  e.g. `Headers.set` on empty list).
- `lower_process` tests updated for acquire-before-return on
  heap-managed values.

**Done in a later slice:**

- **`List.pop()` empty-branch clone** (`koja-ir-llvm` `intrinsics/list.rs`):
  the empty branch returned the borrowed `self` buffer directly as the
  pair's `.second`, so `empty_pair.second` aliased the caller's
  receiver slot and both freed the same buffer at scope exit. Now
  clones into a fresh (empty) buffer, mirroring the nonempty branch's
  `copy_buffer`. Fixes the `list_test` "pop until empty" teardown
  crash (was the global-stdlib exit-1).
- **Tail-call composite-arg acquire** (`tail_calls.rs`): the
  self-tail-call rewrite acquired only _heap-leaf_ args before the
  trailing exit drops; composite args (`List`, struct, etc.) were
  rebound from the just-dropped slot → use-after-free on the next
  iteration. Now acquires every `is_heap_managed` arg (the inserted
  `Clone` is elaborated into `clone_T` for composites). Fixes the
  `http` `headers_test` "get_all" crash (recursive `collect_all`).
- **Shared heap predicate**: hoisted `is_heap_managed` to an
  `IRType` method; `lower::ownership` and `tail_calls` now share it.
- All of `just doit` green (lint + stdlib + `test-rust` + `test-lang`).

**Remaining for Phase 2d:**

- **Call-boundary acquire** (`lower/calls.rs`) is _not_ needed for
  correctness under the current "callee acquires on promotion" +
  "intrinsics return freshly-owned heap" convention — the two crashes
  above were the intrinsic empty-branch and the tail-call gap, not a
  missing caller-side clone. Revisit only if a future aliasing case
  surfaces; blanket caller-side cloning would leak against intrinsics
  that borrow `self`.
- Optional: regression test for zero-trip loop with body-scoped
  heap binding. (`pop()`-on-empty and tail-recursive composite args
  are now covered by stdlib `list_test` / http `headers_test` plus
  `tail_calls` unit tests.)

### Done — Phase 3

Closures are now first-class heap-managed values: `is_heap_managed`
returns `true` for `IRType::Function`, so `materialize_owned` /
`emit_slot_drops` / `drop_discarded_temp` treat a closure exactly like
a `String` or a `List`. Captures are acquired into the env at
`MakeClosure` and released transitively when the env's refcount hits
zero — no more leaked env blocks and no use-after-free when a captured
heap value's outer binding is dropped first.

**Env ABI.** A (non-null) env block gains a 16-byte header mirroring
the heap-leaf shape: `[i64 rc][ptr drop_fn]`, captures following.
`drop_fn` is the address of the closure body's capture-release glue
(or null when no capture is heap-managed). Both the backend
(`CLOSURE_ENV_HEADER_FIELDS`) and the runtime (the `LENGTH_OFFSET`
header note) agree on the layout; capture `i` lives at field
`2 + i`. The env base pointer doubles as the rc word, so the existing
`koja_rc_inc` operates on it unchanged.

**Clone / drop.** `Clone` of a `Function` is an `rc++` on the env
(aliasing the same `{fn_ptr, env_ptr}` fat pointer — the env is shared
like an immutable leaf). `Drop` calls the new `koja_closure_rc_dec`,
which null/immortal-checks, decrements, and at zero runs `drop_fn`
(when present) before `free`ing the block. Both the slot-keyed
(`DropLocal`) and value-keyed (`DropValue`) closure drop paths funnel
through `emit_drop_closure_value`.

**Capture-release glue.** A closure body that owns ≥1 heap-managed
capture gets a sibling `FunctionKind::DropClosureGlue`
(`<body>.$drop_env$`) minted during lowering: closure-shaped (implicit
`env_ptr`, env-first ABI), it `LoadCapture`s each heap-managed capture
and `DropValue`s it, returning `Unit`. Born as real IR so `elaborate`
discovers any composite capture's `drop_T` and rewrites the composite
`DropValue`s into glue calls, exactly as for a `Regular` body. Seal
admits it alongside `Closure` (it's the second `LoadCapture`-bearing
kind); eval never invokes it (host GC reclaims closures).

**Retain cycles: not reachable.** RC here only _shares_ immutable
values; a closure cannot capture a still-mutable binding to itself
(captures are by value, taken at `MakeClosure`, and there are no
reference types). A closure's env can only contain values that
existed before the env, so the ownership graph stays a DAG — no cycle
can form, and the naive `rc--`-frees-at-zero scheme is sound without a
cycle collector.

### Done — Phase 4

The user-facing `Clone` protocol is gone — value semantics makes
explicit duplication meaningless (every value is already independent,
and rc copy-on-write makes assignment cheap). Removed:

- `lib/global/src/clone.koja` (protocol decl + every primitive /
  heap-leaf impl) and its `Global.clone` autoimport entry.
- The `List` / `Map` / `Set` / `CPtr` `Clone` impls in their
  respective `.koja` files.
- `derive_clone.rs` (the auto-`impl Clone for T` synthesizer) and its
  pre-collect wiring in `program.rs` / `synthesize/mod.rs`.
- `Clone` from `UNIVERSAL_PROTOCOLS`; the universal fallback in
  `bounded.rs` now augments bare type-param bounds with `Debug` /
  `Equality` only.
- All `.clone()` call sites in `lib/http` (`client.koja`,
  `parser.koja`) — plain assignment / argument passing replaces them.
- The protocol-clone test files (`koja-typecheck/tests/clone.rs`,
  `koja-ir/tests/lower_clone.rs`, `koja-ir-llvm/tests/clone.rs`,
  `koja-ir-eval/tests/clone.rs`); the `ownership_clone*` lang tests
  were repurposed as `ownership_assign*` (same independence assertion,
  via plain assignment instead of `.clone()`).

This was safe because the value-semantics rc glue already covers every
duplication path: collection copy-on-write
(`hashtable::clone_table_buffers` → `acquire_in_slot`) and element
acquisition run on the internal `$clone$` glue (`clone_glue_symbol` /
`IRInstruction::Clone`), never the protocol. The protocol-clone
backend chain was reachable _only_ through user-facing `.clone()`.

**Backend dead-code cleanup (done):** the now-unreachable
protocol-clone backend chain was excised end to end alongside the
frontend removal — the `*Method::Clone` intrinsic variants and their
`"clone"` mappings in `koja-ir/src/intrinsic_id.rs`, the per-method
dispatch arms (`string.rs` / `binary.rs` / `map.rs` / `set.rs` in both
backends), `emit_table_clone` + `resolve_clone_fn` /
`clone_receiver_symbol` / `call_clone` (hashtable `lifecycle.rs` /
`util.rs`), and the eval `deep_clone_value` / `deep_clone_payload`
helpers. The deep-copy backbone (renamed `heap_clone.rs` →
`heap_payload.rs`) stays for the conversions that genuinely mint a new
block (`Binary.to_string` / `CPtr.to_string`, which add a libc NUL),
and as the seed for future "copy on process boundary" deep copies. The
same-layout reinterprets (`Binary.to_bits`, `String.to_binary`,
`Bits.to_binary`) were migrated off the deep copy to an `rc_inc` +
pointer reinterpret (`heap_payload::share_heap_payload`) — Koja blocks
are immutable, so sharing is invisible and matches the heap-leaf
`Clone` path.

### Phase 5

Refresh doc comments + this doc's "Supersedes" paragraph (elaborate
glue is landed, not shelved); `just lint` + `just test`; leak audit
(HTTP suite passes without explicit clones).

### Known bugs discovered (session notes)

| Symptom                                                                          | Root cause                                                                                               | Fix status                        |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- | --------------------------------- |
| `qualified_signature.koja` SIGBUS (flaky → deterministic with empty list + loop) | Loop-body local dropped at function exit uninitialized when loop runs 0 times                            | Fixed (loop scoping)              |
| `Map.get` double-free                                                            | Hand-out without `acquire_value`                                                                         | Fixed                             |
| `List.pop()` on empty list, teardown SIGBUS                                      | Empty branch returned the borrowed `self` buffer as `Pair.second`; caller slot + pair drop the same list | Fixed (empty-branch clone)        |
| Global stdlib exits 1 after ~84 green dots                                       | Same as pop-on-empty (crashes before harness summary)                                                    | Fixed                             |
| `http` `headers_test` "get_all" SIGSEGV in `clone_Header`                        | Tail-call rewrite acquired only heap-_leaf_ args; composite args rebound from just-dropped slot          | Fixed (acquire heap-managed args) |
| `lower_process` spawn/receive tests                                              | Expected raw `ValueId`; return now acquires heap-managed values                                          | Fixed (structural assert)         |
