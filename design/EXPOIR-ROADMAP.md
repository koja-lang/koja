# ExpoIR Roadmap

Forward-looking roadmap for the ExpoIR refactor. Tracks where the
intermediate-representation work stands today, what slices remain, and
the design invariants that have governed the work so far. The original
SIL-style design prose and the full Wave 1-17 narrative live in
[`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md).

---

## 1. Status snapshot

The instruction-level scaffold has landed. `IRProgram` is the canonical
declaration registry. Fifteen `Lowerer<'a>` lift methods (adding
`lower_statement` / `lower_statements` in Slice 1) cover nineteen
typed `IRInstruction` variants. Six constructs (`unless`, `if`-no-else,
`if`/`else`, ternary, `cond`, `match`) run end-to-end through the full
IR pipeline today; three call families (`Call`, static call, method
call) plus `FieldChain` / `FieldLoad`, `LoadLocal` / `LoadConst` /
`MakeFnRef`, `BinaryOp`, `UnaryOp`, the six pattern primitives
(`PatternTagEq`, `PatternLiteralEq`, `PatternProjectVariantField`,
`PatternUnionPayloadPtr`, `PatternBindFromPtr`, `PatternBinaryMatch`),
and the three statement primitives from Slice 1 (`StoreLocal`,
`StoreField`, `UnionWrap`) reach typed instructions when consumed via
the codegen wrappers' lift-then-fallthrough paths. The IR-level
value-merging primitive (`IRInstruction::Phi`) is in place and
load-bearing for ternary (pre-staged at lowering), `if`/`else`,
`cond`, and `match` (synthesized at emit time when arm-body
trailing-expression values surface). `IRTerminator::Return` joins
`Branch` / `CondBranch` / `Unreachable` so `Statement::Return`
finishes a basic block in IR rather than emitting a codegen-side
`build_return`.

What we do _not_ have yet:

- `IRBasicBlock` is owned per-construct (`IRUnless`, `IRIf`) instead of
  free-floating on `IRFunction`.
- `IRFunction` still carries `expo_ast::ast::Function` AST bodies.
- `compile_function_body` and `compile_method_body` walk AST
  end-to-end (`compile_statement` is now a thin shim that drives the
  IR pipeline; see Phase 4g Slice 1).
- The `IRInstruction::Stub` bridge is alive (single producer:
  `Lowerer::lower_expr_to_operand`); now tolerates void-returning
  expressions so statement-context discards (`print(...)`) round-trip
  through the IR path.
- ~12 constructs still go AST -> LLVM with no IR touchpoint at all
  (the full list is in section 3a).

---

## 2. Phase summary

Condensed from 24 waves of work. The Wave 1-17 prose lives in
[`archive/20260427-EXPOIR.md`](archive/20260427-EXPOIR.md); Waves 18-24
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
- **Phase 4f Slice 4 -- `cond` (done, Wave 22).** The `cond ... end`
  expression lifts to typed IR walked by `execute_instructions`. New
  IR types `IRCondArm` (per-arm check + body slot pair) and `IRCond`
  (N arms plus optional else and a single merge block) generalize
  the shape-2 conditional pattern from `IRIfElse` to N arms. The
  arm chain is encoded directly on each arm's `check_terminator`
  `otherwise` slot pointing at the next arm's `check_block` -- no
  per-arm "next-check" registry needed; the legacy
  `compile_cond`'s `fallthrough_bb` artifact is eliminated since the
  no-else case sends the last arm's `otherwise` straight to merge.
  The merge phi is synthesized inline at emit time (mirroring
  `emit_if_else` -- arms are AST stubs so per-arm trailing-expression
  values aren't visible until Phase 4g), with the all-or-nothing
  value-merge contract preserved from legacy semantics. `IRInstruction::Phi`
  is reused unchanged -- a third construct adopts the merge primitive
  with no modification, validating it as the right shape. See the
  `Phase 4f Slice 4` entry in section 4 for the per-change detail.
- **Phase 4f Slice 5a -- `match` outer scaffold lift (done, Wave 23).**
  The `match` expression's outer scaffold (per-arm cond-branch chain,
  merge phi assembly) converges onto `execute_instructions` +
  `emit_terminator`. New IR types `IRMatch` and `IRMatchArm` mirror
  the `IRCond` shape-2 generalization with two transitional bridges:
  (1) `check_instructions` is empty in 5a and pattern testing remains
  a codegen-side `emit_pattern` call whose i1 result is plumbed into
  the arm's `check_terminator` via the synthetic
  `pattern_result_value` `IRValueId` slot, and (2) pattern bindings
  still flow through a `fn_state.variables` clone/restore around the
  body walk. New `Lowerer::lower_match_expr` wraps the existing free
  `lower_match` (preserved for testing). New `emit_match_unified`
  walker is decomposed into `build_match_block_map`,
  `emit_match_arm_check`, `emit_match_arm_body`, `assemble_match_phi`,
  and `collect_match_incoming` to honor `build.mdc`'s ≤40-LOC
  function budget. The legacy 163-LOC `emit_match` is deleted.
  Three-variant `ArmEmission` (`Value` / `NoValue` / `Terminated`)
  preserves the legacy `pending_arms` vs `needs_branch` vs
  "self-terminated arms are invisible to the value-merge decision"
  contract verbatim. LLVM IR output is byte-for-byte identical
  (same block labels `match_test_*` / `match_body_*` / `match_none`
  / `match_end`, same phi shape with `undef` incoming from
  `match_none`). Both bridges retire in Slice 5b. See the
  `Phase 4f Slice 5` entry in section 4 for the split rationale and
  per-change detail.
