# Compiler northstar — working discussion

Working notes for the design conversation that will produce
`COMPILER-NORTHSTAR.md`. This doc is **not** the spec; it is the
record of decisions and their rationale so that future sessions
(human or AI) can pick up the thread without re-litigating settled
questions.

## Why this exists

The codebase has accumulated multiple parallel mechanisms for the
same job (most notably: generic instantiation discovery happens in
`koja-ir/src/closure/`, in `koja-ir/src/lower/` IR lifters, **and**
in `koja-codegen/src/generics.rs` lazy backfill). Multiple AI
sessions have implemented similar things in different places because
the architectural responsibilities aren't pinned down anywhere
authoritative. The next refactor needs a single load-bearing doc
that:

- Describes the **destination**, not the **trajectory** (no "Slice N"
  markers; those belong in plan docs that have a finite lifetime).
- Is **testable** — every responsibility statement implies an
  assertion or a grep, not just a vibe.
- Commits to **one answer** for each ambiguity that has bitten us.

The plan is to settle the load-bearing decisions here in
conversation, then draft `COMPILER-NORTHSTAR.md` from the settled
list, then validate the doc by walking it against the current code
and producing a refactor backlog of named violations.

## Meta-trap to avoid

The pre-existing `stub-categorization.md` was internally consistent
but measuring the wrong thing (the IR lifter passes `&|_| false` for
`type_mono_exists`, so the `pending-type-mono` cohort the doc
tracked is structurally undrainable by the closure pass). A
confident-sounding architectural doc that's subtly wrong is worse
than no doc — it lends false confidence to subsequent work.

Mitigation: every claim in the northstar doc should be reduced to a
mechanical check (a grep, an assertion in a `seal_*` function, a
test). Aspirational claims are forbidden.

## Glossary (so we don't trip on terms)

These terms recur and are easy to conflate. Definitions used in
this doc (and to be reused in `COMPILER-NORTHSTAR.md`):

- **Lowerer.** The `koja-ir::Lowerer` struct and its per-function
  state. Owns the AST→IR translation work for one function body.
- **`lower::*` helpers.** The free functions in
  `koja-ir/src/lower/` (e.g. `lower_method_call_or_stub`,
  `lower_static_call_or_stub`). Per-AST-node translation logic
  invoked by the `Lowerer`.
- **Closure pass.** The whole-program walk in
  `koja-ir/src/closure/` that discovers which generic instantiations
  the source actually references and registers them in `IRProgram`
  via the planners.
- **Planners.** The `monomorphize_*` functions in
  `koja-ir/src/lower/monomorphize.rs`. They take a generic decl + a
  type-arg vector and append a specialized `IRStruct` / `IREnum` /
  `IRFunction` to `IRProgram`. LLVM-free; idempotent.
- **Discovery.** Deciding _which_ generic instantiations exist at
  all. (Distinct from monomorphization, which is _producing_ the
  specialized decl once you know it's needed.)
- **Lazy backfill.** `koja-codegen/src/generics.rs`'s current
  fallback: when codegen encounters a mangled symbol that isn't
  registered, it calls a planner on the spot. Going away (per D1).
- **Seal.** `seal_program(&IRProgram) -> Result<(),
SealViolation>`. A pass that asserts the invariants the rest of
  the pipeline relies on. Runs after the closure pass.
- **Sealed `IRProgram`.** An `IRProgram` that has passed `seal`. The
  promised input shape for `koja-codegen` and `koja-ir-eval`.

## Open questions (load-bearing)

These were enumerated up front. Resolution status tracked here.

1. **Is monomorphization mandatory before codegen, or can it be
   lazy?** — RESOLVED. Mandatory.
2. **Does `IRProgram` carry the user `fn main` body, or is it a
   special case?** — RESOLVED. Carry it; entry-point variant points
   at it. `fn main` itself is legacy and going away.
3. **Where does generic instantiation discovery live?** —
   RESOLVED. Closure pass exclusively; the `lower::*` helpers are
   pure translators that trust the registry.
4. **What does `koja-codegen` know about types?** — RESOLVED.
   Codegen consumes only the sealed `IRProgram` and builds its own
   indices.
5. **Is `IRProgram` one type with stages, or multiple types with
   translations?** — RESOLVED. Single `IRProgram` type with
   progressively tighter invariants enforced by passes; seal is a
   runtime check at one well-defined seam.
6. **Does `koja-ir-eval` consume the same `IRProgram` as
   `koja-codegen`?** — RESOLVED. Yes; same sealed `IRProgram`, no
   IR construct for dynamic dispatch. Interactive REPL debugging is
   interpreter-binary machinery on top of sealed IR.
7. **Where does `Coercion::*` get lowered out of the IR?** —
   RESOLVED. Lowered out at the AST→IR boundary; every `Coercion`
   becomes an explicit `IRInstruction`; sealed `IRProgram` carries
   no coercion metadata on operands; `apply_coercion` in codegen
   gets deleted.
8. **What's the relationship between `koja-typecheck` and
   `koja-ir`'s `lower::types`?** — RESOLVED. Typecheck delivers a
   sealed AST with all value-level decisions recorded as
   annotations on AST nodes; koja-ir validates nothing about
   names/types/overloads/coercions; `lower::types`'s name- and
   type-resolution helpers get deleted; the per-function
   substitution helper stays in koja-ir as specialization machinery.

## Decisions

### D1. Monomorphization is mandatory before codegen

The closure pass is the **single authoritative** discovery
mechanism for generic instantiations. Not "primary with fallback";
authoritative.

Mechanical consequences (each one is a grep or a deletion target):

- `koja-codegen/src/generics.rs` lazy backfill paths get **deleted**,
  not gated. That includes the four sites that bump `lazy_mono_count`
  and the `lazy_mono_count` field itself.
- `koja-codegen` never calls `monomorphize_*`. Grep-checkable.
- The `&|_| false` callback in `lower_static_call_or_stub` (and any
  similar "this might not be registered yet" callback in the IR
  lifter) becomes `&|id| program.contains_*(id)`. The IR lifter
  trusts the registry once the closure pass is authoritative.
- A `seal_program(&IRProgram) -> Result<(), SealViolation>` runs
  after the closure pass and **panics** on violation. No advisory
  warning.

Transitional cost: short-term, closure-pass misses become panics in
tests instead of silent backfills. That's the right direction (loud
failures > silent backfills) but real. Sequencing in the migration
plan must account for it.

Chicken-and-egg note: the closure pass walks `program.function_order`,
so non-generic decls must be registered into `IRProgram` _before_
the closure pass runs. Today's order — (a) per-function lower
registers non-generics, (b) closure pass discovers and registers
generics, (c) seal asserts, (d) codegen emits — is the right shape
and stays.

### D2. Entry points are an explicit `IRProgram` field; `fn main` is legacy

Two-part decision.

