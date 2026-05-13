# V1 → Alpha Parity

What the alpha pipeline (`expo-parser` → `expo-alpha-typecheck` →
`expo-alpha-ir` → `expo-alpha-ir-llvm` / `expo-alpha-ir-eval`) still
needs to absorb the surface area v1 supports, so we can retire the
v1 pipeline. Replaces the stdlib-scoped
[`archive/20260511-ALPHA-ROADMAP.md`](archive/20260511-ALPHA-ROADMAP.md),
which served its purpose: every file under `lib/global/src/` plus
`Crypto` now compiles end-to-end through alpha.

This doc focuses on **what's left** — the language features v1 has
that alpha doesn't, and the qualified stdlib packages still gated on
those features. It is a parity ledger, not a sequencing plan; the
sequencing block at the bottom is a recommendation, not a contract.

---

## Status snapshot

- **Autoimport bundle (`lib/global/src/`)**: parity. 14 files, 2356
  LOC, all compile and run through alpha — bitwise, cptr, cstring,
  debug, fd, io, kernel, list, map, process, set, string, system,
  time. Plus alpha-only synthesis: `alpha_clone`,
  `alpha_debug_containers`.

- **Qualified packages (`lib/<pkg>/src/`)**:
  - `Crypto` — parity. Wired into `ALPHA_QUALIFIED` and exercised by
    `alias Crypto.Sha256` in driver tests.
  - `Json` — small gap. Needs string interpolation + dotted type
    names + field assignment; no language feature missing.
  - `Http` — small gap. Needs `String.clone()` (shipped),
    field assignment, dotted type names.
  - `Net` — language-feature gap. Process message envelopes use
    type unions (`Tcp.In | Tcp.Out`); blocked until unions land.

- **End-to-end concurrency execution**: spawning works at the LLVM
  level (`expo_rt_spawn` returns a real PID, ABI-correct), but
  `main_wrapper` doesn't call `expo_rt_main_done()` after the body
  runs, so spawned processes never execute. v1 codegen always emits
  the `main_done` call (`expo-codegen/src/compiler.rs:1257`); alpha
  needs the same one-line addition before the trailing `ret i64 0`.

---

## Audit method

For every `.expo` source under `tests/lang/` (the golden suite v1
ships against), grep for the language constructs it uses, then
cross-reference against the `"alpha (typecheck|IR|LLVM) does not
yet ..."` diagnostics in `crates/expo-alpha-*/src/`. Anything a
golden test reaches for that alpha diagnoses (or silently
mishandles) is a parity blocker.

The full list of "not yet" diagnostics lives in:

- `expo-alpha-typecheck/src/pipeline/collect.rs`
- `expo-alpha-typecheck/src/pipeline/lift_signatures/{functions,types,constants}.rs`
- `expo-alpha-typecheck/src/pipeline/resolve/{expr,statements,strings,calls/mod,patterns/mod,literals/binary,closures,match_expr}.rs`
- `expo-alpha-typecheck/src/pipeline/seal/{expressions,patterns}.rs`
- `expo-alpha-ir/src/lower/{expr,ops,body,structs,enums,package,calls,closures}.rs`
- `expo-alpha-ir-llvm/src/{emit/mod,emit/instruction,main_wrapper}.rs`

---

## Gaps gating golden tests

Each row below corresponds to ≥1 file under `tests/lang/` that
alpha can't compile today. **These are the hard parity blockers.**

| Gap                                                                           | Golden tests                                                                                                                               | Alpha gate                                                                                                     |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| **Type unions** (`A \| B`, `type X = …`, typed-binding patterns `p: Post ->`) | `union_types`, `union_named`, `union_typed_binding`, `union_struct_field`, `process_union_msg`                                             | `lift_signatures/types.rs:125`, `resolve/patterns/mod.rs:170`                                                  |
| **Field assignment** (`p.x = 10`)                                             | `structs.expo`                                                                                                                             | `resolve/statements.rs:271`                                                                                    |
| **Dotted type names** (`HTTP.Headers` in type/expr position, no alias)        | `qualified_signature`, `qualified_static_call`                                                                                             | `lift_signatures/types.rs:243,314`                                                                             |
| **Infinite `loop`** (and `break`)                                             | `match_loop_return.expo` (`loop`), `ffi/src/main.expo` (`loop` + `break`)                                                                  | `ExprKind::Loop` falls into `resolve/expr.rs:220` "other"; `break` only allowed inside synthesized for-desugar |
| **Tail-call optimization**                                                    | `tail_call.expo`, `tail_call_unit.expo` (100k-deep recursion)                                                                              | No `TailCall` in alpha-IR-LLVM — would stack-overflow even though it parses and typechecks                     |
| **String interpolation** (`"hello #{x}"`)                                     | ~25 files — `structs`, `methods`, `inline_methods`, `cross_ref`, `match_loop_return`, every `protocols/*`, every `io/*`, every `process_*` | `resolve/strings.rs:27`                                                                                        |
| **`@extern` / `@link` FFI** on inherent methods + `CPtr<T>` arg marshaling    | `ffi/src/main.expo`                                                                                                                        | Annotations parse; alpha-IR-LLVM doesn't emit the C-ABI bridge or marshal pointers                             |

---

