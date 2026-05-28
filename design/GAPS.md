# Known Compiler Gaps

Known limitations, bugs, and workarounds in the Koja compiler. New gaps
should be added here as they're discovered (agent testing, self-hosting,
etc.). For the full design of the iterator protocol replacement, see
[TYPES.md](TYPES.md).

---

## Generic enum unit variants in top-level code

`Option.None` cannot infer `T` without usage context in bare declarations.

**Workaround:** variable type annotations (`z: Option<Int32> = Option.None`).
Inside monomorphized method bodies and closures with return type annotations,
generic enum construction resolves all type parameters automatically.
Struct-literal field positions also propagate the declared field type down
into the initializer, so `Diagnostic{hint: Option.None}` resolves
`Option.None` from `hint: Option<String>` with no extra annotation.

Still affects generic free-function calls where one argument is a generic
unit variant: `Pair.new(self, Option.None)` in a function returning
`Pair<Lexer, Option<String>>` fails to infer `A` and `B` because the return
type isn't propagated into the call. Workaround: use struct literals
directly (`Pair{first: self, second: Option.None}`) where the field-type
hint pins the variant, or bind with a type annotation first.

Re-confirmed 2026-05-27 on both backends; diagnostic now reads
``typecheck cannot infer type parameter `T` of `Global.Option` from unit variant `None` ``.

---

## Process scheduler corrupts callee-saved registers across yield

`tests/lang/io/multi_process.koja` is flaky: in a 500-run stress test of
the standalone binary it crashed 32 times (6.4%) with hard signals
(27 × SIGSEGV, 3 × SIGBUS, 2 × SIGTRAP). `koja run` was masking these as
plain `exit code 1` because `pipeline.rs` does
`status.code().unwrap_or(1)` and a signal-terminated child has
`code() == None`. The flake therefore looks like a logical race in the
language tests, but it's actually a memory-safety bug in the runtime.

**Symptom 1 — dominant pattern (7/8 inspected crashes).** Thread 2 (a
worker) faults with `pc = 0`, `lr = 0`, `fp = 0`, `sp = stack_top`. The
register snapshot is the canonical "post-`ret`-with-LR=0" shape:
`x20 = Coordinator.__spawn_wrapper+44`,
`x24 = koja_runtime::scheduler::process_trampoline+188`. So
`process_trampoline` finished, executed `ret`, and `x30` was loaded as
0 from its prologue's saved frame.