**Part A: `fn main` is legacy and going away.** The target language
has two entry-point conventions:

1. An koja project that defines `Process.run` as the entry method.
2. A single-file shell-style execution that evaluates the whole file
   (no `main`; `koja-ir-eval` consumes this directly).

`fn main` exists today as a transitional convention and is slated
for deletion once the two target conventions are wired up.

**Part B: `IRProgram` carries an explicit entry-point field.** The
shape (subject to refinement during doc drafting):

```rust
pub struct IRProgram {
    pub functions: HashMap<FunctionIdentifier, IRFunction>,
    pub function_order: Vec<FunctionIdentifier>,
    pub structs: ...,
    pub enums: ...,
    pub entry_point: EntryPoint,
    ...
}

pub enum EntryPoint {
    /// Transitional: user wrote `fn main`. Body lives in
    /// `IRProgram.functions` as a normal `IRFunctionKind::Free`.
    /// Deleted when `fn main` support is removed.
    LegacyMain { entry: FunctionIdentifier },

    /// User wrote `Process.run`. The runtime invokes this method.
    Process {
        state_type: MonomorphizedTypeIdentifier,
        run: FunctionIdentifier,
    },

    /// Whole-file evaluation. The "program" is the file's
    /// top-level statements wrapped as a Free function.
    Eval { entry: FunctionIdentifier },
}
```

Mechanical consequences:

- `IRFunctionKind::MainEntry` is **deleted**. The user `fn main`
  body becomes a normal `IRFunctionKind::Free`; the entry-point
  field points at it. The closure pass walks it like any other Free
  function; the `__koja_user_main` `pending-mono` bails dissolve
  rather than getting worked around.
- The synthesized C-callable `int main(...)` shim becomes a
  codegen concern, not an IR concern. Codegen reads
  `program.entry_point` once at emission start, dispatches on the
  variant, and synthesizes its own backend-specific shim (`int main`
  for LLVM, a callable handle for eval).
- `koja-ir-eval` reads `entry_point` once, looks up the entry
  function, interprets from there. No magic-named `__koja_*` lookup.
- `seal_program` asserts `entry_point.referenced_function` exists
  in `functions` and is non-generic.
- The `if func.name == "main"` conditionals scattered through
  `koja-codegen/src/compiler.rs` collapse to one `match
program.entry_point { ... }` at one well-defined emission point.

