# ExpoIR Roadmap

Forward-looking roadmap for the ExpoIR refactor. Tracks where the
intermediate-representation work stands today, what slices remain, and
the design invariants that have governed the work so far. The original
SIL-style design prose and the full Wave 1-17 narrative live in
[`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md).

---

## 1. Status snapshot

The instruction-level scaffold has landed. `IRProgram` is the canonical
declaration registry. Twelve `Lowerer<'a>` lift methods cover ten typed
`IRInstruction` variants. Four conditionals (`unless`, `if`-no-else,
`if`/`else`, ternary) run end-to-end through the full IR pipeline
today; three call families (`Call`, static call, method call) plus
`FieldChain` / `FieldLoad`, `LoadLocal` / `LoadConst` / `MakeFnRef`,
`BinaryOp`, and `UnaryOp` reach typed instructions when consumed via
the codegen wrappers' lift-then-fallthrough paths. The IR-level
value-merging primitive (`IRInstruction::Phi`) is in place and
load-bearing for both ternary (pre-staged at lowering) and the
with-else `if` (synthesized at emit time).

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
- ~13 constructs still go AST -> LLVM with no IR touchpoint at all
  (the full list is in section 3a).

---

## 2. Phase summary

Condensed from 21 waves of work. The Wave 1-17 prose lives in
[`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md); Waves 18-21
are summarized inline below.

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
- **Phase 4c -- Instruction-level scaffold + registry closeout (done,
  Waves 11-18).** Block / terminator / operand vocabulary;
  `Lowerer<'a>` driver; two conditionals plus three call families
  lifted; expression vocabulary covers
  `BinaryOp`/`UnaryOp`/`FieldLoad`/`Call`/`MethodCall` plus literals
  and `Group`. Wave 18 closed the callable-registry contract -- the
  Wave-16 exceptions are gone (see the `Phase 4c -- Registry closeout`
  entry in section 4 for the per-change detail).
- **Phase 4d -- Callable classification and Extern attributes (done,
  Wave 19).** `Extern` becomes a struct variant carrying the standard
  attribute set (`abi`, `link_name`, `link_lib`, `is_variadic`); a new
  `MainEntry` variant tags the transitional `fn main` synthesis pair;
  the `register_extern` catch-all unwinds so non-generic user
  functions/methods register as `Free`/`Method` (matching the kinds the
  monomorphize planner has always written), `debug.rs` per-type format
  emitters register as `Intrinsic`, and the user `@extern "C"`
  annotation finally flows end-to-end into IR. Closes the unfinished
  half of commit `60618c0` -- see the `Phase 4d` entry in section 4
  for the per-change detail.
- **Phase 4e -- Locals foundation (done, Wave 20).** `ExprKind::Ident`
  and `ExprKind::Self_` lift out of `Stub` into discriminated typed
  instructions (`LoadLocal` for in-scope bindings, `LoadConst` for
  module constants, `MakeFnRef` for top-level function-as-value); a
  new `FieldChain` instruction restores the static-chain GEP
  optimization for chains rooted at a named local (`a.b.c`,
  `self.origin.x`) by delegating to the existing
  `emit_chain_field_access` helper. The IR-side classifier reaches
  the codegen-side variables map through a new `LocalBindings` trait
  on `LowerCtx` / `Lowerer`, so `expo-ir` stays LLVM-free while still
  honoring the precedence `compile_expr` uses today. See the
  `Phase 4e` entry in section 4 for the per-change detail.
- **Phase 4f Slice 3 -- `if`/`else` + ternary (done, Wave 21).** The
  with-else `if` and ternary expressions both lift to typed IR walked
  by `execute_instructions`. New shape-2 IR types `IRIfElse` (arms
  remain AST `Vec<Statement>` stubs until Phase 4g) and `IRTernary`
  (arms fully instructionized -- both pre-stage their merge phi).
  New `IRInstruction::Phi { dest, incomings, ty }` is the canonical
  IR-level value-merging primitive: pre-staged at lowering for
  ternary (where both arms are pure expressions and their result
  operands are known), synthesized at emit time for `if`/`else`
  (where the actual end blocks of each arm are known only after
  walking the AST stubs, and the construct may fall through to
  `Ok(None)` when either arm is statement-only). `execute_instructions`
  gained an `Option<&block_map>` parameter (Phi needs LLVM block
  handles to resolve incomings) and a caller-managed `&mut value_map`
  so multi-call constructs (ternary's entry / then / else / merge
  chain) can share SSA values across successive invocations. See the
  `Phase 4f Slice 3` entry in section 4 for the per-change detail.

---

## 3. Ground-truth state

What's actually in the IR today, so future-you doesn't need to re-audit
to plan a slice.

### 3a. Lift status by construct

| Construct                   | Status                      | Notes                                                                                                                                |
| --------------------------- | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `unless`                    | Full IR pipeline            | `Lowerer::lower_unless` -> `IRUnless` -> `emit_unless` + `execute_instructions`                                                      |
| `if` (no else)              | Full IR pipeline            | `Lowerer::lower_if_no_else` -> `IRIf` -> `emit_if` + `execute_instructions`                                                          |
| `if`/`else` (with else)     | Full IR pipeline            | `Lowerer::lower_if_else` -> `IRIfElse` -> `emit_if_else` (merge phi synthesized inline; arms remain AST stubs until Phase 4g)        |
| `ternary`                   | Full IR pipeline            | `Lowerer::lower_ternary` -> `IRTernary` -> `emit_ternary` + `execute_instructions` (merge phi pre-staged in `merge_instructions`)    |
| `Call` / `static_call`      | Instruction-only            | `Lowerer::lower_call_or_stub` / `lower_static_call_or_stub` -> `IRInstruction::Call`                                                 |
| `MethodCall`                | Instruction-only            | `Lowerer::lower_method_call_or_stub` -> `IRInstruction::MethodCall`                                                                  |
| `FieldAccess` (chains)      | Instruction-only            | `Lowerer::lower_field_access_or_stub` -> `IRInstruction::FieldChain` (rooted at named local; delegates to `emit_chain_field_access`) |
| `FieldAccess` (value recv)  | Instruction-only            | `Lowerer::lower_field_access_or_stub` -> `IRInstruction::FieldLoad` (fallback for non-binding receivers)                             |
| `Ident` (locals)            | Instruction-only            | `Lowerer::lower_ident_or_stub` -> `IRInstruction::LoadLocal`                                                                         |
| `Ident` (constants)         | Instruction-only            | `Lowerer::lower_ident_or_stub` -> `IRInstruction::LoadConst`                                                                         |
| `Ident` (function-as-value) | Instruction-only            | `Lowerer::lower_ident_or_stub` -> `IRInstruction::MakeFnRef`                                                                         |
| `Self_`                     | Instruction-only            | `Lowerer::lower_local_load_or_stub` -> `IRInstruction::LoadLocal { name: "self" }`                                                   |
| Binary op (most)            | Instruction-only            | `Lowerer::lower_binary_op_or_stub` -> `IRInstruction::BinaryOp`                                                                      |
| Unary op                    | Instruction-only            | `Lowerer::lower_unary_op_or_stub` -> `IRInstruction::UnaryOp`                                                                        |
| Bool/Int/Float literals     | Inline operand              | `IROperand::ConstBool` / `ConstInt` / `ConstFloat`                                                                                   |
| `match`                     | Parallel pipeline           | `lower_match` -> `ResolvedMatch` -> `emit_match`; bypasses `execute_instructions`                                                    |
| `cond`                      | AST -> LLVM                 | Slice 4                                                                                                                              |
| `while` / `loop` / `for`    | AST -> LLVM                 | Slice 6                                                                                                                              |
| `break` / `return`          | AST -> LLVM                 | Slice 6                                                                                                                              |
| `assignment` / compound     | AST -> LLVM                 | Phase 4g (statement lowering)                                                                                                        |
| `field_assignment`          | AST -> LLVM                 | Phase 4g (statement lowering)                                                                                                        |
| Binary pattern              | AST -> LLVM                 | Phase 4f Slice 5 (folds into match unification)                                                                                      |
| Struct construction         | AST -> LLVM                 | Phase 4h                                                                                                                             |
| Enum construction           | AST -> LLVM                 | Phase 4h                                                                                                                             |
| Closure construction        | AST -> LLVM                 | Phase 4h (`partial_apply` shape)                                                                                                     |
| String literal              | AST -> LLVM                 | Phase 4h                                                                                                                             |
| String interpolation/concat | AST -> LLVM                 | Phase 4h (`compile_concat`, `compile_string_concat`, `compile_binary_concat`)                                                        |
| `EnumStructEqual`           | AST -> LLVM                 | Phase 4h (multi-block per-variant equality)                                                                                          |
| `spawn` / `receive`         | AST -> LLVM (decision lift) | Phase 4h (process resolvers exist; instruction lift pending)                                                                         |
| `print*` / `panic`          | AST -> LLVM                 | Phase 4h (builtin-call instruction lift)                                                                                             |
| Generic-fn / struct ctor    | AST -> LLVM                 | Phase 4h (call-lift fallthrough cases)                                                                                               |
| `union_wrap`                | AST -> LLVM (decision lift) | Phase 4h                                                                                                                             |

The `Stub` bridge does not even reach most of the AST -> LLVM rows
because they're entered through `compile_statement` / `compile_expr`
directly, not through `Lowerer::lower_expr_to_operand`. Phase 4g
(function bodies in IR) is the moment statements stop walking AST
and most of these constructs first become reachable from the IR
pipeline.

### 3b. `IRProgram` callable-registry contract

As of Wave 19, every callable symbol is in `IRProgram` with a typed
`IRFunctionKind` (`Extern` / `Free` / `Intrinsic` / `MainEntry` /
`Method` / `Thunk`) that honestly classifies what it is. Each variant
has exactly one registration helper on `Compiler` and all six funnel
through `register_function`'s dual write to `IRProgram` + the
LLVM-handle map, so the two stores cannot drift.

Wave 19 cleared the `register_extern` overload that lingered after
commit `60618c0`: every non-generic user free function (top-level
`fn foo`) registers as `Free`, every non-generic user impl method
registers as `Method` (matching the kinds the monomorphize planner
already wrote for generics), every per-type `debug` format emitter
registers as `Intrinsic`, the LLVM `main` / `__expo_user_main` pair
registers as `MainEntry`, and `Extern` is reserved for genuinely
foreign-linked symbols carrying `ExternAttrs` sufficient for any
backend to declare and link them without consulting the LLVM module.

Four of the six pre-Wave-18 `Compiler.functions.contains_key`
existence checks (call-time guards in `calls.rs`, `structs.rs`,
`generics.rs::monomorphize_impl_method`) migrated to
`IRProgram::contains_function`. The other two stay on
`Compiler.functions.contains_key` deliberately: they sit at the top
of `emit_ir_function` / `emit_ir_impl_method` and ask "has this
decl's LLVM body been bound yet?", which is a `Compiler.functions`
question because the planner pre-populates `c.ir` immediately
before `emit_ir_*` is called. This is the one place
`Compiler.functions` and `IRProgram.functions` deliberately
disagree -- `IRProgram` is "decls planned"; `Compiler.functions`
is "LLVM bodies bound". New exceptions to one-callable-one-
`IRFunction` would be regressions, not patterns to preserve (see
invariant 12).

### 3c. Single-site landmarks

The load-bearing seams every future slice extends:

- `Lowerer::lower_expr_to_operand` in
  [`expo-ir::lower::values`](../crates/expo-ir/src/lower/values.rs) --
  the single `IRInstruction::Stub` constructor; every operand-shaped
  expression flows through here.
- `execute_instructions` in
  [`expo-codegen::control::instructions`](../crates/expo-codegen/src/control/instructions.rs)
  -- the single `IRInstruction` walker; new instruction variants get
  an arm here. Takes an `Option<&block_map>` (required when the
  instruction sequence may contain `IRInstruction::Phi`) and a
  caller-managed `&mut value_map` so multi-call constructs (ternary
  threads entry / then / else / merge through one shared map) can
  share SSA values across successive invocations.
- `IRInstruction::Phi` in
  [`expo-ir::values`](../crates/expo-ir/src/values.rs) -- the
  canonical IR-level value-merging primitive. `incomings:
Vec<(IRBlockId, IROperand)>` ties merge-time values to their
  predecessor blocks; the codegen executor synthesizes
  `build_phi(llvm_ty, name)` and walks the incomings issuing
  `add_incoming((value, llvm_block))`. The phi's LLVM type is
  derived from the first materialized incoming value (always
  concrete) rather than `to_llvm_type(ty, ...)` (which fails for
  generic-arg-bearing types like `Result<Unknown, _>` common in
  stdlib pipelines). Pre-staged at lowering for ternary;
  synthesized at emit time for `if`/`else` (where statement-bodied
  arms make pre-staging impossible until Phase 4g).
- `emit_terminator` in
  [`expo-codegen::control::terminator`](../crates/expo-codegen/src/control/terminator.rs)
  -- the single `IRTerminator` walker.
- `Compiler::register_function` / `register_extern` /
  `register_free` / `register_intrinsic` / `register_main_entry` /
  `register_method` / `register_thunk` in
  [`expo-codegen::compiler`](../crates/expo-codegen/src/compiler.rs) --
  the single declared-callable seam. Each variant of `IRFunctionKind`
  has exactly one registration helper; the six helpers cannot drift
  because they all funnel into `register_function`'s dual write
  (`IRProgram` + LLVM-handle map).
- `Compiler::lowerer()` in the same file -- the single per-function
  `Lowerer<'a>` constructor.
- [`expo-ir::lower::LocalBindings`](../crates/expo-ir/src/lower/ctx.rs)
  trait -- the single seam through which IR lowering asks "is this
  name an in-scope local binding, and if so, what's its type?".
  Implemented by `expo-codegen::compiler::FnState` (forwarding to
  `fn_state.variables`); installed on `LowerCtx.locals` /
  `Lowerer.locals` by every `Compiler::lower_ctx*` /
  `Compiler::lowerer` constructor. Keeps `expo-ir` LLVM-free without
  forcing a parallel binding mirror -- the codegen-side variables map
  remains the source of truth.

---

## 4. Roadmap: remaining work

Each entry: rationale plus a concrete done-when. The remaining IR
sub-phases are ordered by dependency layer rather than construct
sequence -- Waves 12, 14, 15, and 16 each landed because a planned
construct lift discovered a foundation slice it needed first, and
that "interlude" pattern is the signal that construct ordering has
stopped being the natural organizing principle. Foundations now lead;
construct lifts ride on top.

### Phase 4c -- Registry closeout (Done, Wave 18)

Resolved the three Wave 16 exceptions so invariant 12 ("every callable
is in `IRProgram`") holds without caveat. Independent of every other
remaining phase; cheap; locks down a contract every later phase reads.
Sequenced first because there was no reason to wait.

- ~~Route `fn_ref_thunks` through `register_function`, or surface them
  as a typed `IRFunctionKind::Thunk`.~~ **Done** -- new
  `Compiler::register_thunk` helper routes through `register_function`
  with `IRFunctionKind::Thunk { wraps }`; `get_or_create_thunk` calls
  it instead of writing to `fn_ref_thunks` directly. The
  wraps-keyed `fn_ref_thunks` cache stays on `Compiler` per the
  two-bucket rule (LLVM-bound state lives in codegen).
- ~~Route stdlib intrinsic methods through `emit_ir_impl_method`, or
  add `IRFunctionKind::Intrinsic`.~~ **Done** --
  `IRFunctionKind::Intrinsic { base_type, method_name }` added,
  `Compiler::register_intrinsic` helper extracted, and ~21
  stdlib-intrinsic emitter sites in `list.rs` / `map.rs` / `set.rs` /
  `process.rs` / `hashtable.rs` / `intrinsics/cptr.rs` migrated. Base
  types are `List` / `Map` / `Set` / `Ref` / `ReplyTo` / `CPtr`.
- ~~Migrate `resolve_generic_call` to
  `IRProgram::contains_function`.~~ **Done (qualified)** -- four of
  six `Compiler.functions.contains_key` call-time guards
  (`calls.rs:208`, `structs.rs:514+972`, `generics.rs:340`) migrated
  to `c.ir.contains_function`. The other two (`generics.rs:528+633`
  inside `emit_ir_function` / `emit_ir_impl_method`) deliberately
  stay on `Compiler.functions` because they ask "has the LLVM body
  been bound yet?", which is a `Compiler.functions` question -- the
  planner has already populated `c.ir` immediately before
  `emit_ir_*` is called. This is the planner-vs-emit semantic split
  documented in section 3b, not an exception to invariant 12.

**Outcome.** `IRProgram` is the canonical typed callable registry
across `Free` / `Method` / `Extern` / `Intrinsic` / `Thunk`. Every
registration funnels through `Compiler::register_function` (directly
or via the four typed helpers `register_extern` / `register_intrinsic`
/ `register_thunk`), so the dual write to `c.ir` + `c.functions`
cannot drift.

### Phase 4d -- Callable classification and Extern attributes (Done, Wave 19)

Reframed during planning: the originally scoped "Extern attributes"
work was inheriting the unfinished half of commit `60618c0` (which
mechanically replaced `c.functions.insert(...)` with `register_extern`
without committing to per-site classification). After the audit, the
phase covers two tightly coupled pieces in one wave -- the variant
classification fix that made `Free`/`Method` actually live for
non-generic user code, and the Standard `Extern` attribute set that
closes the original `printf`-vs-`malloc` indistinguishability bug
plus the user `@extern "C"` payload drop.

- ~~`IRFunctionKind::Extern` is a unit variant; backends cannot
  recover the C ABI shape (variadic, link library, link name) from
  `IRProgram` alone.~~ **Done** -- `Extern` is now a struct variant
  carrying `ExternAttrs { abi: ExternAbi, is_variadic, link_lib,
link_name }`. `ExternAbi` is a single-variant enum (`C`) so future
  ABIs drop in without a breaking churn. `builtins.rs::decl` reads
  `is_variadic` straight from the LLVM `FunctionType::is_var_arg`,
  so the ~40 hand-rolled C/runtime decl call sites stay as
  `decl(c, name, ty)` lines.
- ~~User-source `@extern "C"` annotation is parsed but the codegen
  registration path drops it on the floor.~~ **Done** --
  `extract_extern_attrs(annotations, is_variadic)` lifts the existing
  `extract_link_symbol` to return the full attribute bundle from
  `@extern "C"` and `@link "lib"` / `@link "lib:symbol"`. Both
  user-FFI registration sites in `compiler.rs` (free fn and method
  paths) now thread these attrs through `register_extern`.
- ~~`__expo_user_main` and `debug.rs` formatting helpers are
  misclassified as `Extern` today because they happen to register
  without a normal Expo AST.~~ **Done** -- new unit variant
  `IRFunctionKind::MainEntry` covers both the LLVM `main` C entry
  (which calls `expo_rt_spawn(__expo_user_main, ...)`) and
  `__expo_user_main` itself; doc comment notes the variant is
  transitional pending `fn main` retirement. The `debug.rs`
  per-primitive (`call_format` PrimitiveIntrinsic fallback) and
  per-user-type (`begin_synthesis`) format emitters now register as
  `Intrinsic { base_type, method_name: "format" }`.
- ~~`Free` and `Method` are only ever written by the monomorphize
  planner; every non-generic user free fn / method registers as
  `Extern` via the catch-all path at `compiler.rs:921` /
  `compiler.rs:989`.~~ **Done** -- new helpers `register_free` and
  `register_method` mirror the `Free` / `Method` payloads the
  monomorphize planner uses (with empty `subst`); both call sites
  now branch on `is_extern_c_decl` and route to the appropriate
  helper. `Counter.count_down` and `fn main`'s body holder
  (`__expo_user_main`) are correctly typed in IR.

**Outcome.** `IRProgram` now honestly classifies every callable.
`Extern` means precisely "linker resolves this and `ExternAttrs`
tells you how"; `Free` / `Method` carry an Expo AST regardless of
whether the function is generic; `Intrinsic` covers any method-keyed
backend-emitted body (stdlib types and per-user-type derived
methods alike); `MainEntry` flags the transitional `fn main`
synthesis pair; `Thunk` covers calling-convention adapters. Six
typed helpers on `Compiler`, all funneling through
`register_function`'s dual write, are the single declared-callable
seam (see section 3c).

### Phase 4e -- Locals foundation (Done, Wave 20)

Lifted `ExprKind::Ident` and `ExprKind::Self_` out of `Stub` into
discriminated typed instructions matching the three populations
`compile_expr`'s `Ident` arm has always handled (locals, module
constants, function-as-value), and restored the static-chain GEP
optimization at the IR level via a new `FieldChain` instruction.
High-leverage precondition for every later construct lift: typed-IR
chains used to break at the first `Ident` reference (`if x.value > 5`
became `BinaryOp(FieldLoad(Stub(...)), ConstInt(5))` instead of fully
typed). Lifting `Ident` retroactively widens typed-IR coverage on
slices 1-2 and brings every later construct slice up to
nearly-end-to-end typed IR on day one.

- ~~`ExprKind::Ident` and `ExprKind::Self_` mint `Stub` for every
  local-binding read.~~ **Done** -- new `LoadLocal` /
  `LoadConst` / `MakeFnRef` IR variants cover the three populations
  with discriminated dispatch; new `Lowerer::lower_ident_or_stub` and
  `Lowerer::lower_local_load_or_stub` arms classify by querying
  locals first, then `type_ctx.constants`, then `type_ctx.functions`
  (matching `compile_expr`'s precedence). Unresolved `Ident`s still
  fall through to `Stub` defensively.
- ~~Multi-hop field access on a named local
  (`self.origin.x`, `point.span.start`) re-allocates a scratch struct
  per hop because `FieldLoad` only sees opaque struct-value
  receivers.~~ **Done** -- new `IRInstruction::FieldChain` carries
  `base_name` + `base_type` + `Vec<ResolvedFieldStep>` and delegates
  to `expo-codegen::structs::emit_chain_field_access` (already
  existed but had no IR consumer). `Lowerer::lower_field_access_or_stub`
  now tries `resolve_chain_steps` first; on success it emits one
  `FieldChain` (one GEP chain through the binding's alloca, one final
  `load_maybe_indirect`); on failure it falls back to the recursive
  `FieldLoad` path. Verified on `tests/lang/cross_ref/src/shape.expo`
  -- `self.origin.x` lowers to two chained GEPs through `self`'s
  alloca with one final load, no `tmp_struct` scratch alloca.
- ~~`expo-ir` cannot reach into codegen's
  `Compiler.fn_state.variables` to know which `Ident` names are
  in-scope locals; mirroring the map across every codegen mutation
  site (closures, match arms, generic monomorphization,
  save-and-restore patterns) would be invasive and easy to drift.~~
  **Done** -- new `LocalBindings` trait in
  [`expo-ir::lower::ctx`](../crates/expo-ir/src/lower/ctx.rs)
  provides a single `type_of(&str) -> Option<Type>` query. Codegen's
  `FnState` implements it (forwarding to `variables.get(name).map(|(_, ty, _)| ty.clone())`);
  every `Compiler::lower_ctx*` / `Compiler::lowerer` constructor
  installs `&self.fn_state` as the `LowerCtx.locals` /
  `Lowerer.locals` field. The codegen-side map stays the single
  source of truth -- no parallel mirror, no per-mutation-site
  bookkeeping.

**Outcome.** `Ident` and `Self_` no longer mint `Stub`. Static-chain
GEP optimization is back at the IR level. The IR's expression
vocabulary now covers nine typed instruction variants
(`BinaryOp`, `Call`, `FieldChain`, `FieldLoad`, `LoadConst`,
`LoadLocal`, `MakeFnRef`, `MethodCall`, `UnaryOp`) plus inline
literals -- enough that nearly every operand-shaped expression a
later construct lift will see threads typed end-to-end through the
IR.

### Phase 4f -- Construct lifts

The reframed construct ladder, free to lift any expression
vocabulary the construct actually reaches because Phase 4e is in
place. Each slice is one construct family plus whatever expression
instructions its body / condition / arms require -- expression
vocabulary is no longer a separate phase queueing behind constructs;
it lifts as part of the slice that needs it.

- **Slice 3 -- `if`/`else` + ternary (Done, Wave 21).** Shape 2: two
  body blocks plus a value merge. Introduced the value-merging story
  via `IRInstruction::Phi` (chosen over block arguments to keep the
  merge primitive close to LLVM's native shape; block-argument
  refactor is a candidate for Phase 4g if/when the unified block
  representation makes them ergonomic). New IR types `IRIfElse` and
  `IRTernary` follow the parallel-field convention of `IRUnless` /
  `IRIf`. Ternary fully instructionizes both arms at lowering and
  pre-stages the merge phi in `merge_instructions`; `if`/`else`
  keeps arms as AST `Vec<Statement>` stubs (until statement-level
  lowering in Phase 4g) and `emit_if_else` synthesizes the merge
  phi inline after walking each arm, capturing the actual end
  blocks (nested control flow can move the builder past
  `then_block` / `else_block`). The construct gracefully falls
  through to `Ok(None)` when either arm is statement-only or
  diverges -- mirroring the legacy `compile_if` behavior. The new
  `Phi` instruction's LLVM type is derived from the first
  materialized incoming value rather than the resolved Expo type,
  because `to_llvm_type` rejects `Type::Named` carrying inferred
  `Unknown` type args (common in stdlib `Result` pipelines) while
  the LLVM-side type is always concrete by the time the value lands
  at the merge. `execute_instructions` gained an
  `Option<&block_map>` parameter and a caller-managed `&mut
value_map`. **Outcome.** `compile_if`'s else branch and
  `compile_ternary` are thin shims over `Lowerer::lower_if_else`
  and `Lowerer::lower_ternary` plus the new emit walkers; the IR
  pipeline now covers both shape-2 conditional families end-to-end.
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

### Phase 4g -- Function bodies in IR

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

### Phase 4h -- Stub retirement

Lift the still-`Stub`-producing Expr kinds in dependency order, then
delete the `Stub` variant. After Phase 4g every expression is
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
   Phase 4g dissolves both into a free-floating `Vec<IRBasicBlock>`
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
   at Phase 4g. Forbids: per-construct emission code that
   re-implements walker mechanics.

10. **`LowerCtx` is ambient semantic state; `IRProgram` is an output
    container.** `LowerCtx` carries `&TypeContext`, `&TypeLayouts`,
    `current_package`, `closure_site_path`, `&FnLowerState`, and the
    `&dyn LocalBindings` query oracle for in-scope local bindings.
    `IRProgram` flows through resolvers as an explicit positional
    parameter, not on `LowerCtx`. Forbids: stuffing the IR output
    container into the ambient context bundle. The `LocalBindings`
    seam exists by necessity (variable storage is LLVM-bound and
    cannot move into `expo-ir`); ad-hoc closures into other LLVM-side
    state still stay short, one-shot, and at the codegen call site.

11. **Mangling, identities, and registries live in `expo-ir`.** Once a
    registry exists in `IRProgram`, the matching `function_exists` /
    `is_struct_constructor` / `variable_type` closure into codegen
    retires. Forbids: keeping closures alive after their backing
    registry has moved to `expo-ir`.

12. **One-callable-one-`IRFunction`-with-honest-kind.** Every
    callable symbol in the program -- user, monomorphized,
    intrinsic, runtime extern, thunk, main-entry pair -- is an
    `IRFunction` entry with a typed `IRFunctionKind` that _honestly
    classifies what it is_. Wave 18 closed the original three
    exceptions (thunks, stdlib intrinsic methods,
    `resolve_generic_call`'s registry consult). Wave 19 closed the
    `register_extern` catch-all that misclassified non-generic user
    free fns / methods, the `__expo_user_main` entry pair, and
    per-type debug helpers. Forbids: LLVM-only callable side tables;
    using `register_extern` as a "declare without committing to a
    kind" shortcut. New misclassification would be a regression,
    not a pattern to preserve.

---

## 6. Cross-references

- [`ROADMAP.md`](ROADMAP.md) Phase 6A (self-hosting) -- consumer of
  Phase 7 (`CodeEmitter` protocol).
- [`ROADMAP.md`](ROADMAP.md) Phase 4 Track B (shared data,
  `shared_map`) -- consumer of Phase 8 (ARC for shared types).
- [`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md) --
  original SIL-style design prose, full Wave 1-17 narrative, the
  instruction set vision, the comparison with other compilers.