**Symptom 2 — the smoking gun (1/8 crashes).** SIGSEGV inside
`pthread_mutex_lock+12` with the frame chain
`pthread_mutex_lock → std::sys::pal::unix::sync::mutex::Mutex::lock →
koja_rt_receive+320 → multi_process.Coordinator.run+128`. The
`FAR` register contained `0x6c756d20676e6f70` — which, byte-reversed,
is the ASCII string `"pong mul"` (the first 8 bytes of
`"pong multi"`, the Ponger's reply message). `x23 = 0x10578e938`, a
heap address whose first 8 bytes are those exact characters.

**Root cause.** Disassembling `_koja_rt_receive`:

```text
sub  sp, sp, #0x60
stp  x24, x23, [sp, #0x20]          ; saves caller's x23
...
adrp x23, 0x1002a8000
add  x23, x23, #0x730               ; x23 = &SCHED (OnceBox<Mutex<...>>)
...
bl   koja_context_switch            ; yields when mailbox empty
ldapr x0, [x23]                     ; x23 must still be &SCHED here
bl   std::sys::pal::unix::sync::mutex::Mutex::lock
```

After the yield, `x23` was a heap pointer (the address of a
`String` payload containing `"pong multi"`), not `&SCHED`. The
dereference loaded the first 8 bytes of the string into `x0`, which
`pthread_mutex_lock` then treated as a mutex pointer and dereferenced —
SIGSEGV.

For `x23` to be wrong after the resume, one of two things happened:

1. **`koja_context_switch` saved/restored the wrong frame** — i.e. the
   process's saved `sp` in `Scheduler::processes[idx].sp` was stale or
   pointed at a frame from a *different* yield site (e.g. the frame
   from a previous yield inside `koja_rt_receive_timeout` during
   `p.call`, where `x23` legitimately holds the panic-state flag
   address, not `&SCHED`).
2. **`process_trampoline`'s callee-saved register window leaks across
   the Rust↔Koja boundary** — Rust's `bl` to compiled Koja code lets
   the Koja function clobber `x19–x28` freely; on the way back out,
   the trampoline's `ldp` restores them from its own prologue frame.
   But the *yielding* path goes through `koja_context_switch`, which
   saves whatever `x19–x28` happen to be live at the yield point.
   When that yield is inside `koja_rt_receive_timeout` (called from
   the `Ref.call` intrinsic emitted inline in Koja-generated code),
   the saved `x23` is whatever the surrounding Koja frame had there
   — possibly a `String` pointer in flight.

Theory 1 is more likely: the scheduler updates `processes[idx].sp` on
yield, but if there's a window where two yields race against the
`SCHED` mutex (the mutex is dropped *before* the context switch and
reacquired afterwards in `worker_loop`), the saved `sp` from yield N
could be read by the resume that paired with yield N-1. The "post-ret
with LR=0" cases (symptom 1) match this: the frame loaded into
`x19–x30` was *the trampoline's own initial frame*, where `x30` was
the `process_trampoline` entry stub's return-to-nowhere slot, not a
real saved LR.

**Surface area:**
- `koja/crates/koja-runtime/src/scheduler.rs` — `worker_loop`,
  `process_trampoline`, `koja_rt_receive`, `koja_rt_receive_timeout`,
  `YIELD_SP`/`SCHED_SP` TLS handling.
- `koja/crates/koja-runtime/src/arch/aarch64.s` — `koja_context_switch`
  frame layout (160-byte save area at `sp-160..sp`).
- `koja/crates/koja-ir-llvm/src/intrinsics/process.rs` — `Ref.call`
  emits an inline call to `koja_rt_receive_timeout` from within
  Koja-compiled frames, so the yield happens with Koja-managed
  register state.

**Next investigative steps:**
1. Add a debug assertion in `worker_loop`: after reading
   `saved_sp = *yield_sp_ptr`, verify it points within
   `processes[idx].stack_bottom..stack_top`. A mismatch confirms the
   wrong-frame theory.
2. Instrument `koja_context_switch` (debug build) to log
   `(pid, sp_in, sp_out, x23_at_yield)` and dump on resume mismatch.
3. Audit the `SCHED.lock()` drop-and-reacquire pattern in
   `worker_loop` — between the drop and the context-switch is a window
   where another worker can pick the same process if state bookkeeping
   isn't tight.
4. Confirm `init_process_stack` writes the *trampoline*'s expected
   16-byte FP/LR pair at the slot the trampoline's `ldp` will read —
   off-by-one between "saved-frame layout used by context_switch" and
   "saved-frame layout used by trampoline prologue/epilogue" would
   explain symptom 1 directly.

**Workaround until fixed:** none reliable; the test passes ~94% of
runs but should not be enabled in CI gating until the scheduler is
fixed. `koja run` masking signal exits as `1` should also be fixed so
future crashes don't look like benign flakes.

Audited 2026-05-28 via 500-iteration stress run on the standalone
binary at `/tmp/mp_bin` plus inspection of eight macOS CrashReporter
`.ips` files in `~/Library/Logs/DiagnosticReports/`.

---

## Iteration protocol limits (`Enumeration<T>`)

`Enumeration<T>` requires `length()` + `get(index)`, locking `for` to
index-based while loops. This precludes lazy iteration, streaming, and any
non-random-access collection (maps, linked lists, generators).

Pre-v1.0, replace with an `Iterator<T>` protocol using
`next(move self) -> Option<Pair<T, Self>>`. `get` now returns `Option<T>`.
Codegen change is contained to `compile_for` in `loops.rs`; List/String
impls wrap existing index-based access in iterator state.

The current `for` loop hides the `Option` from the user (unwraps
automatically since iteration is bounds-checked). With lazy iteration,
`Option` becomes the termination mechanism -- `for` desugars to
`loop { match iter.next() ... }` and `None` breaks the loop.

Full design in [TYPES.md](TYPES.md) "Iterator protocol redesign" section.

---

## Nested types (`MyApp.Config`) deferred

Declaring a `struct` or `enum` inside another `struct`/`enum` body, accessed
via dotted syntax (`MyApp.Config`, `Lexer.Token`, `Json.Decoder`), is not
supported. The struct/enum body parser in
`koja-parser/src/decl.rs` only accepts fields and inline `fn` methods --
nested type items would need to be allowed in the same loop. Collection in
`koja-typecheck/src/collect.rs` would need to recurse into bodies and
register nested decls under their dotted name.

The naming machinery is already friendly: `TypeIdentifier.name` is an
opaque `String`, and `qualified_name()` / `mangle_name` preserve dots, so
`name = "MyApp.Config"` flows through codegen registration with zero
changes. Identity stays at `(package, name)` -- no `DefId` overhaul needed
(unlike local-types-in-function-bodies).

The two real obstacles:

1. **`path.len() == 2` resolver assumption.**
   `koja-typecheck/src/types.rs::resolve_type_expr_full` treats a 2-segment
   path as `package.Type`. We'd need a third precedence rule for
   `OuterType.NestedType` and a tie-break when both interpretations exist
   (e.g. an aliased package whose name shadows a local type).

2. **`Foo.Bar` ambiguity with enum variants.**
   The parser sends both `Color.Red` (variant) and `MyApp.Config` (would-be
   nested type) down the same enum-construction AST shape. Today
   `koja-typecheck/src/expr.rs::infer_enum_construction` only succeeds if
   the head is an enum; the fallback would need to also try resolving the
   path as a nested type when followed by a struct literal or in type
   position.

Side bits: `classify_impl_target` in `check.rs` only handles
`path.len() == 1`, so `impl MyApp.Config` would need a one-line extension.
Bare `Config` resolving to `MyApp.Config` inside `impl MyApp` (the
"implicit prefix" nicety) would add ~1-2 days of `CheckEnv` plumbing and
can be deferred to a v2 by requiring fully-qualified names initially.

**Cost estimate:** ~1-2 weeks for non-generic nested types; +1-2 more
weeks for generics.

**Why deferred:** much cheaper than local-types-in-function-bodies but
still a sizeable feature; not a 1.0 blocker. Tracked here so the design
analysis isn't lost.

---

## Stdlib signatures lie about ownership for `List.append` / `Map.put`

`List.append(move self, item: T)` and `Map.put(move self, key: K, value: V)`
declare their elements as `borrow`, but the intrinsic implementations take
ownership of the stored value (the list/map's internal storage just records
the heap pointer). The caller's slot stays Live & Owned past the call, so
the fn-exit `DropLocal` frees a payload the container still references —
surfaces as `Utf8Error` panics or silent garbage reads under the LLVM
backend when a `<>`-built local is appended/put inside a helper and the
helper returns the container. The `koja-ir-eval` backend masks the bug
because frame teardown doesn't actually free heap payloads.

Minimal repro (`/tmp/koja-gaps-triage/followup_b_list_literal.kojs`):

```koja
fn build -> List<String>
  text = "hello" <> " world"
  [text]
end
build().get(0).print()  # LLVM: Utf8Error panic; eval: Some("hello world")
```

Two valid fixes:

1. **Mark the params `move`** (`fn append(move self, move item: T)`). Honest
   semantically, but breaks stdlib callers like `List.filter` /
   `List.map` that today do `result.append(item)` against a borrowed
   loop binding — those would need to clone explicitly. Best long-term
   answer.

2. **Have the intrinsic copy heap payloads on insert.** Keeps the
   borrow signature honest at runtime cost. Probably wrong for a
   "moves by default" language.

The IR-side machinery to support option 1 is already in place
([`koja-ir/src/lower/calls.rs::consume_at_mode`](../crates/koja-ir/src/lower/calls.rs))
— flipping the stdlib signatures will route the move through the same
`MoveOutLocal` path that the struct-field-init and call-arg fixes
already use. Not done here because it's a non-trivial stdlib refactor.

Audited 2026-05-03 · Bug section re-triaged 2026-05-27 (seven fixed
entries removed: `Debug.format` tuple payloads, nested type-aliased
unions, bare closure expression statements, closures capturing `self`,
specialized self-nested impls, keyword-as-identifier silent drop, and
`<>` concat into a returned struct field corrupting under LLVM; one
new entry added: dishonest borrow signatures on `List.append` /
`Map.put`).

# Audit: AST / grammar / LANGUAGE.md / ROADMAP.md / IR / codegen drift

Inventory of every discrepancy between `koja-ast`, `koja-parser`,
`grammar.ebnf`, `LANGUAGE.md`, `design/ROADMAP.md`, and downstream
`koja-ir` / `koja-codegen` (as of the v1 pipeline). Grouped by category
so each item can be triaged independently: remove the cruft, tighten
the AST, or just reconcile the docs.

## B. AST shapes never produced by the parser (dead AST subspace)

### B1. `AssignTarget::Pattern` with non-trivial patterns

- **Grammar:** [grammar.ebnf:150-152](../grammar.ebnf) `assignment = IDENT, ":", type_expr, "=", expr | lvalue, "=", expr | pattern, "=", expr`.
- **AST:** [ast.rs:410-415](../crates/koja-ast/src/ast.rs) `AssignTarget::LValue | AssignTarget::Pattern`.
- **Parser reality:** `try_expr_to_pattern` ([stmt.rs:146-157](../crates/koja-parser/src/stmt.rs)) only accepts `Ident` (→ `Binding`) and `_` (→ `Wildcard`). List, struct, enum, and OR patterns on assignment LHS are _not_ parseable today.
- **Typecheck:** `AssignTarget::Pattern(_) => {}` is empty ([stmt.rs:130-131](../crates/koja-typecheck/src/stmt.rs)).
- **Codegen:** errors with `destructuring patterns not yet supported` ([stmt.rs:278-280](../crates/koja-codegen/src/stmt.rs)) for anything non-trivial.
- **LANGUAGE.md lines 1831-1836:** "parsed and/or type-checked" — overstates.

**Action:** tighten `AssignTarget` to
`AssignTarget::LValue(LValue) | AssignTarget::Binding { name, wildcard: bool }` —
or just keep `Pattern` but document the pattern subspace actually
accepted. Update LANGUAGE.md Planned Features to read "designed; not
parsed yet".

### B2. `ClosureParam::Destructured` inside `ShortClosure`

- **Grammar:** line 246-247 allows `(a, b) -> expr`.
- **Parser:** `expr_to_closure_params` ([construct.rs:474-492](../crates/koja-parser/src/construct.rs)) handles only `Ident`, `_`, and single-element `Group`. A parenthesized list short closure collides with `parse_paren_expr` tuple rejection.
- **Block `Closure`:** `Destructured` _is_ produced from `parse_closure_params` ([construct.rs:424-435](../crates/koja-parser/src/construct.rs)). So the variant isn't dead overall — just dead for short closures.

**Action:** either remove destructured-form from `closure_param_short`
in grammar, or implement it. Grammar line 246-247 is the liar today.

---

## C. Grammar.ebnf vs parser shape mismatches (grammar lies, parser right)

### C1. `cond` mandatory `else`

- **Grammar:** lines 279-284 say `cond` is `{ cond_arm } end` with no else arm.
- **Parser:** `parse_cond_expr` ([control.rs:167-203](../crates/koja-parser/src/control.rs)) **requires** an `else -> ...` terminal arm.

**Action:** update grammar.ebnf to reflect parser truth
(`cond_expr = "cond" , { cond_arm } , "else" , "->" , match_body , "end"`).

### C2. Missing `move` modifier on `closure_param`

- **Grammar:** lines 238-241 — no `move`.
- **Parser:** accepts `move` for block closure params ([construct.rs:418-422](../crates/koja-parser/src/construct.rs)).

**Action:** add `[ "move" ]` to `closure_param` in grammar.ebnf.

### C3. `constant_decl` accepts TypeIdent as name

- **Grammar:** line 472 — `IDENT` only.
- **Parser:** [decl.rs:657-661](../crates/koja-parser/src/decl.rs) — accepts `Ident | TypeIdent`.

**Action:** tighten parser to match grammar (constants must be `IDENT`).

### C4. Pattern literals and multiline strings

- **Grammar:** `pattern → literal → multiline_string_lit` legal.
- **Parser:** `parse_literal_pattern` ([pattern.rs:74-95](../crates/koja-parser/src/pattern.rs)) handles `StringStart` only, not `MultilineStringStart`.

**Action:** trivial parser fix (a few lines) or disallow in grammar.
Probably fix the parser since the feature is cheap.

---

## D. LANGUAGE.md drift (docs lie about reality)

### D1. `Process` protocol surface (biggest documented lie)

- **LANGUAGE.md:** shows `fn new(config: C) -> Self`, `handle -> Self | StopReason`, default `run` dispatching `Pair<M, Option<ReplyTo<R>>>` (lines ~991-1042).
- **Reality:** stdlib has `fn start(move config: C) -> Result<Self, StopReason>`, `handle -> Step<Self>`, and `run` also dispatches `Lifecycle` ([process.koja:162-206](../lib/global/src/process.koja)). Typecheck requires `spawn Type.start(config)` form ([expr.rs:312-318](../crates/koja-typecheck/src/expr.rs)).

**Action:** rewrite the Concurrency section to match reality — `start`
not `new`, `Step<Self>` not union, mention `Lifecycle` and
`Step.Continue` / `Step.Done`.

### D2. `Process` example won't copy-paste

- The `spawn Counter.new(Counter{count: 0})` example (line ~1045) is wrong on two counts — `new` should be `start`, and the arg pattern doesn't match the signature.

**Action:** replace with a minimal copy-pasteable `Counter` example
matching today's Process protocol.

### D4. `receive ... after` underdocumented

- LANGUAGE.md lines 1133-1139 show only the mailbox arm.
- Parser supports `after timeout -> body` and codegen emits `koja_rt_receive_timeout`.

**Action:** add an `after` example.

### D5. `Ref<M, R>` missing `send_after`

- Stdlib exposes `send_after(self, msg, delay_ms)` at [process.koja:147-154](../lib/global/src/process.koja).
- LANGUAGE.md lists cast / call / signal / kill / alive? only.

**Action:** add `send_after` to the Ref API list.

### D7. `Debug` auto-derive for generics is degraded

- LANGUAGE.md ~1603 says Debug is auto-derived "for all types with rich formatting".
- Reality: generic types get a type-name-only `format` because `<A: Debug>` bounds aren't inferred ([synthesize.rs:13-23](../crates/koja-typecheck/src/synthesize.rs)).

**Action:** note the generics limitation in docs.

### D8. Struct destructuring assignment status

- LANGUAGE.md 1831-1836 says "parsed and/or type-checked" — typecheck is a no-op, codegen errors.

**Action:** demote to "designed; not parsed yet" pending
shorthand-in-struct-pattern work.

---

## F. Internal AST/AST-user consistency findings (minor)

### F1. `Annotation.value` has 2 variants but grammar allows 3

- **AST:** `AnnotationValue::String | False` ([ast.rs:69-75](../crates/koja-ast/src/ast.rs)).
- **Grammar:** line 442-444 includes `string_lit | multiline_string_lit | "false"`.
- `String` variant holds both single-line and multiline — parser collapses. OK in practice, but grammar suggests a distinction that doesn't exist.

**Action:** tweak grammar comment or leave — low priority.

---

## Recommended execution order

1. **Category C** (grammar.ebnf sync) — 5 minutes, grammar-only.
2. **Category D** (LANGUAGE.md drift) — rewrite Concurrency section, fix TOC, fix `receive` / `reply` / `send_after` / generics note.
3. **Category B / F** (AssignTarget tightening, short closure destructure, other minor cleanups) — pick off as bite-sized PRs.

Each step is independent and can land in its own commit/PR.