Migration sequencing (plan-doc territory, not northstar territory,
but worth recording so we don't forget):

1. Land `COMPILER-NORTHSTAR.md` describing the destination.
2. Build the `Process.run` and eval entry-point machinery (new
   `EntryPoint` variants wired through codegen + eval).
3. Migrate test programs from `fn main` to the new conventions.
4. Delete `fn main` parsing/lowering support.
5. Delete `IRFunctionKind::MainEntry`.
6. Delete `koja-codegen/src/generics.rs` lazy backfill (the closure
   pass is now actually authoritative).
7. Promote `seal_program` from warning to panic.

D1 (mandatory monomorphization) and D2 (explicit entry point) are
mutually reinforcing: D1 needs the entry body to be visible to the
closure pass; D2 makes it visible.

### D3. The closure pass is the only discoverer; `lower::*` helpers are pure translators

Direct extension of D1. Stated explicitly because the codebase
historically had three discovery mechanisms (closure pass, `lower::*`
helpers via `pending_*` fields, codegen lazy backfill) and the
ambiguity drove parallel implementations.

In the destination architecture:

- The **closure pass** discovers every generic instantiation the
  source references and registers each one in `IRProgram` via the
  planners. Authoritative.
- The **`lower::*` helpers** never discover. They translate AST nodes
  to IR instructions, look up callees by mangled name in `IRProgram`,
  and trust the result. Never invoke planners. Never bail with
  `Ok(None)` because a symbol "isn't yet registered."
- A lookup miss inside a `lower::*` helper is a programming bug
  (closure pass missed it) and surfaces as a panic with a useful
  diagnostic ("closure pass missed `<mangled>` referenced from
  `<enclosing fn>` at `<span>`"). It does not silently fall back.
- The **lazy backfill** in `koja-codegen/src/generics.rs` is deleted
  (per D1).

Mechanical consequences (each is a deletion or a grep target):

- The `pending_mono` / `pending_type_mono` fields on
  `ResolvedMethodCall` and `ResolvedStaticCall` are deleted. The
  `PendingMethodMono` / `PendingTypeMono` types are deleted.
- The `Ok(None)` return paths in `lower_method_call_or_stub`,
  `lower_static_call_or_stub`, `lower_call_or_stub`,
  `lower_ident_or_stub` are deleted. Each `Ok(None)` either becomes
  `Ok(Some(...))` (the lift succeeds because the registry is
  trusted) or a panic (the lift can't proceed and that's a closure
  pass bug).
- The `IRInstruction::Stub` variant is eventually deleted. (Once
  every `_or_stub` helper either succeeds or panics, no `Stub` is
  ever emitted, and the variant has no constructors. The seal pass
  asserts no `Stub` remains in any block.)
- The `&|_| false` callback in `lower_static_call_or_stub` becomes
  `&|id| program.contains_struct(id) || program.contains_enum(id)`
  — except that callback's only consumer was the `pending_type_mono`
  bail path, which is also deleted, so the callback parameter goes
  away entirely.
- The `_or_stub` suffix on the helpers becomes a misnomer and gets
  renamed (e.g. `lower_method_call`).

What this _doesn't_ mean:

- The closure pass doesn't have to monomorphize on first reach. The
  fixpoint loop in `closure_program` already iterates until no new
  decls are added; that stays. What changes is what happens when
  closure pass _misses_ something — today it's invisible, in the
  destination it's a panic.
- The closure pass doesn't have to run before the initial per-
  function lower. Today's order — (a) per-function lower registers
  non-generics, (b) closure pass discovers generics, (c) seal, (d)
  codegen — is unchanged. The `lower::*` helpers don't need the
  registry during phase (a) because phase (a) only handles
  non-generic decls. They need it during phase (b) (`populate_ir_blocks`,
  which lifts the bodies of freshly-monomorphized decls), and by
  then the closure pass has populated everything.

Cost to flag:

- The closure pass becomes load-bearing. It has to actually cover
  everything; today it doesn't (the recent closure-pass extension
  attempt confirmed this empirically). Migrating to D3 is a real
  investment in closure-pass coverage _before_ the `Ok(None)` paths
  can be deleted, or test failures will block the migration.

### D4. `koja-codegen` consumes only the sealed `IRProgram` and builds its own indices

`koja-codegen` does not import `koja-typecheck`. It does not hold a
`&TypeContext`. The `Compiler.type_ctx` field is deleted.

`IRProgram` is **data**, deliberately spartan. It exposes the
program's facts (functions, structs, enums, entry point) and a
universal lookup primitive (mangled-name lookup). It does not
precompute backend-specific indices, because future backends may
want different ones than LLVM does.

Each backend builds its own indices on top of `IRProgram`. For
LLVM codegen these are roughly:

- `MonomorphizedTypeIdentifier` → `inkwell::types::StructType`
- `FunctionIdentifier` → `inkwell::values::FunctionValue`
- whatever else turns out to be useful for fast emission.

For `koja-ir-eval` the indices are different (interpreter handles,
not LLVM values). For a hypothetical future backend (Cranelift,
WASM, native interpreter, whatever) they're whatever that backend
needs. `IRProgram` stays out of it.

Mechanical contract (each is a grep target):

- `rg 'koja_typecheck' expo/crates/koja-codegen/src/` — must be
  empty (or only basic re-exported types if those are routed through
  `koja-ir`).
- `rg 'type_ctx' expo/crates/koja-codegen/src/` — must be empty.
- `rg 'TypeContext' expo/crates/koja-codegen/src/` — must be empty.

The four families of current `c.type_ctx` use that codegen will
need to migrate, with their fixes:

1. **Type-definition lookups** (`c.type_ctx.get_type(id)` for
   field/variant info during LLVM struct construction). Fix:
   `IRStruct.fields` and `IREnum.variants` carry this. Use them.
2. **Method-signature lookups** (`c.type_ctx.function_sig(...)`).
   Fix: `IRFunction.param_types` and `IRFunction.return_type` are
   the source of truth.
3. **Name resolution** (`resolve_name_current(...)` etc.). Fix:
   codegen should never resolve a name during emit. If it has a
   name, the IR should carry the resolved identifier already. If
   it's resolving from raw AST during emit, that's the actual bug
   to find.
4. **Specialized-impl lookups** (`c.type_ctx.specialized_impl_asts`
   and friends, used for protocol resolution). Fix: specialized
   impls get baked into `IRProgram` by the closure pass / elaborate.
   If they aren't, that's a closure-pass / elaborate gap to close —
   not a reason for codegen to peek at typecheck data.

Downstream consequence — the cross-crate `LowerCtx` API:

`LowerCtx` (`koja-ir/src/lower/ctx.rs`) currently bundles
`type_ctx`, `fn_lower`, `package`, `layouts`, `closure_site_path`
and is constructed _by codegen_ to call into `koja-ir`'s `lower::*`
helpers for the lazy backfill. Once D1 deletes the lazy backfill
and D4 forbids codegen from holding `type_ctx`, codegen has no
business constructing a `LowerCtx`. The cross-crate calls into
`lower::*` go away. `LowerCtx` becomes private to `koja-ir`.

That collapses `koja-ir`'s public surface dramatically. The
post-migration export list is roughly:

- `IRProgram`, `IRFunction`, `IRStruct`, `IREnum`, `IRInstruction`,
  `IRBasicBlock`, `IRBlockId`, `IROperand`, `EntryPoint`, the
  identifier types (`FunctionIdentifier`,
  `MonomorphizedTypeIdentifier`).
- `lower_program(modules, type_ctx) -> IRProgram` (the one entry
  point that runs the per-function lower + closure pass +
  elaborate + seal).
- `seal_program(&IRProgram) -> Result<(), SealViolation>` (exposed
  for tooling that wants to validate an IR independently).

Everything in `lower::*` and `closure::*` becomes crate-private.

D3 and D4 are mutually reinforcing in the same way D1+D2 are: D3
shrinks what `lower::*` does (so `LowerCtx` can shrink), and D4
forbids the cross-crate API that drove `LowerCtx`'s current shape.

### D5. `koja-ir-eval` and `koja-codegen` consume the same sealed `IRProgram`; no dynamic-dispatch IR construct

Both backends consume the same data type with the same invariants.
Eval interprets it; codegen emits LLVM. Neither needs a special
construct the other doesn't.

Underpinning this: **all polymorphic dispatch in koja is expressed
through generics, and generics are mandatorily monomorphized
(D1).** There is no `dyn Trait` / trait objects, no inheritance
polymorphism, no duck typing. Every call site in a sealed
`IRProgram` resolves to a concrete mangled callee.

Mechanical consequences:

- `IRInstruction::Call` (and friends) carry only mangled callee
  references. No `DynamicCall { vtable, method_index }`. No
  `PolymorphicCall { generic_name, type_args }`. The seal pass
  asserts this.
- Eval's interpreter is a plain "look up `FunctionIdentifier`,
  invoke, recurse" loop. No vtable machinery.
- Codegen emits `inkwell::values::FunctionValue` lookups. No
  function-pointer indirection for polymorphism (function pointers
  for first-class function values are still fine — that's not
  polymorphism, that's first-class functions over a concrete `fn`
  type).

**Principle (broader than D5, worth recording):** **interactive
debugging tooling is interpreter-binary machinery, not an IR
feature.** A REPL that wants to dynamically poke at running
processes (e.g. send a typed message to a running counter and
inspect the reply) builds that capability on top of sealed IR
using interpreter-side runtime introspection. It does not require
an `IRInstruction::Dynamic*` construct.

Future-AI guidance: if you find yourself reaching for a dynamic-
dispatch IR construct, stop. Either (a) the polymorphism is
expressible through generics and should be (in which case
monomorphize it), or (b) it's a debugging/REPL feature and belongs
in the interpreter binary on top of sealed IR. The IR enum stays
free of dynamic-dispatch constructs.

What this rules out (be explicit so we don't drift):

- Heterogeneous collections of trait objects (`List<dyn Trait>`).
  Sum types only: `enum Shape { Circle(...) | Square(...) }`.
- Inheritance-based virtual method dispatch.
- `Any`-typed values / runtime type erasure as a language feature.
- An IR construct for dynamic dispatch even if a future feature
  _seems_ to need it. Such features either get expressed through
  generics or built in interpreter-side machinery.

Open follow-up (Q9): the design of `monitor()` for typed processes
must be expressible without runtime expansion of M (the receivable
message type). Three plausible designs (system-event channel
modeled like `Lifecycle`; statically-declared union expansion;
typed wrapper) are all compatible with this constraint. The actual
choice is a future language-design decision and does not need to
be settled in the northstar doc.

### D6. Single `IRProgram` type with progressively tightened invariants; seal is a runtime check

`IRProgram` is one struct in `koja-ir/src/program.rs`. It does not
gain a phase parameter (`IRProgram<Sealed>`) and does not get split
into per-stage variants (`RawIRProgram` → `MonomorphizedIRProgram`
→ `SealedIRProgram`).

Passes are mutating: they take `&mut IRProgram`, modify it in place,
and document which invariants they assume on input and establish on
output. Phase ordering is enforced by the structure of the
pipeline, not by the type system.

Mechanical commitments:

- One `IRProgram` struct. No phantom phase parameter. No parallel
  per-stage types.
- One pipeline entry point: `lower_program(packages: &[IRPackage],
entry: EntryPointSpec, type_ctx) -> Result<IRProgram, ...>`.
  Internally runs merge → closure → elaborate → seal in order. The
  returned `IRProgram` is sealed by construction. (Per-package source
  lowering is a separate `lower_package` entry point; see D7.)
- `seal_program(&IRProgram) -> Result<(), SealViolation>` is
  exposed for tooling that wants to validate independently.
- Backend signatures take `&IRProgram` (no phase marker). The
  contract is documented: "input must be sealed; behavior on
  unsealed input is unspecified / debug-asserts panic."
- **Doc-comment discipline**: every pass documents which invariants
  it assumes and which it establishes. Format: `## Assumes: ...` and
  `## Establishes: ...` sections in the pass's module-level doc.
  Not mechanically enforced (single maintainer; cheap to commit
  to). Future-AI guidance: when adding a pass, write these sections
  before writing the code.

Rationale (why A over B/C from the discussion):

- Optimization passes added in the future just declare the
  invariants they need and operate on `&mut IRProgram`. Adding a
  new pass doesn't require updating type signatures across the
  codebase.
- The bug type-state would catch (calling pass-N before pass-N-1
  finished) is already prevented by the single-entry-point
  pipeline structure.
- `From` plumbing for split types would be many hundreds of lines
  of mechanical translation code that adds zero semantic value.
- Swift / SIL works this way and scales to a much larger IR
  surface than koja-lang's.

Open follow-up (Q10): incremental compilation. **Resolved as D7.**
Per-package source-lowering is cacheable; closure pass and seal
always re-run whole-program over the merged result.

### Note on `CFGBuilder` and the recursive AST-walk pattern

Out of scope for the destination architecture (it's an internal
mechanism, not a layer boundary), but worth noting since it came up:

The recursive AST-walk pattern with a threaded `CFGBuilder` —
each `lower_*` helper takes `(builder, current_block_id)` and
returns the (possibly new) current block plus the result operand —
**stays.** It's the standard shape for IR construction (LLVM's
`IRBuilder`, Cranelift's `FunctionBuilder`) and it's the right
shape for this kind of work: block creation and termination are
handled by the helper, not by the caller, and the recursive
structure of the IR matches the recursive structure of the AST.

D3 (`lower::*` helpers as pure translators) doesn't change anything
about how `CFGBuilder` works. The only refinement: under D3, the
helpers trust the `IRProgram` registry for symbol lookups, so they
no longer need to express "what if this symbol isn't registered"
in their return shape (no more `Ok(None) → Stub` paths).

### D7. Incremental compilation is package-granular; closure + seal always whole-program

The unit of source-lowering caching is the **package** (collection
of files that share a namespace, e.g. the user's project, each
stdlib package, each third-party dependency). It is not the file,
not the module (koja has no modules), and not the function.

This matches the language's own granularity: within a package all
types are mutually visible (no imports); across packages, types
are referenced via qualification (`json.Decoder`) or `alias`. The
package is the smallest unit whose type information is
self-contained enough to lower independently.

A new IR type `IRPackage` is introduced as the per-package
fragment: structs, enums, non-generic IR functions defined in that
package, plus the generic ASTs the package contributes for later
monomorphization. `IRPackage` is what gets cached on disk per
package and what `lower_package` produces.

`IRProgram` (per D6) is what `lower_program` produces by merging
`IRPackage`s, running the closure pass, and sealing. `IRProgram`
is never cached; it is reconstructed every build.

Mechanical commitments:

- **Pipeline shape** (refines D6's single-entry-point statement):

```text
lower_package(source: &PackageSource, deps: &[&IRPackage],
              type_ctx) -> Result<IRPackage, ...>
    // pure function of source + dep interfaces; cacheable.

lower_program(packages: &[IRPackage], entry: EntryPointSpec,
              type_ctx) -> Result<IRProgram, ...>
    // merges fragments
    // runs closure pass over the merged whole-program view
    // resolves entry symbol
    // seals
    // returns sealed IRProgram
```

- **`IRPackage`** is a new type in `koja-ir/src/program.rs` (or a
  sibling module). It is the source-lowering output of a single
  package and the unit of disk-caching.
- **`IRProgram` is never cached**, only reconstructed. The
  cheap-to-recompute passes (merge, closure, seal) always run
  whole-program. This avoids the entire class of "stale
  monomorphization registry" bugs.
- **Cross-package monomorphizations live in `IRProgram`, not in any
  one `IRPackage`.** If package B calls `A.identity(42)`, the mono
  `identity_$Int$` lives in the merged `IRProgram.functions`. Its
  body is derived from A's generic AST (which A's `IRPackage`
  carries). The closure pass discovers the instantiation by
  walking B; the planner emits the body using A's generic decl.
  Neither `IRPackage` "owns" the mono in a caching sense — the
  closure pass produces it fresh each build.
- **Invalidation rule for an `IRPackage` cache entry**: a package
  P's cached fragment is valid iff
  - (a) the hash of P's source files is unchanged, AND
  - (b) the hashes of P's dependency packages' fragments are
    unchanged (specifically: their public-interface portion —
    type defs, function signatures, generic decls — not their
    function bodies, since those don't affect P's lowering).
    Implementation note: a conservative first cut can use
    whole-fragment hashes for (b) and refine to interface-only later
    without changing the contract.
- **Codegen function-level caching is layered on top.** Each
  emitted LLVM function is keyed by `(mangled_name,
hash(IRFunction body + signature + dependency mono hashes))`.
  Same body + same dependencies = same bytecode, regardless of
  which package or build triggered it. Out of scope for the
  initial pipeline; the architecture supports it without further
  changes.
- **Entry point stays at the `IRProgram` level** (per D2). A
  library package built standalone produces an `IRPackage` and
  stops; only executable builds construct an `IRProgram` and
  require an `EntryPointSpec`.

REPL semantics fall out:

- The REPL session has an "ad-hoc package" that grows by one
  fragment per input. After each input: re-lower that fragment
  (small), re-run `lower_program` over `[user_packages...,
stdlib..., repl_session_package]` (cheap), hand the resulting
  sealed `IRProgram` to eval.
- No special-case machinery in the IR for REPL. Same pipeline,
  same `IRProgram`, just a tiny package that grows.

What this rules out (be explicit):

- File-level granularity. A change to one file in package P
  invalidates the entire `IRPackage` for P. This is coarser than
  rustc's module-level incremental, but the package model makes
  finer granularity unnecessary (typical packages are small) and
  the implementation is dramatically simpler.
- Caching `IRProgram` directly. The merge + closure + seal passes
  run every build. Don't try to make them incremental — they're
  whole-program by design and cheap (LLVM-free, ms for typical
  projects).
- Per-function source-level invalidation. Function-level caching
  exists, but only at the codegen layer (`IRFunction →
LLVMBytecode`), where it's content-addressed and cross-package
  by construction.

Future-AI guidance: when adding a feature that touches the
pipeline boundary, check which side it belongs on. If it's
per-package and cacheable (e.g. typecheck rules, name resolution
within a package), it goes in `lower_package`. If it requires the
whole-program view (e.g. closure-pass extensions, seal
assertions, cross-package optimization passes), it goes in
`lower_program` and runs every build.

### D8. Coercions are typecheck annotations; lowered to explicit IR instructions; codegen never sees them

`Coercion` (defined in `koja-typecheck/src/context.rs`) is a
**typecheck-layer concept**. It records a decision the type
checker made: "the value at this site needs widening / wrapping /
conversion before it can flow to its consumer." It exists only as
metadata attached to AST nodes.

The `lower::*` helpers in `koja-ir` are **responsible for translating
every `Coercion` annotation into an explicit `IRInstruction`** at the
exact site the coercion was annotated. By the time AST→IR lowering
finishes for an expression, all `Coercion` metadata has been
consumed and emitted as IR instructions. The sealed `IRProgram`
contains zero coercion metadata anywhere in its operand graph.

`koja-codegen` (and `koja-ir-eval`) **never branch on
`Coercion::*`**. Each backend just emits the `IRInstruction` it sees,
one per coercion kind, with a direct LLVM (or interpreter) pattern.

This is the Swift/SIL pattern: Sema annotates the AST with
conversion nodes (`ImplicitConversionExpr`, `ErasureExpr`,
`InjectIntoOptionalExpr`, etc.); SILGen reads the annotations and
emits dedicated SIL instructions (`init_existential_*`, `enum`,
`upcast`, `convert_function`, etc.); IRGen translates each SIL
instruction to LLVM with no branching on coercion semantics.

Mechanical commitments:

- **`Coercion` enum stays in `koja-typecheck`.** It is the type
  checker's vocabulary for telling the lowerer what conversions
  are needed. It does not appear in any `koja-ir` or
  `koja-codegen` type signature.
- **One `IRInstruction::*` variant per `Coercion::*` variant.**
  Today: `Coercion::UnionWiden` ↔ `IRInstruction::UnionWrap`. Any
  new `Coercion` variant added to `koja-typecheck` must be paired,
  in the same change, with a new `IRInstruction` and the
  corresponding lowerer emission. Grep-checkable: every `Coercion`
  variant should appear in `koja-ir/src/lower/coercion.rs`'s
  staging dispatch.
- **`koja-codegen/src/stmt.rs::apply_coercion` is deleted**, not
  relocated. Its current `UnionWiden` arm is the last surviving
  instance of the operand-metadata anti-pattern and is the
  concrete unblock for D4 (codegen as pure emission).
- **Seal-pass assertion**: `seal_program` walks the operand graph
  and panics on any operand carrying `Coercion` metadata. Today
  this is true by accident on most paths; the seal makes it a
  contract.
- **Naming convention**: each `IRInstruction` that materializes a
  `Coercion` is named for the _operation_, not the source
  coercion (e.g. `UnionWrap` not `UnionWidenInst`). Coercions are
  decisions; instructions are operations. Different layers,
  different vocabularies.

Why this works (mirroring the Swift rationale):

- **Optimizer visibility.** Coercion-as-instruction is visible to
  any future IR pass. The pass can fold redundant pairs, hoist
  invariant wraps out of loops, eliminate cancelled-out
  conversions. Operand-metadata coercions are invisible to passes.
- **Backend simplicity.** Codegen and eval each get one match arm
  per `IRInstruction` and are done. No conditional logic about
  "did this operand need coercion." This is what makes D4
  (codegen as pure emission) actually achievable.
- **One source of truth.** "Did a coercion happen here?" is
  answered by "is there a coercion-emitting instruction in the
  IR?" — not by inspecting operand metadata at three different
  layers.

What this rules out:

- Storing `Coercion` on `Operand` or any other IR-side type.
- "Just-in-time" coercion application in `koja-codegen` (the
  current `apply_coercion` shape).
- A single generic `IRInstruction::Coerce(Coercion)` that pushes
  the coercion-kind switch into the backend. Each coercion gets
  its own instruction variant; backends match on instruction
  variant, not on a payload.
- Asymmetry between codegen and eval. Both consume the same
  sealed IR; both have the same per-instruction emission pattern;
  neither imports `Coercion`.

Future-AI guidance: if you find yourself adding a `Coercion::*`
variant to `koja-typecheck`, the next file you touch is
`koja-ir/src/lower/coercion.rs` (paired emitter) and the next
after that is `koja-ir`'s instruction enum (paired
`IRInstruction::*`). If you find yourself reaching for
`apply_coercion` in `koja-codegen`, stop — the answer is to lift
the coercion at its `koja-ir` lowering site, not to interpret it
in the backend.

Migration note: today there is exactly one `Coercion` variant
(`UnionWiden`) and the lift is approximately 70% complete (Slices
1 and 2 covered method-call args/receivers and free/static call
args). Finishing Q7 is mechanical: walk remaining
`apply_coercion` call sites, find the corresponding `koja-ir`
lowerer, add `stage_union_widen` (or its successor in
`coercion.rs`), delete `apply_coercion`, add the seal assertion.
This is a near-term slice once the northstar is drafted.

### D9. Typecheck delivers a sealed AST; annotations are identity handles; consumers build their own indices

The `typecheck → koja-ir` boundary mirrors the `koja-ir →
koja-codegen` boundary established by D1 + D6. Typecheck delivers
a **sealed AST** to koja-ir; koja-ir validates nothing about
names, types, overloads, dispatch, or coercions — those decisions
are already made and recorded on AST nodes.

This generalizes D4 ("koja-codegen consumes only the sealed
`IRProgram` and builds its own indices") to **all** downstream
consumers of typecheck output: koja-ir (for lowering), the LSP
(for editor operations), formatters, doc-gen tools, anything
else. The AST + its annotations are the substrate; each consumer
builds its own purpose-shaped indices over those annotations.

#### Sealed-A interpretation

Two interpretations of "sealed AST" exist:

- **Sealed-A (Swift-style):** typecheck resolves all _value-level_
  decisions (names, types, overloads, coercions, `resolved_type`
  at every Expr). Generic decls remain with type-parameter
  references; _specialization_ is koja-ir's job (per D1).
- **Sealed-B (Rust MIR-style):** typecheck also discovers required
  monomorphizations and produces N specialized AST copies per
  generic decl. koja-ir lowers each with no substitutions left.

This decision commits to **Sealed-A**. Specialization stays in
koja-ir (closure pass + planners); typecheck doesn't take on
LLVM-mangling concerns; the migration from current code is
incremental rather than a wholesale typecheck rewrite.

The seal accepts type parameters and `Self` in generic-decl
bodies as "resolved" — they're properly bound and named at every
site. What it rejects is `Type::Unknown` or `None` annotations
on typecheck-success ASTs.

#### Annotations live on AST nodes (Choice (i))

Resolution information is attached directly to AST nodes, not
stored in a parallel side-table that consumers query by node ID.
This is a deliberate choice over the side-table alternative;
rationale:

- **LSP gets first-class navigation.** Every editor operation
  (go-to-def, hover, find-references, rename) is an AST walk
  from the cursor's node. No "query the side-table, hope it's
  still in sync" indirection.
- **Per-package source-lowering caching (D7) is straightforward.**
  The typed AST is what gets serialized; annotations are inherent
  to the AST, not a side-table that has to be re-derived or kept
  in sync across builds.
- **The "stale side-table" failure mode is structurally
  impossible** — there is no side-table to drift.

We've already accepted the AST is not pure (it carries
`resolved_type` today). D9 deepens that acceptance into a
structural commitment.

#### One identifier type

`TypeIdentifier` (already defined in `koja-ast/src/identifier.rs`)
gets a small structural extension:

```rust
pub struct TypeIdentifier {
    pub package: Package,
    pub path: Vec<String>,  // lexical containment chain
}
```

The change is `name: String` → `path: Vec<String>`. Every other
property of `TypeIdentifier` (content-addressed, deterministic
across builds, `qualified_name` round-trip) is preserved.

This single identifier type identifies anything _bound_ in the
program — types, functions, methods, fields, variants, locals,
type parameters, anonymous closures, all of them. (Functions
have function types in koja's model; calling the struct
`TypeIdentifier` still fits.)

The path is the lexical containment chain. Examples:

| Decl                       | `path`                                      |
| -------------------------- | ------------------------------------------- |
| Top-level struct `User`    | `["User"]`                                  |
| Top-level fn `validate`    | `["validate"]`                              |
| Nested struct `User.Role`  | `["User", "Role"]`                          |
| Method on `Role`           | `["User", "Role", "validate"]`              |
| Struct defined inside a fn | `["User", "Role", "validate", "TempTable"]` |
| Enum variant               | `["Color", "Red"]`                          |
| Field on a struct          | `["Circle", "radius"]`                      |
| Local in a fn              | `["validate", "x"]`                         |
| Type param in a fn         | `["identity", "T"]`                         |

Type args remain _separate_ from the identifier (composed at use
sites): `List<Int>` is `(TypeIdentifier { package: Std, path:
["List"] }, type_args: [Type::Int])`. The identifier names the
generic decl; type args parameterize uses of it. Today's
`Type::Named { identifier, type_args }` shape stays correct.

#### Single-variant resolution enum (migration prep)

```rust
pub enum Resolution {
    Global(TypeIdentifier),
}
// AST annotation: Option<Resolution>
```

The single-variant enum costs ~nothing today (one
`Resolution::Global(...)` pattern instead of a bare identifier)
and turns any future shape change — e.g. introducing
`Resolution::Local(LocalId)` as a memory optimization once
performance demands it — into a **compiler-enforced** migration:
Rust's exhaustiveness checker flags every read site that needs
an arm for the new variant.

This is the architectural insurance against premature
commitment. Consumers code against `Resolution`; the underlying
representation can evolve without grep-and-pray refactoring.

#### Naming policy for synthesized path segments

Most path segments come from source-given names. Some don't —
anonymous closures and disambiguating shadow blocks need
synthesized segments. Policy, in priority order:

1. **Source-given when available.** A `let f = |x| x + 1`
   produces `["fn_name", "f"]` for the binding; the closure
   value lives at the binding's path.
2. **Call-site-relative for anonymous expressions in argument
   positions.** `iter.map(|x| ...)` produces `["fn_name",
"iter.map", "<arg-0>"]`. Stable across cosmetic source edits.
3. **Line/column fallback** for inline anonymous expressions not
   in argument positions: `<closure@42:5>`.

The discipline is: **prefer positional-among-siblings over
absolute-line/column** so that adding a comment at the top of a
file doesn't invalidate every cache key in it (relevant to D7).

#### Fractal scoping: container-kind determines internal visibility

The path-based identifier handles arbitrarily-nested decls
uniformly. Scope rules layer on top, _per container kind_:

| Container kind     | Internal visibility                                                                     |
| ------------------ | --------------------------------------------------------------------------------------- |
| Package            | Package-level decls visible within package; external requires qualification or alias.   |
| Type (struct/enum) | Members (fields, methods, nested types) externally accessible via parent (`User.Role`). |
| Function           | _Nothing externally accessible._ Locals, type defs, nested fns — all internal.          |

Each container _kind_ has one consistent rule applied to _all_
its internals. Functions don't pick-and-choose ("locals private
but nested types public"); they uniformly hide. Types uniformly
expose.

Consequence: `TypeIdentifier { package, path: ["User", "Role",
"validate", "TempTable"] }` exists as a structurally-valid
identifier (the closure pass and codegen mangling can use it),
but **typecheck's name-resolution rules refuse to resolve that
path from outside `validate`'s body**. Identity and resolvability
are separate concerns: identity is global, resolvability is
scoped.

(Strict-escape commitment for fn-local types — they cannot escape
as values either — is recorded as a language-semantics note
under "Other notes" below; it is adjacent to D9, not part of it.)

#### What this kills in `lower/types.rs`

Concrete deletions (4 of 6 functions):

| Function               | Disposition                                                                                                                                                        |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `find_type_current`    | Deleted. AST nodes carry `Resolution::Global(id)`; lower reads it.                                                                                                 |
| `resolve_name_current` | Deleted. Same.                                                                                                                                                     |
| `resolve_type_expr`    | Deleted. Every `TypeExpr` carries its resolved `Type` post-typecheck.                                                                                              |
| `id_for`               | Deleted. The fallback exists only because typecheck doesn't always populate; after D9 it always does.                                                              |
| `monomorphize_type`    | **Stays** in koja-ir (per-fn substitution under specialization context, not name resolution). Renamed for clarity (`substitute_in_current_fn_context` or similar). |
| `type_name_from_expr`  | Trivial utility, no architectural concern.                                                                                                                         |

#### `seal_ast` runtime check

```rust
pub fn seal_ast(ast: &Ast) -> Result<(), SealViolation>
```

Walks every AST node, asserts every relevant resolution
annotation is `Some(_)` and every `Expr.resolved_type` is
populated. Mirrors `seal_program` from D1/D6.

The seal applies on the **typecheck-success path**: `typecheck OK
→ seal_ast → koja-ir`. Typecheck-_failure_ ASTs have `None`
annotations on partially-resolved nodes; they're consumed by the
LSP best-effort and never enter the lowering pipeline. The LSP
never calls `seal_ast`; it operates on whatever annotations are
populated.

This separation is load-bearing. A `seal_ast` that asserted at
the LSP boundary would break editing. The seal is the contract
for _lowering input_, not for "every AST in the system."

#### Hard rules

1. **Annotations record decisions, not derivations.** If
   something can be computed from other annotations + the source,
   don't store it.
2. **No backend handles in the AST.** Resolution annotations
   point to typecheck-layer concepts (`TypeIdentifier`, `Type`).
   Never `IRInstructionId`, `LLVMValueRef`, etc. AST is for
   user-view; IR/codegen are for compiler-view.
3. **One annotation per decision kind per node.** No two fields
   storing the same fact in different shapes.
4. **Typecheck writes; consumers read.** Annotations populated
   by typecheck and immutable thereafter. Lower never mutates;
   LSP never mutates.
5. **Adding a new annotation field requires justification** — a
   use case existing fields can't satisfy. Soft rule (single
   maintainer); a forcing function to keep the AST lean.

