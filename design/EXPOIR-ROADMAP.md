# ExpoIR Roadmap

Forward-looking roadmap for the ExpoIR refactor. Tracks where the
intermediate-representation work stands today, what slices remain, and
the design invariants that have governed the work so far. The original
SIL-style design prose and the full Wave 1-17 narrative live in
[`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md).

---

## 1. Status snapshot

The instruction-level scaffold has landed. `IRProgram` is the canonical
declaration registry. Eight `Lowerer<'a>` lift methods cover six typed
`IRInstruction` variants. Two conditionals (`unless` and `if`-no-else)
run end-to-end through the full IR pipeline today; three call families
(`Call`, static call, method call) plus `FieldLoad`, `BinaryOp`, and
`UnaryOp` reach typed instructions when consumed via the codegen
wrappers' lift-then-fallthrough paths.

What we do _not_ have yet:

- `IRBasicBlock` is owned per-construct (`IRUnless`, `IRIf`) instead of
  free-floating on `IRFunction`.
- `IRFunction` still carries `expo_ast::ast::Function` AST bodies.
- `compile_statement` and `compile_function_body` walk AST end-to-end.
- The `IRInstruction::Stub` bridge is alive (single producer:
  `Lowerer::lower_expr_to_operand`).
- `match` is partially lifted via a parallel pipeline
  (`lower_match` -> `ResolvedMatch` -> `emit_match`) that bypasses
  `execute_instructions`.
- ~14 constructs still go AST -> LLVM with no IR touchpoint at all
  (the full list is in section 3a).

---

## 2. Phase summary

Condensed from 17 waves of work. Full prose lives in
[`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md).

- **Phase 1 -- Typed foundation (done, Waves 1-5).** `TypeRegistry`
  renamed to `LLVMTypeCache`; semantic struct/enum layouts split into
  `TypeLayouts`; `FnState` (LLVM-bound) split from `FnLowerState`
  (semantic); `LowerCtx<'a>` borrow bundle introduced as the single
  gateway between LLVM-bound driver and LLVM-free lowering surface;
  `closure_counter` migration.
- **Phase 2 -- Decision-type extraction (done, Wave 7 + folded).** 39
  `Resolved*` types live in
  [`expo-ir::resolved`](../crates/expo-ir/src/resolved/) across 16
  modules. `expo-codegen` consumes them via thin `compile_*` wrappers.
- **Phase 3 -- `expo-ir` crate (done as scoped).** 24 `lower::*` modules
  host the lift surface; `resolved/`, `lower/`, identity newtypes,
  `TypeLayouts`, `FnLowerState` all live here. The instruction
  containers were deliberately deferred -- they emerged bottom-up from
  Phase 4c real consumers, not top-down from speculation.
- **Phase 4a/b -- Pure-decision lifts (done, Waves 6-10).** 9 LLVM-free
  semantic helpers (Wave 6) plus ~28 pure `resolve_*`/`lower_*` helpers
  (Wave 7) plus the Phase 4b structural cluster (Waves 8a-8d) plus
  monomorphization-in-IR (Wave 10: `monomorphize_struct`/`enum`/
  `function`/`impl_method` planners write `IRProgram`). Companion Wave
  9 introduced opaque mono identities.
- **Phase 4c -- Instruction-level scaffold (in progress, Waves 11-17).**
  Block / terminator / operand vocabulary; `Lowerer<'a>` driver;
  `IRProgram` as canonical callable registry (with three live
  exceptions, called out in section 3b); two conditionals plus three
  call families lifted; expression vocabulary covers
  `BinaryOp`/`UnaryOp`/`FieldLoad`/`Call`/`MethodCall` plus literals
  and `Group`.

---

## 3. Ground-truth state

What's actually in the IR today, so future-you doesn't need to re-audit
to plan a slice.

### 3a. Lift status by construct

| Construct                   | Status                      | Notes                                                                                |
| --------------------------- | --------------------------- | ------------------------------------------------------------------------------------ |
| `unless`                    | Full IR pipeline            | `Lowerer::lower_unless` -> `IRUnless` -> `emit_unless` + `execute_instructions`      |
| `if` (no else)              | Full IR pipeline            | `Lowerer::lower_if_no_else` -> `IRIf` -> `emit_if` + `execute_instructions`          |
| `Call` / `static_call`      | Instruction-only            | `Lowerer::lower_call_or_stub` / `lower_static_call_or_stub` -> `IRInstruction::Call` |
| `MethodCall`                | Instruction-only            | `Lowerer::lower_method_call_or_stub` -> `IRInstruction::MethodCall`                  |
| `FieldAccess`               | Instruction-only            | `Lowerer::lower_field_access_or_stub` -> `IRInstruction::FieldLoad`                  |
| Binary op (most)            | Instruction-only            | `Lowerer::lower_binary_op_or_stub` -> `IRInstruction::BinaryOp`                      |
| Unary op                    | Instruction-only            | `Lowerer::lower_unary_op_or_stub` -> `IRInstruction::UnaryOp`                        |
| Bool/Int/Float literals     | Inline operand              | `IROperand::ConstBool` / `ConstInt` / `ConstFloat`                                   |
| `match`                     | Parallel pipeline           | `lower_match` -> `ResolvedMatch` -> `emit_match`; bypasses `execute_instructions`    |
| `if`/`else` (with else)     | AST -> LLVM                 | Slice 3                                                                              |
| `ternary`                   | AST -> LLVM                 | Slice 3                                                                              |
| `cond`                      | AST -> LLVM                 | Slice 4                                                                              |
| `while` / `loop` / `for`    | AST -> LLVM                 | Slice 6                                                                              |
| `break` / `return`          | AST -> LLVM                 | Slice 6                                                                              |
| `Ident` (locals)            | AST -> LLVM                 | Phase 4d (locals foundation -- restores static-chain GEP optimization)               |
| `assignment` / compound     | AST -> LLVM                 | Phase 4f (statement lowering)                                                        |
| `field_assignment`          | AST -> LLVM                 | Phase 4f (statement lowering)                                                        |
| Binary pattern              | AST -> LLVM                 | Phase 4e Slice 5 (folds into match unification)                                      |
| Struct construction         | AST -> LLVM                 | Phase 4g                                                                             |
| Enum construction           | AST -> LLVM                 | Phase 4g                                                                             |
| Closure construction        | AST -> LLVM                 | Phase 4g (`partial_apply` shape)                                                     |
| String literal              | AST -> LLVM                 | Phase 4g                                                                             |
| String interpolation/concat | AST -> LLVM                 | Phase 4g (`compile_concat`, `compile_string_concat`, `compile_binary_concat`)        |
| `EnumStructEqual`           | AST -> LLVM                 | Phase 4g (multi-block per-variant equality)                                          |
| `spawn` / `receive`         | AST -> LLVM (decision lift) | Phase 4g (process resolvers exist; instruction lift pending)                         |
| `print*` / `panic`          | AST -> LLVM                 | Phase 4g (builtin-call instruction lift)                                             |
| Generic-fn / struct ctor    | AST -> LLVM                 | Phase 4g (call-lift fallthrough cases)                                               |
| `union_wrap`                | AST -> LLVM (decision lift) | Phase 4g                                                                             |

The `Stub` bridge does not even reach most of the AST -> LLVM rows
because they're entered through `compile_statement` / `compile_expr`
directly, not through `Lowerer::lower_expr_to_operand`. Phase 4f
(function bodies in IR) is the moment statements stop walking AST
and most of these constructs first become reachable from the IR
pipeline.

### 3b. `IRProgram` callable-registry exceptions

Wave 16 isn't fully closed -- three callable-symbol paths still
bypass the `IRProgram::insert_function` seam:

- `Compiler::get_or_create_thunk` writes to `fn_ref_thunks` only, not
  `IRProgram`. Synthetic thunks are LLVM-only today.
- `monomorphize_impl_method` short-circuits via `EmitResult::Emitted`
  for stdlib intrinsic methods, bypassing `emit_ir_impl_method`.
- `resolve_generic_call` still consults `Compiler.functions.contains_key`,
  not `IRProgram::contains_function`.

Resolving these is Phase 4c (sequenced first because it is independent
of every other remaining phase).

### 3c. Single-site landmarks

The load-bearing seams every future slice extends:

- `Lowerer::lower_expr_to_operand` in
  [`expo-ir::lower::values`](../crates/expo-ir/src/lower/values.rs) --
  the single `IRInstruction::Stub` constructor; every operand-shaped
  expression flows through here.
- `execute_instructions` in
  [`expo-codegen::control::instructions`](../crates/expo-codegen/src/control/instructions.rs)
  -- the single `IRInstruction` walker; new instruction variants get
  an arm here.
- `emit_terminator` in
  [`expo-codegen::control::terminator`](../crates/expo-codegen/src/control/terminator.rs)
  -- the single `IRTerminator` walker.
- `Compiler::register_function` / `register_extern` in
  [`expo-codegen::compiler`](../crates/expo-codegen/src/compiler.rs) --
  the single declared-callable seam (when not bypassed; see 3b).
- `Compiler::lowerer()` in the same file -- the single per-function
  `Lowerer<'a>` constructor.

---

## 4. Roadmap: remaining work

Each entry: rationale plus a concrete done-when. The remaining IR
sub-phases are ordered by dependency layer rather than construct
sequence -- Waves 12, 14, 15, and 16 each landed because a planned
construct lift discovered a foundation slice it needed first, and
that "interlude" pattern is the signal that construct ordering has
stopped being the natural organizing principle. Foundations now lead;
construct lifts ride on top.

### Phase 4c -- Registry closeout

Resolve the three Wave 16 exceptions so invariant 12 ("every callable
is in `IRProgram`") holds without caveat. Independent of every other
remaining phase; cheap; locks down a contract every later phase
reads. Sequenced first because there is no reason to wait.

- Route `fn_ref_thunks` through `register_function`, or surface them
  as a typed `IRFunctionKind::Thunk`.
- Route stdlib intrinsic methods through `emit_ir_impl_method`, or
  add `IRFunctionKind::Intrinsic`.
- Migrate `resolve_generic_call` to `IRProgram::contains_function`.

**Done when** `Compiler.functions` and `IRProgram.functions` carry
identical key sets and no codegen site reads `functions.contains_key`
for an existence check.

### Phase 4d -- Locals foundation

Lift `ExprKind::Ident` from `Stub` to a typed operand path that
recognizes named locals without minting a `Stub`. High-leverage
precondition for every later construct lift: today every typed-IR
chain breaks at the first `Ident` reference. Trace `if x.value > 5`:
the binary lift recurses to field-access lift, which recurses to the
receiver `Ident(x)` and falls through to `Stub`; the chain becomes
`BinaryOp(FieldLoad(Stub(...)), ConstInt(5))` instead of fully typed.
Lifting `Ident` retroactively widens typed-IR coverage on slices 1-2
and brings every later construct slice up to nearly-end-to-end typed
IR on day one.

Also restores the static-chain GEP optimization that `FieldLoad`
currently sacrifices: with `Ident` known to lowering, multi-hop
field access on a named local can lower as one GEP chain instead of
alloca-store-GEP-load round-trips.

**Done when** `Ident`-rooted expressions stop minting `Stub` and the
static-chain GEP optimization is back at the IR level.

### Phase 4e -- Construct lifts

The reframed construct ladder, free to lift any expression
vocabulary the construct actually reaches because Phase 4d is in
place. Each slice is one construct family plus whatever expression
instructions its body / condition / arms require -- expression
vocabulary is no longer a separate phase queueing behind constructs;
it lifts as part of the slice that needs it.

- **Slice 3 -- `if`/`else` + ternary.** Shape 2: two body blocks
  plus a value merge. Introduces the value-merging story (`IRPhi` or
  block arguments). Folded together because they share the same
  functional shape. **Done when** `compile_if`'s else branch and
  `compile_ternary` both lift to typed IR walked by
  `execute_instructions`.
- **Slice 4 -- `cond`.** N-arm chain of `CondBranch`s. Tests the
  scaffold scaling beyond fixed-N constructs. **Done when**
  `compile_cond` is a thin shim over the unified walker.
- **Slice 5 -- `match` unification.** Existing `lower_match` /
  `emit_match` parallel pipeline converges onto `IRBasicBlock` +
  `execute_instructions`. Pattern bindings become explicit IR (scope
  save/restore lives in lowering, not emission). **Done when**
  `emit_match` is deleted in favor of the unified walker.
- **Slice 6 -- loops (`while`, `loop`, `for`, `break`, `return`).**
  Loop headers become `IRBasicBlock`s with explicit back-edges;
  `break` and `return` become explicit terminators. The
  `tail_position` ambient flag on `FnLowerState` becomes a
  `tail: bool` field on `IRInstruction::Call` / `MethodCall`.
  **Done when** loops carry no ambient state and `tail_position()`
  is deleted.

### Phase 4f -- Function bodies in IR

The structural cut. `IRFunction` stops carrying
`expo_ast::ast::Function` bodies and starts carrying
`Vec<IRBasicBlock>`. `compile_statement`, `compile_function_body`,
and `compile_method_body` lift to IR. Per-construct IR types
(`IRUnless`, `IRIf`, the Slice-3 merge type) dissolve into the
unified block representation; per-construct emit walkers
(`emit_unless`, `emit_if`, etc.) retire in favor of one block
walker. `closure_site_path` and `current_package` move off
`Compiler` onto IR or lowering state. The two production
`unreachable!()` sites (`closures.rs:53`, `expr.rs:301`) disappear
as their ad-hoc fallback shapes become unreachable through the
typed instruction set. `Compiler` becomes a pure consumer of
`IRProgram`.

This is the architectural moment the original SIL-style design
called for and the moment "the lowering / emission split" finally
lands. Slice 7 of the original construct ladder is folded in here
because it is the same structural change: there is no half-state
where `IRFunction` carries both an AST body and a `Vec<IRBasicBlock>`.

**Done when** `expo-codegen` performs no AST traversal, `IRFunction`
holds no AST, and per-construct emit walkers are deleted.

### Phase 4g -- Stub retirement

Lift the still-`Stub`-producing Expr kinds in dependency order, then
delete the `Stub` variant. After Phase 4f every expression is
reached through `lower_expr_to_operand` (because `compile_statement`
no longer walks AST), so the residual `Stub` kinds are exactly the
Expr kinds not yet lifted. Order chosen so each lift's vocabulary is
in place before its dependents.

1. Struct construction.
2. Enum construction.
3. Indexed access.
4. Closure construction (`partial_apply` shape).
5. String literal + string interpolation / `Concat`.
6. `EnumStructEqual` (multi-block per-variant equality).
7. `compile_spawn` / `compile_receive` (decision lifts already exist;
   instruction shape pending).
8. `compile_print*` / `compile_panic` (builtin-call instructions).
9. Builtin / generic-fn / struct-constructor calls (the call-lift
   fallthrough cases that today fall through to `Stub`).

**Done when** `IRInstruction::Stub` is deleted and the crate compiles.

### Phase 5 -- Identity interning

Intern `MonomorphizedTypeIdentifier` / `FunctionIdentifier` /
`VariantIdentifier` to `u32` behind the existing newtype shape. Wave 9
already laid the foundation -- every cache key already wraps a string
through one of these newtypes, so the intern table flip is now a
self-contained slice with no call-site changes. **Done when** the
newtypes wrap `u32` and a single `Interner` owns the string table.

### Phase 6 -- Ownership instructions (raw -> canonical)

`move_value`, `borrow_value`, `end_borrow`, `clone_value`, `drop_value`
as first-class `IRInstruction` variants per the original SIL-style
design. Add the mandatory passes:

- **Ownership verification** -- every value moved, borrowed, or
  dropped exactly once on every control flow path.
- **Clone/drop elimination** -- `clone_value` followed by drop of the
  original collapses to `move_value`; drop of an already-moved value is
  removed.
- **Definite initialization** -- every variable assigned before use on
  all paths. Easier on the IR's CFG than on the AST's tree.

**Done when** raw IR is conservatively annotated, canonical IR is
optimized, and the ownership errors currently caught in the typechecker
become IR-pass diagnostics.

### Phase 7 -- `CodeEmitter` protocol + second backend

Define the backend protocol (Rust trait during bootstrap; Expo
protocol once self-hosted). The LLVM backend becomes the first
implementation, not a special case. Cranelift backend for the REPL is
the natural validation target. Aligns with
[`ROADMAP.md`](ROADMAP.md) Phase 5 (REPL) and Phase 6A (self-hosting).
**Done when** a second backend compiles a non-trivial program through
ExpoIR with no regressions on the LLVM backend.

### Phase 8 -- ARC for shared types

`shared_alloc` / `shared_retain` / `shared_release` / `shared_read` /
`shared_write` as first-class `IRInstruction` variants. ARC
optimization passes:

- **Retain/release pairing** -- eliminate adjacent retain/release on
  the same value.
- **Retain sinking / release hoisting** -- minimize the window where
  the reference is held.
- **Single-owner elision** -- if a shared reference proves not to
  escape the creating process, replace atomic operations with
  non-atomic (or elide).
- **Read-only detection** -- if a process only reads, the backend uses
  read-optimized locking.

Unblocks `shared_map` in [`ROADMAP.md`](ROADMAP.md) Phase 4 Track B
(shared data). **Done when** an ETS-equivalent concurrent hash map
ships in the stdlib backed by `shared_*` instructions.

---

## 5. Design and refactoring invariants

The load-bearing rules every future session must internalize before
touching `expo-ir` or the IR-side of `expo-codegen`. Each one is the
rule plus the concrete behavior it forbids.

1. **SIL-style, not MIR-style.** High-level operations (`switch_enum`,
   `partial_apply`, ownership ops, ARC) survive into the IR. Backends
   emit; they do not reconstruct semantics. Forbids: lowering an enum
   match to manual tag loads + payload offset arithmetic in the IR.

2. **Two-bucket migration discipline.** Every piece migrated into
   `expo-ir` answers "IR data + its query methods" or "lowering scratch
   state." Forbids: rebuilding `Compiler` inside `expo-ir` one index at
   a time. Once `IRProgram` exists, lookups are methods on `IRProgram`,
   not separate registries.

3. **Real consumers drive IR design.** Each instruction variant is
   added by the slice that needs it. Forbids: top-down instruction-set
   sketches with no producers. The deleted speculative instruction set
   from earlier in the refactor is the cautionary tale.

4. **Direct construct names over premature unification.** `IRUnless`
   and `IRIf` stay separate field-for-field-identical structs until
   Phase 4f dissolves both into a free-floating `Vec<IRBasicBlock>`
   on `IRFunction`. Forbids: introducing a polarity-neutral
   `IRConditional` shape now and then renaming it later.
   Renaming-then-deleting is more churn than the bounded duplication.

5. **Control-flow negation lives in lowering.** Encoded by branch-target
   ordering on `IRTerminator::CondBranch` (body on `otherwise` for
   `unless`, body on `then` for `if`), not by an IR `Not` operator or a
   `negated` flag. Forbids: per-construct branch-direction knowledge in
   any backend's cond-branch emission. Value-context negation
   (`let x = !cond`) stays a unary op.

6. **`Stub` is the bridge, not a permanent shape.** Greppable
   retirement marker (`IRInstruction::Stub`). Each Expr kind retires
   `Stub` at its lowering site by introducing a typed variant; the
   final delete is one PR. Forbids: side tables, parallel stores, or
   any "two sources of truth" structure for un-lifted shapes.

7. **Tests pass at every step.** Each wave/slice ships green on
   `just lint`, `cargo test --workspace`, `just test-stdlib`,
   `just doit`. Forbids: "nothing works for 4 weeks" big-bang refactor
   phases. Continuous progress, 25/25 lang-suite always green.

8. **One operand-lowering seam.** `Lowerer::lower_expr_to_operand` is
   the single point every construct uses to thread an expression value
   into the IR. Forbids: parallel operand-lowering paths per
   construct. New construct lifts call this method or fall through to
   `Stub`; they do not invent alternatives.

9. **One walker per IR shape.** `execute_instructions` walks
   `IRInstruction`; `emit_terminator` walks `IRTerminator`. New
   instruction variants extend the existing walker; new constructs do
   not get their own walker except as transitional shims that retire
   at Phase 4f. Forbids: per-construct emission code that
   re-implements walker mechanics.

10. **`LowerCtx` is ambient semantic state; `IRProgram` is an output
    container.** `LowerCtx` carries `&TypeContext`, `&TypeLayouts`,
    `current_package`, `closure_site_path`, `&FnLowerState`.
    `IRProgram` flows through resolvers as an explicit positional
    parameter, not on `LowerCtx`. Forbids: stuffing the IR output
    container into the ambient context bundle. Closures into LLVM-side
    state stay short, one-shot, and at the codegen call site.

11. **Mangling, identities, and registries live in `expo-ir`.** Once a
    registry exists in `IRProgram`, the matching `function_exists` /
    `is_struct_constructor` / `variable_type` closure into codegen
    retires. Forbids: keeping closures alive after their backing
    registry has moved to `expo-ir`.

12. **One-callable-one-`IRFunction`.** Every callable symbol in the
    program -- user, monomorphized, intrinsic, runtime extern, thunk
    -- is an `IRFunction` entry. Forbids: LLVM-only callable side
    tables. Today's three exceptions (Phase 4c) are bugs to close, not
    patterns to preserve.

---

## 6. Cross-references

- [`ROADMAP.md`](ROADMAP.md) Phase 6A (self-hosting) -- consumer of
  Phase 7 (`CodeEmitter` protocol).
- [`ROADMAP.md`](ROADMAP.md) Phase 4 Track B (shared data,
  `shared_map`) -- consumer of Phase 8 (ARC for shared types).
- [`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md) --
  original SIL-style design prose, full Wave 1-17 narrative, the
  instruction set vision, the comparison with other compilers.