## Feature-gap diagnostics with no golden coverage

These all surface "alpha … does not yet support …" diagnostics, but
nothing in `tests/lang/` exercises them, so they're **not** on the
parity critical path. Listed for completeness — useful when the gap
inventory needs an audit pass.

| Gap                                                                                                                            | Diagnostic                                               |
| ------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------- |
| Generic protocol methods (`fn m<U>(…)` inside `protocol`)                                                                      | `collect.rs:580`                                         |
| `type` aliases inside `impl` blocks                                                                                            | `collect.rs:541`                                         |
| Default parameter values                                                                                                       | `lift_signatures/functions.rs:409`                       |
| Default field values                                                                                                           | `collect.rs:480`                                         |
| Named arguments                                                                                                                | `calls/mod.rs:701`                                       |
| Pattern destructuring assignment (`[a, b] = …`)                                                                                | `statements.rs:246`                                      |
| Destructured closure parameters                                                                                                | `closures.rs:194`                                        |
| Dynamic-width binary segments (`<<x::n>>` runtime `n`)                                                                         | `literals/binary.rs:132`                                 |
| Binary patterns in `match`                                                                                                     | `patterns/mod.rs:154` (Phase 7 of `archive/20260511-ALPHA-MATCH-PLAN.md`) |
| List patterns in `match`                                                                                                       | `patterns/mod.rs:162`                                    |
| Annotations on protocols, protocol methods, struct items, enum items, constants (other than `@doc` / `@intrinsic` / `@extern`) | `collect.rs:394,463,500,560,593`                         |

---

## Verified parity (false positives I checked while writing this)

These look like they might be missing from a quick read of the
alpha source tree, but golden coverage confirms them shipped:

- **`for ... in` (statement position)** — desugared in
  `synthesize/for_desugar.rs`; pinned by `process_argv`.
- **`cond` and ternary `cond ? a : b`** — `cond_type_mismatch`
  exercises both arms.
- **Generic protocols** (`protocol Greeter<T>`) — `default_impl`.
- **Default protocol method bodies** — `default_impl` (`FancyGreeter`
  overrides; `HelloGreeter` inherits).
- **Field-as-callable dispatch** (`self.work()` against a fn-typed
  field) — `process` is in `ALPHA_AUTOIMPORT`; `Task<R>.run` lowers.
- **Aliases** (`alias Pkg.Type as X`) — `alias_dep`,
  `package_collision`.
- **`Clone` protocol** for heap primitives — shipped 2026-05-12;
  pinned by `crates/expo-alpha-typecheck/tests/clone.rs` and the
  `*_clone_*` driver tests in `alpha_two_plus_two.rs`.

---

## Recommended sequencing

Roughly cheapest → most expensive, weighted by what each step
unblocks. Each step lands behind seal-asserted output and standalone
tests, per northstar.

### 1. Field assignment (`p.x = 10`)

Single statement-resolve gap; multi-segment `LValue` already lifts.
Unblocks `structs.expo` and the `Http` package. **~1 day.**

### 2. Dotted type names in expr + type position

`Foo.Bar` and `Foo.Bar.method()` without forcing an `alias` first.
Unblocks `qualified_*` golden tests and most `Http`/`Json` user
code. **~1 day.**

### 3. String interpolation (`"hello #{x}"`)

Single resolver path that desugars into `<>` chains; eval and LLVM
both already support `<>` end-to-end. Highest golden-test coverage
per LOC of compiler change. **~1–2 days.**

### 4. Infinite `loop` + `break` / `continue`

Adds `ExprKind::Loop` to resolve / lower; threads `loop_depth`
through the resolver context so `break` and `continue` are gated
to loop bodies. Retires the `__idx_n = __len_n` hack in the
for-desugar. **~2–3 days.**

### 5. Type unions (`A | B`, `type X = A | B`, typed-binding patterns)

The big one. Lift `Type::Union(Vec<ResolvedType>)`, widening
coercion in `unify`, typed-binding patterns (`p: Post -> …`),
exhaustiveness over union arms, layout strategy in IR (likely tag

- payload, mirroring v1's `expo-codegen/src/types/unions.rs`).
  Unblocks `Net` package and 5 golden tests. **~1–2 weeks.**

### 6. Tail-call optimization

Mark self-recursive last-position calls in alpha-ir-llvm, mirroring
`expo-codegen/src/control/{instructions,terminator}.rs`. Unblocks
`tail_call.expo` (currently stack-overflows). **~3–5 days.**

### 7. FFI surface (`@extern "C"` + `CPtr<T>` marshaling)

The annotations parse; the ABI bridge and pointer marshaling
don't. Unblocks `ffi/src/main.expo`. `Net` will likely need this
too once unions land (socket file descriptors). Possibly its own
milestone. **~1 week.**

### 8. End-to-end concurrency execution (`expo_rt_main_done()`)

One-line addition to `main_wrapper.rs`; lights up the
`task_async`, `counter_call`, and `receive_lifecycle` driver
tests originally scoped to the concurrency slice. **~1 day.**

After (1)–(8) the alpha pipeline is at v1 surface parity for
`tests/lang/` plus all qualified stdlib packages, and the v1
toolchain (`expo-typecheck`, `expo-codegen`) can be removed.