#### Stable IDs across builds (G2)

Satisfied by construction:

- `TypeIdentifier` paths are derived from source positions
  (lexical containment + naming policy above). Stable per source.
- Cross-package references stay valid across cache hits because
  package B's reference to `A.identity` is just `TypeIdentifier
{ package: A, path: ["identity"] }` — no opaque counter that
  could shift between A's rebuilds.
- `LocalId` (when added later as a memory optimization) is "index
  in encounter order during typecheck of this function's body" —
  deterministic given deterministic AST traversal.

D7's per-package caching works without any ID-stability
contortions.

#### Generic-bound forms (G3)

Inside `fn map<T, U>(items: List<T>, f: fn(T) -> U)`, the call
`items.iter()` annotates with a path whose receiver segment is a
type parameter: roughly, the resolved method is "the `iter`
method on whatever T's `Iterable` impl is." Expressible as a path
form like `Resolution::Global(TypeIdentifier { package: ...,
path: ["map", "T", "iter"] })` — first segment names the type
parameter binding (a path under the enclosing fn), the rest
chains as normal.

Specialization (in koja-ir) rewrites generic-bound forms to
concrete forms during monomorphization: `["map", "T", "iter"]`
becomes `["List", "iter"]` when T = List. The closure pass and
planners are exactly the right home for this rewrite.

This isn't a _separate_ annotation shape; it's the same
identifier shape, with type-param segments resolved later. No
special enum variant needed.

#### Migration

D9 is the largest single migration in the northstar — it adds
fields to AST nodes, requires typecheck to populate them, and
requires migrating all the lookup callers in koja-ir. Sequenced
**after** D1's loud-failure migration completes, so the
`koja-ir → koja-codegen` boundary is solid before retargeting at
the `typecheck → koja-ir` boundary.

Strangler-fig sequence:

1. Extend `TypeIdentifier`'s `name: String` to `path:
Vec<String>` (mechanical rename + update construction sites).
2. Define `Resolution::Global(TypeIdentifier)` as a single-variant
   enum.
3. Add `resolved: Option<Resolution>` fields to AST node kinds
   (default `None`); `Expr.coercion: Option<Coercion>` per D8.
4. Tighten typecheck to populate annotations, one node kind per
   slice. Each slice ships independently.
5. As each node kind reaches "always populated post-typecheck-
   success," simplify the corresponding lower code to read the
   annotation; delete the corresponding fallback in
   `lower::types.rs`.
6. Define `seal_ast`; flip lower's contract to require sealed
   input; ship the assertion as panic-on-violation per D1.
7. Move `koja-typecheck` two-pass structure (collect decls →
   typecheck bodies) into explicit phases if not already, so
   forward references resolve correctly when populating
   annotations.

#### Forward note: `my_package` → `MyPackage`

The eventual rename of snake_case packages to PascalCase makes
the visual story `Package.Type.Member` consistent regardless of
nesting depth. It motivates the path-based identifier shape: the
lookalike treatment of packages and types feels intentional
rather than coincidental. Doesn't block anything; recorded so
future-AI doesn't see the rename as a structural change later.

### D10. Linear pipeline; preprocess collapses into typecheck as sub-passes

The pipeline is a strict linear sequence between sealed outputs:

```text
Parse → Typecheck → koja-ir → koja-codegen
```

Each arrow either crosses a sealed boundary (the latter two) or
hands off raw parser output (the first). There is no top-level
"preprocess" phase. AST transformations that today live in
preprocess — whether semantic (default protocol impl generation,
which needs type info) or pre-checking-but-syntactic
(`@cfg` stripping, which removes nodes before any checking
happens) — become **sub-passes within typecheck**, ordered by
their data dependencies.

This applies the same fractal pattern that D6 established for
koja-ir to typecheck: each pipeline-level phase has internal
multi-pass machinery converging on a sealed output; pipeline
boundaries are between _sealed outputs_, not between _kinds of
work_.

The crate name `koja-typecheck` stays (renaming is mechanical
churn for limited gain), but the conceptual phase is **semantic
analysis** (or "the frontend") since typecheck is now its primary
but not sole responsibility. There's even a defensible argument
that `@cfg` stripping _is_ a form of type checking — the build
configuration acts like a type-level value that determines which
code exists in the current build's "world." Wherever you draw
the conceptual line, the work belongs in the semantic-analysis
phase.

#### Sub-pass order

```text
koja-typecheck (semantic-analysis phase):
  strip-cfg     -> remove @cfg-excluded nodes; no type info needed
  collect       -> register all surviving top-level decls; assign
                   TypeIdentifier per D9
  synthesize    -> generate AST for default protocol impls (Debug,
                   etc.) for surviving types only
  resolve       -> walk all bodies (user-written + synthesized);
                   populate D9 annotations (Resolution::Global,
                   resolved_type, ...)
  check         -> validate type compatibility everywhere
  annotate      -> populate any remaining D9 annotations
                   (coercion, ...)
  seal          -> assert sealed AST invariants per D9; output