- **Phase 4f Slice 5b -- pattern testing + binding lift (done, Wave 24).**
  Both transitional bridges from 5a retire. Six new
  `IRInstruction` variants encode pattern testing as native IR:
  `PatternTagEq` (enum/union tag equality), `PatternLiteralEq`
  (literal compare), `PatternProjectVariantField` (variant-field
  GEP + load + alloca + store, returns the new alloca's pointer),
  `PatternUnionPayloadPtr` (union payload GEP), `PatternBindFromPtr`
  (load + alloca + store + register into `fn_state.variables`,
  side-effect only with no SSA dest), and `PatternBinaryMatch`
  (wraps `compile_binary_pattern` -- multi-block algorithm kept
  whole at the IR seam). New `IRInstruction::dest()` returns
  `Option<IRValueId>` to accommodate the no-dest `PatternBindFromPtr`.
  AND/OR fusion of i1 results reuses existing
  `IRInstruction::BinaryOp { op: BoolAnd | BoolOr }` with
  constant-folding shortcuts (`BoolAnd(true, x) -> x`, etc.) so
  arms like `Some(v) -> ...` (whose `Bind` returns
  `ConstBool(true)`) emit no spurious AND. New
  `Lowerer::lower_pattern_to_instructions` returns a
  `LoweredPattern { instructions, check_result }` -- a single
  ordered stream containing test ops, binds, and any AND/OR
  fusion, plus the [`IROperand`] referencing the final i1.
  Guards lift to `lower_expr_to_operand`-emitted instructions
  appended to the arm's check stream and `BoolAnd`-fused with the
  pattern's i1; no codegen-side guard handling remains.
  `IRMatchArm.guard` and `IRMatchArm.pattern_result_value` retire;
  `IRMatch.patterns` retires. `IRMatch.subject_value` is added so
  pattern primitives reference the subject pointer through
  [`IROperand::Local`] / a single shared `value_map`. The
  codegen-side `emit_pattern`, `emit_bind`, `emit_tag_check`,
  `emit_literal_const`, `emit_binary_pattern` shim, and
  `get_union_payload_ptr` helpers are deleted (~250 LOC). Six new
  executor arms in `control/instructions.rs` perform the LLVM
  builder calls (`emit_pattern_tag_eq`,
  `emit_pattern_literal_eq`, `emit_pattern_project_variant_field`,
  `emit_pattern_union_payload_ptr`, `emit_pattern_bind_from_ptr`,
  `emit_pattern_binary_match`), with shared
  `materialize_ptr_operand` helper for the common pointer-operand
  diagnostic. `compile_pattern` (the public entry from
  `compile_receive_arms`) routes through the same
  `lower_pattern_to_instructions` + `execute_instructions` path,
  so receive arms and match arms share one pattern-emission
  pipeline. Bindings stay in the check block (not the body) so
  guards can reference them; per-arm scoping is enforced by a
  `fn_state.variables` clone/restore wrapping each arm's check +
  body in `emit_match_unified` (and similarly in
  `compile_receive_arms`). The 5b lift moved binding _setup_ into
  IR; _scoping_ stays in codegen because the variables map
  carries LLVM-typed allocas not exposed at the IR surface. Dead
  `lower_match` free function and `ResolvedMatch` struct are
  deleted. See the `Phase 4f Slice 5` entry in section 4 for
  per-change detail.
- **Phase 4f Slice 6 -- loops + tail-flag retirement (done, Wave 25).**
  `while`, `loop`, and `for` converge onto the
  `Lowerer::lower_*` + `emit_*_unified` + `execute_instructions` +
  `emit_terminator` pipeline. Three new IR types --
  [`IRWhile`], [`IRLoop`], [`IRFor`] -- mirror the `IRCond`
  shape-2 generalization: each carries the IR-minted block ids
  for `header` / `body` / `exit`, the `header_instructions`
  (condition lift for `while`), the `header_terminator`
  (`CondBranch { cond, body, exit }`), and the
  `body_terminator` (`Branch(header)` back-edge). `for` keeps the
  iterable AST + binding `Pattern` inline (same precedent as
  `PatternBinaryMatch`) so the multi-block iterator-protocol
  desugar -- `length()` / `get()` / `Option` unwrap / pattern
  bind -- stays whole at the IR seam, broken into ≤40-LOC
  helpers (`build_for_loop_setup`, `resolve_for_impl_methods`,
  `build_for_header_check`, `build_for_element_load`,
  `bind_for_pattern`, `emit_for_back_edge`). Loop bodies remain
  AST `Vec<Statement>` stubs until Phase 4g; loops expose
  `exit_block` so `Statement::Break` continues to resolve through
  the unchanged `loop_exit_stack`. `compile_while` /
  `compile_loop` / `compile_for` collapse to five-line shims
  (`lowerer().lower_*(...)` + `emit_*_unified`); the legacy
  233-LOC `loops.rs` body is deleted.
  In parallel, the ambient `FnLowerState::tail_position` flag
  retires. `IRInstruction::Call` and `IRInstruction::MethodCall`
  gain a `tail: bool` field, populated by a new
  `Lowerer::lower_tail_expr_to_operand` helper threaded through
  the immediately-emitted call instruction (transparent through
  `ExprKind::Group`). Two source-level callers --
  `Statement::Return` in `crates/expo-codegen/src/stmt.rs` and
  the last-statement-implicit-return in
  `compile_function_body` -- swap their `mark_tail` /
  `clear_tail` brackets for a `compile_tail_expr` call that
  routes through the explicit IR-level lowering.
  `compile_body_as_value`'s `save_tail` / `restore_tail`
  save/restore loop is deleted because per-statement tail
  status is now passed explicitly only at the trailing
  expression. The TCO rewrite logic (drop live variables, store
  args into `param_allocas`, branch to `tco_loop`) moves from
  `crates/expo-codegen/src/structs.rs` into a new
  `emit_tail_call_back_edge` helper in
  `crates/expo-codegen/src/control/instructions.rs`'s
  `emit_method_call`, gated on the IR instruction's `tail`
  field plus `FnLowerState::is_self_call`. The
  `tail_position`, `mark_tail`, `clear_tail`, `save_tail`,
  `restore_tail`, and `is_self_tail_call(was_tail)` accessors
  are deleted; only `current_fn` / `is_self_call` survive on
  `FnLowerState` for self-recursion detection. **Outcome.**
  Loops are now indistinguishable from conditionals at the IR
  surface (block ids + terminators + a body block of stubs)
  and tail-call optimization is honest IR data: the `tail`
  field on the call instruction names what previously took an
  ambient walk-state flag and three accessor pairs to express.
- **Phase 4g Slice 1 -- statement vocabulary in IR (done, Wave 26).**
  `Statement::Expr`, `Statement::Assignment` (annotated, single-segment
  or multi-segment lvalue, non-list non-destructure RHS),
  `Statement::CompoundAssign`, `Statement::Return`, and
  `Statement::Break` lift onto the
  `Lowerer::lower_statement` + `execute_instructions` +
  `emit_terminator` pipeline. Three new `IRInstruction` variants land:
  `StoreLocal { name, value, ty, is_decl, ownership }` covers
  alloca+store for fresh let-bindings (with `Ownership` -- itself
  moved into `expo-ir` -- pre-classified at lowering time) and
  reassignments to existing slots; `StoreField { base_name,
  base_type, steps, value, ty }` walks the `ResolvedFieldStep`
  chain shared with `IRInstruction::FieldChain` to assign multi-
  segment lvalues; `UnionWrap { dest, value, source_ty,
  target_union }` lifts the recorded `Coercion::UnionWiden` so the
  store's right-hand operand is union-wrapped at the IR level
  rather than at the codegen seam. New
  `IRTerminator::Return { value: Option<IROperand>, drop_skip: Option<String> }`
  replaces the codegen-side `build_return` pair; the executor
  performs the `drop_live_variables` walk before emitting the LLVM
  return and short-circuits to a no-op when the block was already
  terminated by a TCO back-edge. `IRInstruction::dest()` returns
  `None` for `StoreField` / `StoreLocal` / `PatternBindFromPtr` and
  `Some(*dest)` for `UnionWrap`. The legacy ambient
  `loop_exit_stack: Vec<BasicBlock>` on `FnState` retires in favor
  of paired stacks: `FnLowerState.loop_exit: Vec<IRBlockId>`
  (semantic, used by Slice 1's `lower_break_stmt` to mint the
  `Branch(exit_id)` terminator) and
  `FnState.loop_exit_blocks: Vec<(IRBlockId, BasicBlock<'ctx>)>`
  (LLVM-bound, seeded into the shim's `block_map` so `Break`
  terminators resolve). Loop emit walkers' new `enter_loop` /
  `leave_loop` helpers push / pop both stacks in lockstep.
  Pure-semantic helpers (`ownership_for_expr`, `infer_type_from_expr`
  and its supporting `infer_static_method_return_type` /
  `infer_instance_method_return_type` / `infer_receiver_type`) move
  from `expo-codegen::stmt` into
  `expo-ir::lower::{ownership, inference}`; codegen retains a thin
  `infer_type_from_expr_codegen` wrapper that bridges
  `Compiler.fn_state.variables` to the IR helper through the
  existing `LocalBindings`-style closure pattern.
  `compile_statement` is now a thin shim: it pushes annotation-
  derived `type_subst` entries (so `IRInstruction::Stub`'s
  deferred `compile_expr` sees `T = Int` for
  `list: List<Int> = List.new()`-style sites), seeds `block_map`
  from `loop_exit_blocks`, then dispatches via `lower_statement`
  -> `execute_instructions` -> `emit_terminator`. Three
  transitional AST shapes still fork to a slimmed legacy
  `compile_assignment`: (1) RHS is `ExprKind::List` literal --
  the protocol-driven `from_list` coercion (e.g.
  `Set<Int> = [1, 2, 3]`) is deferred to Slice 3 because the
  on-demand `monomorphize_impl_method` it triggers is LLVM-bound
  and the alternatives (mangled-symbol string lookup in the
  executor; a `Monomorphizer` callback into codegen during
  lowering) both push against Phase 4g's end-state of "codegen
  consumes a closed `IRProgram`"; (2) destructuring `Pattern`
  targets, which the Lowerer rejects today (preserving the
  existing diagnostic surface); (3) unannotated assignments,
  where the legacy `compile_expr` computes the actual evaluated
  type at codegen time (`addrs = match Socket.resolve(...) ...`
  settling to `List<TCPAddr>`) but the IR Lowerer can only
  consult `expr.resolved_type` (often `None` / `Type::Unknown`
  for compound RHS shapes). All three retire when the
  elaboration pass arrives in Slice 3. The legacy
  `compile_compound_assign` / `apply_compound_op` /
  `compile_field_assignment` / `field_ptr` / local `infer_*` /
  `ownership_for_expr` helpers are deleted (~250 LOC);
  `apply_coercion` survives only because the legacy list-literal
  fork still calls it. New executor arms (`emit_store_local`,
  `emit_store_field`, `walk_field_chain`) and the new `Return`
  terminator arm round out the codegen side. The `Stub` arm is
  relaxed to tolerate `compile_expr` returning `Ok(None)` for
  void-returning calls (statement-context discards like
  `print(...)`); a downstream reference to the absent `dest`
  becomes a clear `materialize_operand` lookup miss rather than
  a strict `ok_or` panic. **Outcome.** Five of six `Statement`
  variants flow through the IR pipeline; `Statement::Assignment`'s
  three unsupported shapes are tagged for Slice 3 retirement.
  The IR surface gained two structural primitives (`StoreLocal` /
  `StoreField`) and one new terminator (`Return`) that Slice 2
  (per-construct body lift) and Slice 3 (function-body lift)
  build on directly.
- **Sidebar -- Pattern-CFG gating for nested-enum-literal payloads
  (done, Wave 27).** Resolves the GAPS "Nested enum pattern matching
  with literal payloads" segfault that the Slice 5b match rework had
  predicted but not actually delivered. The 5b lift translated the
  flat `BoolAnd`-fused pattern stream verbatim into IR; payload
  projections (`PatternProjectVariantField`,
  `PatternUnionPayloadPtr`) still ran unconditionally, so a `None`
  matched against an arm shaped `Some(<literal-payload>)` dereffed
  uninitialized payload memory and segfaulted. Pattern lowering
  becomes a CFG builder: `IRMatchArm.{check_block,
  check_instructions, check_terminator}` collapse into a single
  `check_blocks: Vec<IRBasicBlock>`; today's flat case is
  `len() == 1` and constructor patterns produce `len() >= 2` -- the
  outer tag check terminates the open block with `CondBranch(tag,
  payload_block, failure_target)`, payload projections move into
  the fresh `payload_block`, and the same `failure_target` (the
  next arm's entry / `fallthrough_block`) threads through every
  nested gate. The new
  `Lowerer::lower_pattern_into_arm(resolved, subject_ptr,
  failure_target, blocks)` is the per-arm imperative driver; flat
  patterns (`LiteralEq`, `EnumUnit`, `PatternBinaryMatch`) keep
  emitting into the open block and returning their i1, while
  constructor + `Or` patterns gate via control flow and return
  `IROperand::ConstBool(true)`. Inter-field gating
  (`gate_intermediate_field`) avoids running later field
  projections when an earlier element's literal compare already
  failed. The codegen walker simplifies to `for blk in
  &arm.check_blocks { position; execute_instructions; emit_terminator
  }`. The single-pattern path (`compile_pattern` for `receive` /
  `expr matches Pattern`) keeps the legacy flat
  `LoweredPattern { instructions, check_result }` shape and
  retains the same payload-deref vulnerability for those surfaces;
  lifting them to the gated CFG builder is tracked separately.
  Regression locked in via
  [tests/lang/types/nested_enum_pattern_literal.expo](../tests/lang/types/nested_enum_pattern_literal.expo).

---

## 3. Ground-truth state

What's actually in the IR today, so future-you doesn't need to re-audit
to plan a slice.

### 3a. Lift status by construct

| Construct                   | Status                      | Notes                                                                                                                                                                                                                                                                                                                                                                                        |
| --------------------------- | --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `unless`                    | Full IR pipeline            | `Lowerer::lower_unless` -> `IRUnless` -> `emit_unless` + `execute_instructions`                                                                                                                                                                                                                                                                                                              |
| `if` (no else)              | Full IR pipeline            | `Lowerer::lower_if_no_else` -> `IRIf` -> `emit_if` + `execute_instructions`                                                                                                                                                                                                                                                                                                                  |
| `if`/`else` (with else)     | Full IR pipeline            | `Lowerer::lower_if_else` -> `IRIfElse` -> `emit_if_else` (merge phi synthesized inline; arms remain AST stubs until Phase 4g)                                                                                                                                                                                                                                                                |
| `ternary`                   | Full IR pipeline            | `Lowerer::lower_ternary` -> `IRTernary` -> `emit_ternary` + `execute_instructions` (merge phi pre-staged in `merge_instructions`)                                                                                                                                                                                                                                                            |
| `Call` / `static_call`      | Instruction-only            | `Lowerer::lower_call_or_stub(..., tail)` / `lower_static_call_or_stub(..., tail)` -> `IRInstruction::Call { tail, .. }` (tail flag carried for symmetry with `MethodCall`; only `MethodCall` currently triggers a TCO back-edge)                                                                                                                                                             |
| `MethodCall`                | Instruction-only            | `Lowerer::lower_method_call_or_stub(..., tail)` -> `IRInstruction::MethodCall { tail, .. }`. The `tail` field is set via `Lowerer::lower_tail_expr_to_operand` from `Statement::Return` and the last-statement-implicit-return; the codegen executor rewrites self-recursive `tail = true` calls to a `tco_loop` back-edge.                                                                  |
| `FieldAccess` (chains)      | Instruction-only            | `Lowerer::lower_field_access_or_stub` -> `IRInstruction::FieldChain` (rooted at named local; delegates to `emit_chain_field_access`)                                                                                                                                                                                                                                                         |
| `FieldAccess` (value recv)  | Instruction-only            | `Lowerer::lower_field_access_or_stub` -> `IRInstruction::FieldLoad` (fallback for non-binding receivers)                                                                                                                                                                                                                                                                                     |
| `Ident` (locals)            | Instruction-only            | `Lowerer::lower_ident_or_stub` -> `IRInstruction::LoadLocal`                                                                                                                                                                                                                                                                                                                                 |
| `Ident` (constants)         | Instruction-only            | `Lowerer::lower_ident_or_stub` -> `IRInstruction::LoadConst`                                                                                                                                                                                                                                                                                                                                 |
| `Ident` (function-as-value) | Instruction-only            | `Lowerer::lower_ident_or_stub` -> `IRInstruction::MakeFnRef`                                                                                                                                                                                                                                                                                                                                 |
| `Self_`                     | Instruction-only            | `Lowerer::lower_local_load_or_stub` -> `IRInstruction::LoadLocal { name: "self" }`                                                                                                                                                                                                                                                                                                           |
| Binary op (most)            | Instruction-only            | `Lowerer::lower_binary_op_or_stub` -> `IRInstruction::BinaryOp`                                                                                                                                                                                                                                                                                                                              |
| Unary op                    | Instruction-only            | `Lowerer::lower_unary_op_or_stub` -> `IRInstruction::UnaryOp`                                                                                                                                                                                                                                                                                                                                |
| Bool/Int/Float literals     | Inline operand              | `IROperand::ConstBool` / `ConstInt` / `ConstFloat`                                                                                                                                                                                                                                                                                                                                           |
| `match` (full pipeline)     | Full IR pipeline            | `Lowerer::lower_match_expr` -> `IRMatch` -> `emit_match_unified` (per-arm cond-branches via `emit_terminator`; merge phi synthesized inline). Pattern testing + binding fully lifted to `PatternTagEq` / `PatternLiteralEq` / `PatternProjectVariantField` / `PatternUnionPayloadPtr` / `PatternBindFromPtr` / `PatternBinaryMatch` instructions; guards lifted via `lower_expr_to_operand`. Per-arm checks are CFG sub-graphs (`IRMatchArm.check_blocks: Vec<IRBasicBlock>`) with constructor patterns gated by `CondBranch(tag, payload_block, failure_target)` so payload-load primitives never execute when the enclosing tag check failed (Wave 27). |
| `cond`                      | Full IR pipeline            | `Lowerer::lower_cond` -> `IRCond` -> `emit_cond` (merge phi synthesized inline; arms remain AST stubs until Phase 4g)                                                                                                                                                                                                                                                                        |
| `while`                     | Full IR pipeline            | `Lowerer::lower_while` -> `IRWhile` -> `emit_while_unified` (header `IRInstruction`s + `CondBranch` terminator + body back-edge `Branch`; body remains AST stub until Phase 4g)                                                                                                                                                                                                              |
| `loop`                      | Full IR pipeline            | `Lowerer::lower_loop` -> `IRLoop` -> `emit_loop_unified` (single body block + `Branch` back-edge; body remains AST stub until Phase 4g)                                                                                                                                                                                                                                                      |
| `for`                       | Full IR pipeline            | `Lowerer::lower_for` -> `IRFor` -> `emit_for_unified` (header / body / exit blocks + idx/iterable allocas in shared `value_map`; iterator-protocol desugar -- `length()` / `get()` / `Option` unwrap / pattern bind -- kept whole at the IR seam, broken into ≤40-LOC helpers)                                                                                                               |
| `break` / `return`          | Full IR pipeline            | `Statement::Break` -> `IRTerminator::Branch(loop_exit_id)` (resolved via `FnLowerState.loop_exit` at lowering, paired `FnState.loop_exit_blocks` at emit); `Statement::Return` -> `IRTerminator::Return { value, drop_skip }` with executor-side `drop_live_variables` walk and TCO short-circuit                                                                                            |
| `assignment` (annotated)    | Full IR pipeline            | `Lowerer::lower_assignment_stmt` -> `IRInstruction::StoreLocal` / `StoreField` (preceded by optional `IRInstruction::UnionWrap` for recorded `Coercion::UnionWiden`); `compile_statement` shim pushes annotation-derived `type_subst` around lowering + execution so deferred `Stub` evaluation sees the entries                                                                            |
| `assignment` (other shapes) | Legacy fork                 | Three transitional shapes route through legacy `compile_assignment`: `ExprKind::List` RHS (protocol `from_list` coercion -- Slice 3), destructuring `Pattern` target (Lowerer rejects), and unannotated RHS (codegen-time inference required for compound shapes like `match` / `cond` value)                                                                                                |
| `compound_assign`           | Full IR pipeline            | `Lowerer::lower_compound_assign_stmt` -> load-current + `IRInstruction::BinaryOp` + `StoreLocal` / `StoreField` (single ordered instruction stream, no extra terminator)                                                                                                                                                                                                                    |
| `field_assignment`          | Full IR pipeline            | Multi-segment lvalue assignments lower to `IRInstruction::StoreField` (executor walks the resolved chain via the new `walk_field_chain` helper that ports the legacy `field_ptr`)                                                                                                                                                                                                           |
| Binary pattern              | Instruction-only            | `PatternBinaryMatch` wraps `compile_binary_pattern` whole at IR seam (multi-block algorithm; no further decomposition planned)                                                                                                                                                                                                                                                               |
| Struct construction         | AST -> LLVM                 | Phase 4h                                                                                                                                                                                                                                                                                                                                                                                     |
| Enum construction           | AST -> LLVM                 | Phase 4h                                                                                                                                                                                                                                                                                                                                                                                     |
| Closure construction        | AST -> LLVM                 | Phase 4h (`partial_apply` shape)                                                                                                                                                                                                                                                                                                                                                             |
| String literal              | AST -> LLVM                 | Phase 4h                                                                                                                                                                                                                                                                                                                                                                                     |
| String interpolation/concat | AST -> LLVM                 | Phase 4h (`compile_concat`, `compile_string_concat`, `compile_binary_concat`)                                                                                                                                                                                                                                                                                                                |
| `EnumStructEqual`           | AST -> LLVM                 | Phase 4h (multi-block per-variant equality)                                                                                                                                                                                                                                                                                                                                                  |
| `spawn` / `receive`         | AST -> LLVM (decision lift) | Phase 4h (process resolvers exist; instruction lift pending)                                                                                                                                                                                                                                                                                                                                 |
| `print*` / `panic`          | AST -> LLVM                 | Phase 4h (builtin-call instruction lift)                                                                                                                                                                                                                                                                                                                                                     |
| Generic-fn / struct ctor    | AST -> LLVM                 | Phase 4h (call-lift fallthrough cases)                                                                                                                                                                                                                                                                                                                                                       |
| `union_wrap`                | AST -> LLVM (decision lift) | Phase 4h                                                                                                                                                                                                                                                                                                                                                                                     |

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
- `Lowerer::lower_statement` / `lower_statements` in
  [`expo-ir::lower::statements`](../crates/expo-ir/src/lower/statements.rs)
  -- the single statement-lowering seam introduced by Phase 4g
  Slice 1. Returns `(Vec<IRInstruction>, Option<IRTerminator>)`;
  the terminator is `Some` only for `Return` / `Break`. Driven
  today by the `compile_statement` shim and slated to drive the
  per-construct body lift in Slice 2 and the function-body lift in
  Slice 3.
- `Lowerer::lower_pattern_into_arm` in
  [`expo-ir::lower::patterns`](../crates/expo-ir/src/lower/patterns.rs)
  -- the per-arm pattern CFG builder. Imperatively writes blocks
  into the arm's `check_blocks: Vec<IRBasicBlock>` buffer, gating
  every constructor pattern (`EnumStruct` / `EnumTuple` /
  `UnionMember`) with `CondBranch(tag, payload_block,
  failure_target)` so payload-load primitives never run when the
  enclosing tag check failed. Threads `failure_target` (the next
  arm's entry / `fallthrough_block`) unchanged into nested
  recursion, including inter-field gating (`gate_intermediate_field`)
  and `Or`-pattern alternatives (`lower_or_into_arm`). Returns
  `IROperand::ConstBool(true)` whenever control-flow gating
  encoded the success/failure decision; flat patterns
  (`LiteralEq`, `EnumUnit`, `PatternBinaryMatch`) keep returning
  their data-flow i1. Distinct from the legacy flat
  `Lowerer::lower_pattern_to_instructions` /
  `lower_resolved_pattern` shape that `compile_pattern` (the
  `receive` / `expr matches Pattern` entry point) still uses;
  lifting the single-pattern path to the gated CFG builder is the
  remaining followup tracked separately.
- [`expo-ir::FnLowerState.loop_exit`](../crates/expo-ir/src/fn_state.rs)
  paired with
  [`FnState.loop_exit_blocks`](../crates/expo-codegen/src/compiler.rs)
  -- the dual stack the loop emit walkers maintain via `enter_loop` /
  `leave_loop`. The IR-side `Vec<IRBlockId>` lets `Lowerer::lower_break_stmt`
  resolve the target block id at lowering time; the LLVM-bound
  `Vec<(IRBlockId, BasicBlock<'ctx>)>` twin lets the
  `compile_statement` shim seed `block_map` so the `Branch(exit_id)`
  terminator resolves at emit time. Slice 2 retires the codegen
  twin once loop bodies are block-shaped (back-edge / break
  terminators pre-resolve at lowering).

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
- **Slice 4 -- `cond` (Done, Wave 22).** N-arm chain of `CondBranch`s.
  Tested the scaffold scaling beyond fixed-N constructs without any
  new IR primitive: `IRCondArm` + `IRCond` generalize `IRIfElse`'s
  shape-2 pattern by encoding the arm chain on each arm's
  `check_terminator` `otherwise` slot pointing at the next arm's
  `check_block`. The legacy `compile_cond`'s `fallthrough_bb`
  artifact is eliminated -- the no-else case sends the last arm's
  `otherwise` straight to merge. Like `IRIfElse`, arm bodies remain
  AST `Vec<Statement>` stubs (until Phase 4g) and the merge phi is
  synthesized inline at emit time; the all-or-nothing value-merge
  contract from legacy `compile_cond` is preserved (every arm + else
  must produce a matching-LLVM-typed value, or the construct returns
  `Ok(None)` for no-production / `Err` for partial-production --
  the latter is defensive since typecheck enforces consistency at
  the source level via `expo-typecheck::expr::infer_expr`).
  `IRInstruction::Phi` is reused unchanged. Known semantic wart
  preserved as-is: divergent arms (early `return` / `panic`) mixed
  with value-producing arms hit the partial-production error path
  because the divergent arm doesn't push to the incoming list while
  still counting toward `expected_sources`. Out-of-scope for this
  slice; addressable later. **Outcome.** `compile_cond` is a thin
  shim over `Lowerer::lower_cond` + `emit_cond`; the IR pipeline now
  covers the third value-producing conditional family.
- **Slice 5 -- `match` unification.** Split into two waves to keep
  the cumulative LOC reviewable and isolate the two distinct risk
  surfaces (outer scaffold convergence vs. pattern-test lift).
  - **Slice 5a -- outer scaffold lift (Done, Wave 23).** Existing
    `lower_match` / `emit_match` parallel pipeline converges onto
    `execute_instructions` + `emit_terminator`. New `IRMatch` /
    `IRMatchArm` resolved types describe the per-arm cond-branch
    chain with the same shape as `IRCond` (`check_block` ->
    `body_block` per arm, `otherwise` slot chained to the next
    arm's `check_block`). New `Lowerer::lower_match_expr` wraps the
    existing free `lower_match` (preserved for testing) with id
    minting. New `emit_match_unified` walker (broken into
    `build_match_block_map` / `emit_match_arm_check` /
    `emit_match_arm_body` / `assemble_match_phi` /
    `collect_match_incoming` for `build.mdc` ≤40-LOC compliance)
    drives the per-arm cond-branches through the shared
    `emit_terminator`. Pattern testing remains a codegen-side
    `emit_pattern` call in slice 5a -- its i1 result is bridged
    into each arm's `check_terminator` via the synthetic
    `pattern_result_value` slot that the walker stuffs into the
    arm's value map after `emit_pattern` returns. Pattern binding
    scope (`fn_state.variables` clone/restore) also remains in the
    walker -- both bridges retire in slice 5b. Three-variant
    `ArmEmission` enum (`Value` / `NoValue` / `Terminated`) tracks
    the legacy `pending_arms` vs `needs_branch` vs "self-terminated
    is invisible to the value-merge decision" contract verbatim.
    LLVM IR output is byte-for-byte identical to the legacy
    `emit_match` (same block labels, same phi shape with `undef`
    incoming from `match_none`). **Outcome.** `compile_match` is a
    thin shim over `Lowerer::lower_match_expr` +
    `emit_match_unified`; the legacy 163-LOC `emit_match` is
    deleted. The outer scaffold is now construct-agnostic from
    emission's perspective.
  - **Slice 5b -- pattern testing + binding lift (Done, Wave 24).**
    Six new `IRInstruction` variants encode pattern testing as
    native IR: `PatternTagEq`, `PatternLiteralEq`,
    `PatternProjectVariantField` (variant-field GEP + load + alloca - store, returns the new alloca's pointer for sub-pattern
    recursion or binding), `PatternUnionPayloadPtr`,
    `PatternBindFromPtr` (load + alloca + store + register into
    `fn_state.variables`, no SSA dest), and `PatternBinaryMatch`
    (wraps the multi-block `compile_binary_pattern` whole at the
    IR seam). `IRInstruction::dest()` returns `Option<IRValueId>`
    to accommodate the no-dest `PatternBindFromPtr`. AND/OR fusion
    of i1 results reuses `IRInstruction::BinaryOp { op: BoolAnd
| BoolOr }` with constant-folding shortcuts (`BoolAnd(true,
x) -> x`, etc.) so arms whose `Bind` returns `ConstBool(true)`
    emit no spurious AND. New
    `Lowerer::lower_pattern_to_instructions` returns a
    `LoweredPattern { instructions, check_result }` -- a single
    ordered stream containing test ops, binds, and AND/OR fusion,
    plus the [`IROperand`] referencing the final i1. Guards lift
    via `lower_expr_to_operand` appended to the arm's check stream
    and `BoolAnd`-fused with the pattern's i1 -- no codegen-side
    guard handling remains. `IRMatchArm.guard`,
    `IRMatchArm.pattern_result_value`, and `IRMatch.patterns`
    retire; `IRMatch.subject_value` is added so pattern
    primitives reference the subject pointer through
    [`IROperand::Local`] / a single shared `value_map` threaded
    across all arms. The codegen-side `emit_pattern`, `emit_bind`,
    `emit_tag_check`, `emit_literal_const`, `emit_binary_pattern`
    shim, and `get_union_payload_ptr` helpers are deleted (~250
    LOC). Six executor arms in `control/instructions.rs` perform
    the LLVM builder calls (`emit_pattern_tag_eq`,
    `emit_pattern_literal_eq`,
    `emit_pattern_project_variant_field`,
    `emit_pattern_union_payload_ptr`,
    `emit_pattern_bind_from_ptr`, `emit_pattern_binary_match`),
    sharing `materialize_ptr_operand` for the pointer-operand
    diagnostic. `compile_pattern` (the public entry from
    `compile_receive_arms`) routes through the same
    `lower_pattern_to_instructions` + `execute_instructions` path,
    so receive arms and match arms share one pattern-emission
    pipeline. Bindings stay in the check block (not the body) so
    Expo guards (`Some(v) when v > 0`) can reference them; per-arm
    scoping is enforced by a `fn_state.variables` clone/restore
    wrapping each arm's check + body in `emit_match_unified` and
    `compile_receive_arms`. The 5b lift moved binding _setup_
    into IR; _scoping_ stays in codegen because the variables map
    carries LLVM-typed allocas not exposed at the IR surface. Dead
    `lower_match` free function and `ResolvedMatch` struct are
    deleted. **Outcome.** `emit_pattern` and the 5a synthetic
    bridges are gone; `match` and `receive` arms drive a single
    IR-encoded pattern-emission pipeline through the shared
    `execute_instructions` walker. The IR surface now describes
    what gets tested, what gets bound, and how results fuse --
    the only codegen-side concession is per-arm variable scoping
    around LLVM-typed allocas.

    The flat-stream shape preserved by this slice (a single
    `Vec<IRInstruction>` per arm with `BoolAnd` fusion across all
    sub-checks) carried over the legacy `emit_pattern`'s
    payload-deref-before-tag-gate hazard verbatim. Wave 27 splits
    the per-arm check into a CFG (`IRMatchArm.check_blocks:
    Vec<IRBasicBlock>`) so constructor patterns gate via
    `CondBranch(tag, payload_block, failure_target)` and payload
    loads only run on the success edge. See the Wave 27 sidebar
    entry in section 2 for the full follow-up.
- **Slice 6 -- loops + tail-flag retirement (Done, Wave 25).**
  `while`, `loop`, and `for` lift onto the `Lowerer::lower_*` +
  `emit_*_unified` pipeline (parallel to Slices 3-5): three new
  IR types -- `IRWhile`, `IRLoop`, `IRFor` -- carry IR-minted
  `header_block` / `body_block` / `exit_block` ids, the
  `header_instructions` lift, `header_terminator`
  (`CondBranch { cond, body, exit }`), and `body_terminator`
  (`Branch(header)` back-edge). Bodies remain AST stubs until
  Phase 4g; the loops expose `exit_block` so `Statement::Break`
  resolves through the unchanged `loop_exit_stack`. `for` keeps
  its iterable AST + binding `Pattern` inline, mirroring the
  `PatternBinaryMatch` precedent: the multi-block iterator
  desugar (`length()` / `get()` / `Option` unwrap / pattern
  bind) stays whole at the IR seam, broken into ≤40-LOC helpers.
  `compile_while` / `compile_loop` / `compile_for` collapse to
  five-line shims; the 233-LOC legacy `loops.rs` body is
  deleted.
  In parallel, the ambient `FnLowerState::tail_position` flag
  retires. `IRInstruction::Call` and `IRInstruction::MethodCall`
  gain a `tail: bool` field; new
  `Lowerer::lower_tail_expr_to_operand` threads `tail = true`
  into the immediately-emitted call (transparent through
  `Group`). `Statement::Return` and the last-statement-implicit-
  return swap their `mark_tail` / `clear_tail` brackets for the
  explicit lowering via a new `compile_tail_expr` helper;
  `compile_body_as_value`'s `save_tail` / `restore_tail`
  save/restore loop is deleted. The TCO rewrite (drop live
  variables, store args into `param_allocas`, branch to
  `tco_loop`) moves from `structs.rs` into
  `emit_tail_call_back_edge` in `control/instructions.rs`'s
  `emit_method_call`, gated on `IRInstruction::MethodCall.tail`
  and `FnLowerState::is_self_call`. The
  `tail_position` / `mark_tail` / `clear_tail` / `save_tail` /
  `restore_tail` / `is_self_tail_call(was_tail)` accessors are
  deleted; only `current_fn` / `is_self_call` survive for
  self-recursion detection. **Outcome.** Loops are
  indistinguishable from conditionals at the IR surface; TCO is
  honest IR data on the call instruction. The `tco_loop` block
  - `param_allocas` LLVM-side scaffolding in `compile_method_body`
    is unchanged -- it's the rewrite target, orthogonal to the IR
    flag. LLVM IR for `Counter.count_down` (the
    `tests/lang/functions/tail_call.expo` regression) confirms
    the self-recursive call is rewritten to `br label %tco_loop`
    byte-for-byte as before.

### Phase 4g -- Function bodies in IR

The structural cut. `IRFunction` stops carrying
`expo_ast::ast::Function` bodies and starts carrying
`Vec<IRBasicBlock>`; `compile_statement`, `compile_function_body`,
and `compile_method_body` lift to IR; the nine per-construct IR
types (`IRUnless`, `IRIf`, `IRIfElse`, `IRTernary`, `IRCond`,
`IRMatch`, `IRWhile`, `IRLoop`, `IRFor`) dissolve into free-floating
basic blocks on `IRFunction`; per-construct emit walkers
(`emit_unless`, `emit_if`, `emit_if_else`, `emit_cond`,
`emit_ternary`, `emit_match_unified`, `emit_while_unified`,
`emit_loop_unified`, `emit_for_unified`) retire in favor of one
block walker. `Compiler` becomes a pure consumer of `IRProgram`
with no per-module ambient state.

This is the architectural moment the original SIL-style design
called for and the moment "the lowering / emission split" finally
lands. Slice 7 of the original construct ladder is folded in
because it is the same structural change: there is no half-state
where `IRFunction` carries both an AST body and a `Vec<IRBasicBlock>`.
The dissolution of the nine per-construct types follows mature
compiler precedent (Swift SIL, Rust MIR, GCC GIMPLE, LLVM IR all
keep high-level _operations_ as instructions while dissolving
high-level _control flow_ to CFG); the SIL-style operations that
matter at the IR level have already been lifted to instructions
(`PatternTagEq`, `PatternProjectVariantField`,
`PatternUnionPayloadPtr`, `PatternBinaryMatch`, `Phi`), so the
wrapper types carry no construct-identity that a backend or
optimizer needs typed access to.

Sequenced as four slices to keep cumulative LOC reviewable and
isolate distinct risk surfaces (statement vocabulary vs body lift
vs structural cut vs ambient-state retirement).

- **Slice 1 -- Statement vocabulary in IR (Done, Wave 26).**
  Five `Statement` variants flow through the new
  `Lowerer::lower_statement` / `lower_statements` ->
  `execute_instructions` -> `emit_terminator` pipeline:
  `Statement::Expr` (lowered via `lower_expr_to_operand`,
  discarding the operand and tolerating void-returning
  `compile_expr` results in the relaxed `Stub` arm),
  `Statement::CompoundAssign` (load-binop-store reusing
  `IRInstruction::BinaryOp` plus the new `StoreLocal` /
  `StoreField`), `Statement::Return` (new
  `IRTerminator::Return { value: Option<IROperand>, drop_skip: Option<String> }`
  with executor-side `drop_live_variables` walk and TCO short-
  circuit), `Statement::Break` (bare `IRTerminator::Branch(exit_id)`
  resolved through `FnLowerState.loop_exit` at lowering and
  `FnState.loop_exit_blocks` at emit), and most
  `Statement::Assignment` shapes -- specifically annotated single-
  or multi-segment lvalues with non-list non-destructure RHS,
  emitting `IRInstruction::StoreLocal { name, value, ty, is_decl, ownership }`
  / `StoreField { base_name, base_type, steps, value, ty }`
  preceded by an optional `IRInstruction::UnionWrap { dest, value, source_ty, target_union }`
  for recorded `Coercion::UnionWiden`. `Ownership` itself moves
  from `expo-codegen::drop` into `expo-ir::ownership` so the
  enum is reachable from the LLVM-free Lowerer.
  `loop_exit_stack: Vec<BasicBlock>` retires from `FnState` in
  favor of paired stacks: `FnLowerState.loop_exit: Vec<IRBlockId>`
  (semantic) and `FnState.loop_exit_blocks: Vec<(IRBlockId, BasicBlock<'ctx>)>`
  (LLVM-bound, retained as the shim's `block_map` seed until
  Slice 2 makes back-edge / break terminators block-pre-resolved
  at lowering). Loop emit walkers' new `enter_loop` /
  `leave_loop` helpers push / pop both stacks in lockstep. The
  pure-semantic helpers `ownership_for_expr` and
  `infer_type_from_expr` (with its supporting
  `infer_static_method_return_type` /
  `infer_instance_method_return_type` /
  `infer_receiver_type`) move from `expo-codegen::stmt` into
  `expo-ir::lower::{ownership, inference}`; codegen retains a
  thin `infer_type_from_expr_codegen` wrapper that bridges
  `Compiler.fn_state.variables` to the IR helper through the
  existing `LocalBindings`-style closure pattern.
  `compile_statement` becomes a thin shim that pushes annotation-
  derived `type_subst` entries before lowering / executing the
  statement (so `IRInstruction::Stub`'s deferred `compile_expr`
  call sees `T = Int` for `list: List<Int> = List.new()`-style
  sites), seeds the executor's `block_map` from
  `loop_exit_blocks`, and dispatches via `lower_statement` ->
  `execute_instructions` -> `emit_terminator`.
  **Three transitional AST shapes still fork to a slimmed legacy
  `compile_assignment`** and retire when the elaboration pass
  arrives in Slice 3:
  1. RHS is `ExprKind::List` literal -- the protocol-driven
     `from_list` coercion (e.g. `Set<Int> = [1, 2, 3]`) is
     deferred because the on-demand `monomorphize_impl_method` it
     triggers is LLVM-bound. The two architectural alternatives
     considered (mangled-symbol string lookup in the executor;
     a `Monomorphizer` callback into codegen during lowering)
     both push against Phase 4g's end-state of "codegen
     consumes a closed `IRProgram`"; the right fix is a
     pre-codegen elaboration pass that pre-monomorphizes the
     `from_list` impl into `IRProgram` so the Lowerer can emit a
     canonical `IRInstruction::MethodCall`. Lands alongside the
     function-body lift in Slice 3.
  2. Destructuring `Pattern` target -- the Lowerer rejects them
     today (the legacy path also rejects them); the fork
     preserves the pre-existing diagnostic surface until
     destructuring patterns get proper IR support.
  3. Unannotated assignment -- the legacy `compile_expr`
     computes the actual evaluated type at codegen time
     (e.g. `addrs = match Socket.resolve(...) ...` settling to
     `List<TCPAddr>`) but the IR Lowerer can only consult
     `expr.resolved_type`, which is `None` / `Type::Unknown`
     for many compound RHS shapes. Annotated assignments are
     safe -- the annotation pins the binding's type
     independent of RHS inference. Slice 3's elaboration pass
     gives lowering the same codegen-time type the legacy
     path computes, retiring this fork too.
  Codegen-side helpers deleted: `compile_compound_assign` /
  `apply_compound_op` / `compile_field_assignment` / `field_ptr`
  plus the local `infer_*` / `ownership_for_expr` /
  `is_concat_expr` duplicates and the
  `structs::infer_static_method_return_type` wrapper (~250 LOC
  net). New executor arms (`emit_store_local`, `emit_store_field`,
  `walk_field_chain`) and the new `Return` terminator arm round
  out the codegen side. `IRInstruction::dest()` returns `None`
  for `StoreField` / `StoreLocal` / `PatternBindFromPtr` and
  `Some(*dest)` for `UnionWrap`. The `Stub` arm now tolerates
  `compile_expr` returning `Ok(None)` (statement-context discards
  like `print(...)`); a downstream reference to the absent
  `dest` becomes a clear `materialize_operand` lookup miss
  rather than a strict `ok_or` panic.
  Per-binding `Drop` instructions (the original Slice 1 plan's
  `IRInstruction::DropLiveVariables` synthetic) **moved out of
  this slice** -- the `drop_live_variables` walk lives in the
  `Return` terminator's executor arm today, and the per-binding
  drop lift is Phase 6 (ownership) work where it belongs.
  **Outcome.** Five of six `Statement` variants flow through the
  IR pipeline; the three transitional `Assignment` forks are
  scoped for Slice 3 retirement. The IR surface gains two
  structural primitives (`StoreLocal` / `StoreField`), one
  coercion primitive (`UnionWrap`), and one terminator
  (`Return`) that Slice 2 (per-construct body lift) and Slice 3
  (function-body lift) build on directly. Validation: 25/25
  lang tests, 246/246 stdlib tests, zero clippy warnings.

- **Slice 2 -- Per-construct body block lift.** Each per-construct
  IR type's `body_stmts: Vec<Statement>` (and `IRCond.else_stmts`,
  `IRMatchArm.body_stmts`) becomes an IR-level basic block
  populated by `lower_statements` from Slice 1.
  `compile_body_as_value` retires; per-construct emit walkers stop
  calling `compile_statement` and walk the lowered block(s) via
  `execute_instructions` + `emit_terminator`. The mechanical shape
  is one-for-one across the eight constructs that still carry AST
  stubs (`IRUnless`, `IRIf`, `IRIfElse`, `IRCond` arm + else
  bodies, `IRMatch` arm bodies, `IRWhile`, `IRLoop`, `IRFor`):
  each construct's struct keeps its block ids and terminators;
  the `body_stmts` field becomes `body_block: IRBlockId` plus an
  `IRBasicBlock` materialized into a per-construct `body_blocks:
Vec<IRBasicBlock>` (or held as a flat `Vec<IRBasicBlock>`
  tied to the construct's id range). `IRFor` continues to carry
  its `iterable: Expr` and `binding_pattern: Pattern` whole at
  the IR seam through this slice (per the `PatternBinaryMatch`
  precedent from Wave 24); the iterator-protocol desugar still
  runs at emit time. The slice eliminates the inline merge-phi
  synthesis from `emit_if_else`, `emit_cond`, and
  `emit_match_unified` -- with arm bodies now real instruction
  streams, lowering captures the trailing-expression operand for
  each arm and pre-stages `IRInstruction::Phi` in the merge
  block exactly as `IRTernary` does today,   generalizing the one
  canonical pre-staging idiom from Wave 21 to every
  value-producing conditional. The `FnState.loop_exit_blocks`
  twin retires (the `FnLowerState.loop_exit` semantic stack from
  Slice 1 stays) -- with loop bodies now block-shaped, the
  back-edge `Branch(header)` and break `Branch(exit)` terminators
  are both pre-resolved at lowering time, so the codegen-side
  `(IRBlockId, BasicBlock)` pairing the `compile_statement` shim
  uses to seed `block_map` is no longer needed. The slice is
  large but mechanical; if cumulative diff exceeds review budget
  it splits into 2a (conditionals: `unless`, `if`, `if_else`,
  `cond`, `match`) and 2b (loops: `while`, `loop`, `for`);
  default to one wave. **Done when** no per-construct IR type
  carries `Vec<Statement>`; `compile_body_as_value` is deleted;
  merge-phi synthesis happens exclusively at lowering for every
  conditional construct; `FnState.loop_exit_blocks` is deleted.

- **Slice 3 -- `IRFunction` carries `Vec<IRBasicBlock>`;
  per-construct types and walkers dissolve.** The structural cut.
  `IRFunctionKind::Free { func_ast, ... }` and
  `IRFunctionKind::Method { func_ast, ... }` swap their
  `expo_ast::ast::Function` body for `blocks: Vec<IRBasicBlock>`.
  The planners (`monomorphize_function`,
  `monomorphize_impl_method` in
  [`expo-ir/src/lower/monomorphize.rs`](../crates/expo-ir/src/lower/monomorphize.rs))
  lower the AST body to blocks at planning time using the
  `lower_statements` seam from Slice 1; `func_ast` retires from
  both kinds (param names and span info migrate to a small
  per-function metadata struct so debug emission keeps its source
  positions). `emit_ir_function` and `emit_ir_impl_method` in
  [`expo-codegen/src/generics.rs`](../crates/expo-codegen/src/generics.rs)
  walk `IRFunction.blocks` instead of calling
  `compile_function_body` / `compile_method_body`. The LLVM-bound
  responsibilities those two helpers carry today -- entry block
  creation, param alloca setup, debug `push_function`,
  `type_subst` save/restore, the `tco_loop` block scaffolding --
  survive in `emit_ir_*` directly (or extract a thin
  `setup_function_frame` helper); `compile_function_body` and
  `compile_method_body` retire entirely (the unused `_is_main`
  parameter retires with them). With function bodies now
  block-of-blocks, the nine per-construct IR wrappers
  (`IRUnless`, `IRIf`, `IRIfElse`, `IRTernary`, `IRCond`,
  `IRMatch`, `IRWhile`, `IRLoop`, `IRFor`) collapse into
  free-floating `IRBasicBlock`s linked by their existing
  `IRTerminator`s -- there's no functional reason to keep the
  wrapper structs once the bodies they were wrapping are
  themselves blocks, and mature compiler precedent (Swift SIL,
  Rust MIR, GCC GIMPLE, LLVM IR) confirms control-flow shape
  lives at the CFG level rather than the type-tag level.
  Lowering produces blocks directly into the function's
  `Vec<IRBasicBlock>`; per-construct emit walkers retire in
  favor of one block walker that, per block, calls
  `execute_instructions` then `emit_terminator`. `IRFor`'s
  iterator-protocol desugar (`length()` / `get()` / `Option`
  unwrap / pattern bind) migrates from emit-time to
  planning-time -- the `build_for_loop_setup` /
  `resolve_for_impl_methods` / `build_for_header_check` /
  `build_for_element_load` / `bind_for_pattern` /
  `emit_for_back_edge` helpers move from
  [`expo-codegen::control::loops`](../crates/expo-codegen/src/control/loops.rs)
  into the IR-side for-lowering, generating the multi-block
  protocol expansion as part of the function's block list (the
  AST `iterable: Expr` and `binding_pattern: Pattern` are
  consumed during planning; the for loop becomes plain blocks).
  `PatternBinaryMatch` continues unchanged -- it's already an
  `IRInstruction` and lives inside whatever check block contains
  it.   Implicit-return semantics (today: trailing `Statement::Expr`
  in `compile_function_body`) become an `IRTerminator::Return`
  synthesized by lowering when the function body's tail block
  ends in an expression statement; tail-call status
  (`Lowerer::lower_tail_expr_to_operand`, in place from Wave 25)
  gets called for the last operand-shaped expression before the
  synthesized return. Invariant 9 ("one walker per IR shape")
  finally holds without transitional shims.
  This slice also lands the **pre-codegen elaboration pass**
  that retires the three transitional `compile_assignment` forks
  Slice 1 left in place. The pass runs after monomorphization
  planning but before any LLVM body is bound: it walks each
  function's lowered blocks and pre-resolves the protocol-driven
  coercions (today: list-literal `from_list`; future:
  `FromBinaryLiteral` / `FromFloatLiteral` / similar) into
  canonical `IRInstruction::MethodCall` sites with the impl
  method already registered in `IRProgram`. With elaboration in
  place, lowering can also use compile-time inference (the same
  unification typecheck records on `expr.resolved_type`) to type
  the LHS of unannotated assignments, so the third fork
  (unannotated RHS) retires too. The destructuring `Pattern`
  fork retires when destructuring patterns get IR support
  alongside the other pattern-shape lifts. With the elaboration
  pass landed, the `compile_statement` shim deletes entirely:
  every `Statement::Assignment` shape lowers to
  `StoreLocal` / `StoreField`, and the legacy `compile_assignment`
  / `apply_coercion` / `convert_list_literal_if_needed` helpers
  retire. **Done when** `IRFunctionKind::{Free,Method}` carry
  `Vec<IRBasicBlock>`; `compile_function_body`,
  `compile_method_body`, `compile_statement`, and
  `compile_assignment` are deleted; the nine
  `IR{Unless,If,IfElse,Ternary,Cond,Match,While,Loop,For}` types
  are deleted; the nine per-construct emit walkers are replaced
  by one block walker; the elaboration pass pre-resolves
  protocol coercions into `IRProgram`; `expo-codegen` performs
  no AST traversal except inside `IRInstruction::Stub` (whose
  retirement is Phase 4h).

- **Slice 4 -- Compiler trim (no `IRPackage`, no `IRFile`).**
  Retires `Compiler.current_package` because every IR element
  already carries its package via `TypeIdentifier` /
  `FunctionIdentifier`
  ([`expo-ast/src/identifier.rs`](../crates/expo-ast/src/identifier.rs))
  -- by Slice 3 emission walks `IRProgram.function_order` and
  reads each callable's package off its identifier; no ambient
  field on `Compiler` is needed once the data is properly
  tagged. `with_package` deletes; the `LowerCtx.package` /
  `Lowerer.package` fields stay (lowering still needs the
  scope-aware name resolution invariant) but get populated from
  the planner's per-package loop rather than from `Compiler`.
  The `closure_id` reform lands in this slice as the natural
  companion: parser mints a monotonic `ClosureId` per closure
  literal and bakes it into `Expr::Closure { closure_id, ... }`
  and `Expr::ShortClosure { closure_id, ... }`;
  `TypeContext.closure_info` re-keys from
  `(Option<PathBuf>, Span)` to `ClosureId`;
  `closure_info_at(ctx, span)` becomes
  `closure_info_at(ctx, closure_id)`;
  `Compiler.closure_site_path`, `LowerCtx.closure_site_path`,
  and `Lowerer.closure_site_path` fields delete along with the
  `define_functions` save/restore at
  [`compiler.rs:1295-1298`](../crates/expo-codegen/src/compiler.rs).
  The two production `unreachable!()` sites are addressed:
  [`expo-ir/src/lower/closures.rs:53`](../crates/expo-ir/src/lower/closures.rs)
  (the "all annotated" closure-params case) becomes a typed
  `LoweredClosureParam` enum that makes the case structurally
  absent;
  [`expo-codegen/src/expr.rs:369-370`](../crates/expo-codegen/src/expr.rs)
  (`Literal::String` in `compile_literal`) is documented for
  retirement when string literals lift in Phase 4h, not deleted
  in this slice. The decision to keep `IRProgram` flat (no
  `IRPackage` container) follows from the package-via-identifier
  discovery: per-package iteration is a cheap filter on flat
  registries (`program.functions.values().filter(|f|
f.mangled.package() == pkg)`); per-file metadata isn't needed
  at the IR level (files are pure organization in Expo, debug
  info flows through `DebugContext`, imports/aliases through
  `TypeContext`). `Compiler` becomes a pure consumer of
  `IRProgram` with no per-module ambient state. **Done when**
  `Compiler.current_package`, `Compiler.closure_site_path`,
  `LowerCtx.closure_site_path`, `Lowerer.closure_site_path`, and
  the `with_package` / save-restore plumbing are all deleted;
  `TypeContext.closure_info` keys by `ClosureId`; the
  `closures.rs:53` unreachable retires.

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
4. Closure construction (`partial_apply` shape; consumes the
   `ClosureId`-keyed `TypeContext.closure_info` from Phase 4g
   Slice 4 and pre-resolves each closure's `ClosureInfo` directly
   onto its `partial_apply` instruction, so codegen never queries
   `closure_info` at emit time).
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