```

The order is forced by data dependencies, not preference:

- strip-cfg first because everything else operates only on
  surviving code.
- collect after strip-cfg so excluded decls don't get registered.
- synthesize after collect so it has type info to generate bodies.
- resolve after synthesize so it can resolve names in synthesized
  bodies the same way as user-written ones.
- check after resolve so it has resolved types to validate.
- annotate after check so it knows resolution succeeded.
- seal last to assert the contract.

#### Mechanical commitments

- **Pipeline shape.** `Parse → Typecheck → koja-ir → koja-codegen`.
  Four phases. Two seal-asserted handoffs (typecheck → koja-ir,
  koja-ir → koja-codegen). Parser output is raw AST; first thing
  typecheck does is its own internal sub-passes culminating in a
  seal.
- **Top-level preprocess crate is abolished.** Whatever it does
  today moves into `koja-typecheck` under the appropriate
  sub-pass. During migration the crate may stay temporarily with
  responsibilities tagged for relocation; long-term it disappears.
- **Synthesized AST nodes are first-class.** Once synthesize
  produces them, the resolve / check / annotate / seal sub-passes
  treat them identically to user-written nodes. No "this is a
  synthesized node" branching anywhere downstream of synthesize.
- **One owner of all AST mutation.** After D10, `koja-typecheck`
  owns every AST mutation between parse and seal. Downstream
  consumers (koja-ir, LSP, formatter, doc-gen) read only.

#### Decision procedure for new AST transformations

When a new transformation is proposed, find its slot mechanically:

1. Does it remove nodes that should never be checked?
   → strip-cfg time (first sub-pass).
2. Does it need type info?
   → after collect (synthesize time or later).
3. Does it produce nodes that themselves need checking?
   → before resolve.
4. Does it touch invariants the seal asserts?
   → it doesn't belong in typecheck; it belongs in koja-ir or
   later.

This is mechanical. No architectural debate required.

#### Why this works

- **Linear pipeline is a load-bearing simplification.** Four
  phases, two seals, fits in your head. Adding intermediate
  phases would require establishing new seal contracts at each
  new boundary, which compounds.
- **Fractal symmetry between phases.** Both typecheck and koja-ir
  internally do collect-style → transformation(s) → check-style →
  seal. Same shape, different vocabularies. Future-AI sees one
  pattern, not two.
- **No phase-ordering decisions to relitigate.** "Where does this
  go?" gets a mechanical answer per the decision procedure.
- **`@cfg`-excluded code never reaches checking.** A bare-minimum
  correctness requirement that comes free from putting strip-cfg
  first.

#### What this rules out

- Top-level preprocess phase as a long-term architectural element.
- Cross-phase mutation. Each phase consumes its sealed input and
  produces its own sealed output; it does not mutate upstream
  data.
- New top-level pipeline phases without deletion of an existing
  one. The four-phase shape is the commitment; if a new
  responsibility doesn't fit any existing phase's seal contract,
  the question is "which existing phase grows to accept it,"
  not "do we add a fifth top-level phase."
- Synthesize-time work that _also_ depends on check results
  (i.e. transformations that need post-check info to generate new
  nodes that themselves need checking). Such work would create a
  cycle within typecheck. If a real use case demands this, the
  fix is to iterate (synthesize → resolve → check → synthesize
  → resolve → check → ...) until fixed point — same convergence
  pattern koja-ir uses for monomorphization. Cross that bridge
  if/when it appears; today's needs (strip-cfg, default impls)
  don't.

#### Migration

Mostly mechanical relocation:

- Debug-impl generation moves from preprocess into typecheck's
  synthesize sub-pass.
- `@cfg` stripping (when implemented) lands in typecheck's
  strip-cfg sub-pass.
- Making typecheck's internal phases explicit is a refactor for
  clarity; today they're probably implicit in the typecheck
  driver's call order. The external contract ("give me an AST,
  get back a sealed annotated AST or errors") doesn't change.

Future-AI guidance: when adding a new AST transformation, walk
the decision procedure. When adding a new IR transformation, the
equivalent procedure for koja-ir (per D6) applies. If you find
yourself wanting a fifth top-level pipeline phase, stop — the
answer is almost always a new sub-pass within an existing phase.

## Other notes captured along the way

### Background on architectural inspiration

Swift / SIL is the design inspiration for the IR (not Rust / MIR).
Implication: the IR should be _one_ representation with progressively
tighter invariants enforced by passes (Swift's "raw SIL → canonical
SIL → lowered SIL" pattern), rather than multiple distinct IRs with
1:1 translators between them.

Considered and rejected: adding a polymorphic call instruction to
the IR (Swift-style `apply %f<Int>`). Rationale for rejection:
correctness today is fine (lazy backfill catches misses); the actual
problem is _clarity_ and _triplicated discovery_, not missing IR
expressiveness. The fix is the seal assertion + closure-pass
authority (D1), not new IR constructs.

### Language semantics the architecture assumes

These are language-level rules that aren't compiler-architecture
decisions per se, but the architecture (D9 in particular) assumes
they hold. Recorded here so future-AI doesn't see them as
implementation details that can be relaxed without consequence.

**Strict fn-local type escape.** Types declared inside a function
(e.g. `struct TempTable` inside `fn validate`) cannot escape that
function — neither as type references in external code, nor as
values returned/passed to external code. If you need a type to be
referenced from outside, hoist it to package level.

This is the fractal-correct consequence of "function bodies hide
their internals uniformly across kinds of internals" (per D9's
fractal-scoping section). Locals can't escape; nested types
can't escape; nested fns can't escape. The rule is the same.

What this rules out:

- Returning a value of a fn-local type from the enclosing fn
  (even via `Any` or via casting to a package-level protocol the
  fn-local type implements). If you need to return something with
  type identity, the type must be at package level.
- Naming a fn-local type from outside the function via path
  qualification. The path exists structurally; resolution
  refuses it.

What this preserves:

- The fn-local type can be used freely _inside_ the function and
  any nested fns/closures defined within it (they're inside the
  same hiding boundary).
- Codegen still emits the fn-local type as a regular nominal
  type at the LLVM level (the language rule is a typecheck-layer
  scope rule, not an IR-layer one).

This is a language-design commitment, separable from D9's
compiler-architecture commitments. If a real use case demands
opaque-existential escape later (Swift's `some Protocol`, Rust's
`impl Trait`), it can be added without breaking D9.

### The Slice 3 work that triggered this conversation

The closure-pass extensions added in the most recent slice are on
the working branch but landed without producing the bail-count
drops the plan claimed:

- `lower_static_call_or_stub pending-type-mono`: 63 → 63 (no change;
  bail is structurally undrainable by closure pass because the IR
  lifter's `type_mono_exists` callback is hard-wired `false`).
- `lower_method_call_or_stub pending-mono`: 118 → 140 (worse; the
  closure pass extensions are registering things but new
  monomorphized bodies introduce more sites that themselves bail).
- `lower_call_or_stub pending-mono`: 9 → 9 (unchanged; all in
  `__koja_user_main` whose body the closure pass cannot see).

The Phase 0 substitution-threading change (passing `IRFunction.subst`
through the visitor) is genuinely useful plumbing for any future
closure-pass work and is worth keeping. The Phase 1/2 discovery
extensions should probably be reverted once the northstar lands and
the work is restructured around the seal assertion.

This is recorded here as data, not as a directive — what to do with
the working branch is a separate decision.

## Next questions to settle

- Q9: design of `monitor()` for typed processes. Must be
  expressible without runtime expansion of M. Tentative options
  (system-event channel pattern, static union declaration, typed
  wrapper) all compatible with prior decisions. Punted to future
  language-design session.

With Q1–Q8 settled, the load-bearing architectural questions are
resolved. Q9 is a language-design follow-up with no impact on
the compiler-architecture northstar.
