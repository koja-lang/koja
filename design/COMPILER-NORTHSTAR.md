# Compiler Northstar

The destination architecture for the Koja compiler. This is **not** a
trajectory doc; it describes the system as it should exist, not how
to get there from where it is today.

Every claim in this doc reduces to a mechanical check (a grep, an
assertion in a `seal_*` function, a test, a deletable file). If a
section feels aspirational, it is wrong and needs to be tightened.

For the rationale and design conversation that produced this doc,
see `archive/20260502-COMPILER-NORTHSTAR-QA.md`.

## Pipeline shape

```text
                  (sealed AST + GlobalRegistry)    (sealed IRProgram)
koja-parser → koja-typecheck ──────────────────→ koja-ir ──────────────→ koja-ir-llvm
                                                                       ↘ koja-ir-eval
```

Five crates (counting both backends), four logical phases. Two
seal-asserted handoffs (`koja-typecheck → koja-ir`, `koja-ir →
backend`). The `koja-parser → koja-typecheck` handoff is raw AST;
the first sub-pass `koja-typecheck` runs is its own internal work
on that raw AST.

(`koja-lexer` exists as an implementation detail of `koja-parser`
and is not noted in the pipeline; it is not a phase boundary.)

Each pipeline-level phase is a single logical phase that internally
runs sub-passes converging on a sealed output. Pipeline boundaries
are between **sealed outputs**, not between **kinds of work**. New
transformations slot into existing phases as sub-passes; they do not
add new top-level phases.

Backends consume the same sealed `IRProgram`; they are siblings, not
sequential.

## Glossary

Definitions used throughout this doc.

- **Sealed AST.** The AST half of the typecheck phase's sealed output
  on the typecheck-success path. Every `Expr.resolution` is populated
  to a resolved `ResolvedType` (seal asserts `resolution.is_resolved()`
  recursively across heads and type args). The sealed AST does not
  travel alone — see **Sealed substrate** below.
- **Sealed substrate.** The typecheck → ir handoff is the pair
  `(sealed AST, GlobalRegistry)`, wrapped together as
  `CheckedProgram`. The AST carries `Resolution::Global(GlobalRegistryId)`
  handles that dereference into the registry; the registry holds the
  canonical decl metadata (signatures, lifted struct / enum payloads,
  type-param bindings) that those handles point to. Splitting them
  would force ir to re-derive the registry from sealed annotations,
  which is pure duplication of typecheck work. The registry travels
  with the AST as a sealed, read-only input — not as a shared mutable
  brain.
- **`GlobalRegistry`.** The typecheck-side registry of every
  globally-named decl. Indexed by `GlobalRegistryId` (opaque,
  sequential `u32` today). Built during the `collect` and
  `lift_signatures` sub-passes, frozen at seal, then consumed by
  ir (and the LSP) as part of the sealed substrate.
- **Sealed `IRProgram`.** The output of the koja-ir phase. No
  `Stub` instructions; every callee is resolved to a registered
  symbol; every coercion has been emitted as an explicit
  `IRInstruction`. Asserted by `seal_program`.
- **Closure pass.** The whole-program walk in `koja-ir/src/closure/`
  that discovers which generic instantiations the source actually
  references and registers them via the planners.
- **Planners.** The `monomorphize_*` functions in
  `koja-ir/src/lower/monomorphize.rs`. Take a generic decl + a
  type-arg vector and append a specialized `IRStruct` / `IREnum` /
  `IRFunction` to `IRProgram`. LLVM-free; idempotent.
- **`IRPackage`.** A per-package fragment produced by source
  lowering. The unit of incremental cache.
- **`Identifier`.** Globally-unique handle for any bound thing in
  the program (`{ package: String, path: Vec<String> }`, fields
  private, accessed via `package()`, `path()`, `qualified_name()`,
  `is_in_package`, `is_in_global`). Identifiers live inside the
  `GlobalRegistry`; AST nodes carry `GlobalRegistryId` handles, not
  bare `Identifier` clones.
- **`Resolution`.** AST annotation type with four variants:

  ```rust
  pub enum Resolution {
      Global(GlobalRegistryId),
      Local(LocalId),
      TypeParam { owner: GlobalRegistryId, index: TypeParamIndex },
      Unresolved,
  }
  ```

  `Global` indexes into the `GlobalRegistry`; `Local` indexes into
  the enclosing function's `LocalScope`; `TypeParam` references a
  generic decl's type parameter slot. `Unresolved` is the default
  (pre-resolve) state. Each variant carries an opaque handle whose
  numeric derivation is an implementation detail.

## Phase 1: `koja-parser`

Input: source text (per file).
Output: raw AST (per file).

`koja-parser` produces an AST faithfully reflecting the source. No
semantic analysis. No name resolution. No type inference. No
synthesis.

`koja-parser` does not fail-fast on semantic errors that aren't
syntactic; those are `koja-typecheck`'s job.

## Phase 2: `koja-typecheck` (the semantic-analysis phase)

Input: raw AST + build configuration (target, cfg flags, etc.).
Output: `CheckedProgram` — the sealed pair (sealed AST +
`GlobalRegistry`). Every relevant AST node carries a resolved
`Resolution` and `resolution: ResolvedType`; the registry holds the
canonical decl metadata that those handles dereference into.

Crate name `koja-typecheck` for implementation continuity. The phase
is conceptually broader than its name — it does name and type
resolution, lifting of signatures and lifted struct / enum / protocol
payloads onto the registry, surface-shape synthesis, type checking,
annotation, and sealing. There is no separate "preprocess" phase.

### Sub-pass order

```text
koja-typecheck:
  collect          -> two passes across all files. collect_file_decls
                      registers named decls (types, functions, etc.)
                      into the GlobalRegistry. collect_file_impls runs
                      after every decl is registered and binds impl
                      blocks. The two-pass shape lets impls reference
                      types declared in later files.
  lift_signatures  -> stamp FunctionSignatures and lifted struct /
                      enum / protocol payloads on the registry.
  synthesize       -> surface-shape AST rewrites (today: `for` desugar).
                      Default protocol impl bodies (Debug, etc.) are
                      derived at monomorphization time by the codegen
                      backends, not pre-baked as a typecheck sub-pass.
  resolve          -> walk all bodies; populate Resolution and
                      resolution: ResolvedType on every relevant node;
                      validate type compatibility; annotate coercion
                      sites. Check, annotate, and resolve are folded
                      into one walk rather than separate sub-passes.
  seal             -> assert sealed-substrate invariants per seal_ast.
```

Order is forced by data dependencies, not preference. (cfg-stripping
is not yet implemented as a sub-pass; when it lands it slots ahead of
`collect` so excluded nodes never reach the registry.)

### Decision procedure for new AST transformations

When a new transformation is proposed, find its slot mechanically:

1. Does it remove nodes that should never be checked?
   → ahead of collect (the future strip-cfg slot).
2. Does it need to register new decls?
   → at collect time (decls phase or impls phase, depending on shape).
3. Does it need a registered signature?
   → at lift_signatures time or later.
4. Does it need type info on bodies?
   → at synthesize time or later.
5. Does it produce nodes that themselves need resolution?
   → before resolve.
6. Does it touch invariants the seal asserts?
   → it doesn't belong in typecheck; it belongs in koja-ir or later.

### What typecheck owns

- All AST mutation between parse and seal.
- Name resolution for every identifier.
- Type resolution for every type expression.
- Overload / dispatch resolution for every call.
- Coercion annotation on every site that requires one.
- Surface-shape synthesis (today: `for` desugar).
- Registration of every top-level decl in its own
  `GlobalRegistry`.

### What typecheck does not own

- Generic specialization (monomorphization). Generic decls remain
  in the sealed AST with type-parameter references; koja-ir
  specializes them.
- Default protocol impl bodies (Debug, etc.). The registry knows
  which protocols a type conforms to, but the synthesized impl
  bodies are produced at monomorphization time by the codegen
  backends — not pre-baked into the sealed AST.
- Any IR construction.
- Any awareness of LLVM types or backend concerns.

### Seal contract

```rust
pub(crate) fn seal_ast(program: &CheckedProgram)
```

Walks every AST node and asserts `Expr.resolution.is_resolved()`
recursively, and asserts every `RegistryEntry` in the
`GlobalRegistry` is complete. Panics on violation — the seal is a
hard invariant, not advisory.

The seal applies on the **typecheck-success path**: typecheck OK →
`seal_ast` → koja-ir. Typecheck-_failure_ outputs (partial
annotations, diagnostics attached) are consumed by the LSP
best-effort and never enter the lowering pipeline. The LSP never
calls `seal_ast`.

## Phase 3: koja-ir (the lowering phase)

Input: `CheckedProgram` — the sealed (AST + `GlobalRegistry`) pair
from typecheck.
Output: sealed `IRProgram`.

### Sub-pass order

```text
koja-ir:
  collect-package -> per-package source-lowering produces an IRPackage
                     fragment (cacheable per package per the
                     incremental-compilation section)
  merge           -> merge IRPackages into a single working IRProgram
  closure         -> whole-program walk discovers required generic
                     instantiations; planners register them in IRProgram
  elaborate       -> later refinements (currently empty; reserved for
                     future passes)
  seal            -> assert sealed-IRProgram invariants per seal_program
```

### Pipeline entry points

```rust
pub fn lower_program(
    checked: &CheckedProgram,
    entry_state: &Identifier,
) -> Result<IRProgram, LowerError>;

pub(crate) fn lower_package(
    pkg: &CheckedPackage,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRPackage;
```

`lower_program` is the single public entry point — it owns the
package-by-package walk, the closure pass, the merge, and the
seal. `lower_package` is `pub(crate)`; it is called once per
package from inside `lower_program` and produces a per-package
fragment. Per-package source-lowering cacheability is the design
intent (the function is pure with respect to its inputs); the
on-disk cache layer is not yet implemented and lives behind the
`lower_program` shell today.

### What koja-ir owns

- Translation of sealed AST to IR instructions.
- Generic specialization (the closure pass discovers; planners
  produce specialized decls).
- All coercion emission. Each `Coercion` annotation in the AST
  becomes an explicit `IRInstruction` (today: `Coercion::UnionWiden`
  → `IRInstruction::UnionWrap`).
- Construction of the entry-point reference in the resulting
  `IRProgram`.

### What koja-ir does not own

- Name resolution (typecheck did it; koja-ir reads annotations).
- Type resolution (typecheck did it).
- Overload resolution (typecheck did it).
- Coercion _decisions_ (typecheck made them; koja-ir emits the
  corresponding instructions).

### `lower::*` helpers contract

The free functions in `koja-ir/src/lower/` are **pure translators**
under this architecture:

- They do not discover generics or bail on unregistered symbols.
- They panic on lookup misses. Misses are `seal` violations
  upstream, not recoverable conditions in the helpers.
- They have no `Ok(None) → Stub` paths. `IRInstruction::Stub` does
  not exist in the sealed IR.

### Seal contract

```rust
pub(crate) fn seal_program(program: &IRProgram)
```

Implemented in `koja-ir/src/seal/program.rs` as a top-level walk
that delegates to focused helpers (`seal_program_entry_wrappers`,
`seal_program_calls`, `seal_program_closure_ops`,
`seal_program_enum_ops`, `seal_program_struct_ops`,
`seal_program_loadconst_pool`). Together they assert:

- No `IRInstruction::Stub` anywhere.
- Every callee in `IRInstruction::Call` and friends resolves to a
  registered symbol in `prog.functions`.
- Every operand graph is free of `Coercion` metadata (all coercions
  emitted as instructions).
- The entry point referenced in `prog.entry_point` resolves to a
  registered function.

`seal_program` panics on violation; it is not advisory.

## Phase 4a: koja-ir-llvm (LLVM emission)

Input: sealed `IRProgram`.
Output: LLVM IR / object code.

### What koja-ir-llvm owns

- Translation of each `IRInstruction` to its LLVM emission pattern.
- koja-ir-llvm builds **its own indices** over the sealed
  `IRProgram` (mangled-name → `FunctionValue`, struct → LLVM type,
  etc.). It does not import any koja-ir or koja-typecheck
  side-tables.

### What koja-ir-llvm does not own

- Any monomorphization. There is no lazy backfill. There is no
  `lazy_mono_count`. If a callee resolves to a symbol not present
  in `IRProgram.functions`, that is a bug upstream — codegen panics
  with a clear "missing symbol" diagnostic.
- Any interpretation of `Coercion::*`. The backend does not import
  `Coercion`. Each `IRInstruction` has a single direct LLVM
  emission pattern.
- Any imports of `koja-typecheck`. The sealed `IRProgram` is the
  full input contract.

## Phase 4b: koja-ir-eval (interpreter)

Input: sealed `IRProgram`.
Output: side effects from running the program (or returned values
in REPL mode).

Sibling to `koja-ir-llvm`, not sequential. Both backends consume
the same sealed `IRProgram` and follow the same pattern: each
`IRInstruction` has one direct emission/interpretation; no
fallback paths; no awareness of `Coercion::*`.

REPL debugging features (e.g. dynamically inspecting running
processes) are interpreter-binary machinery built on top of sealed
IR. They are not IR features and do not introduce any
`IRInstruction::Dynamic*` constructs.

## Cross-cutting concerns

### Identifiers and resolution

One identifier type — globally-named decls only:

```rust
pub struct Identifier {
    package: String,        // private
    path: Vec<String>,      // private; lexical containment chain
}
```

Fields are private and accessed via the public surface (`package()`,
`path()`, `qualified_name()`, `is_in_package`, `is_in_global`). The
path is the lexical containment chain. Examples:

| Decl                       | `path`                                      |
| -------------------------- | ------------------------------------------- |
| Top-level struct `User`    | `["User"]`                                  |
| Top-level fn `validate`    | `["validate"]`                              |
| Nested struct `User.Role`  | `["User", "Role"]`                          |
| Method on `Role`           | `["User", "Role", "validate"]`              |
| Struct defined inside a fn | `["User", "Role", "validate", "TempTable"]` |
| Enum variant               | `["Color", "Red"]`                          |
| Field on a struct          | `["Circle", "radius"]`                      |

`Identifier` is for globally-named decls. Locals and type parameters
use different handles (`LocalId`, `TypeParamIndex`) — see
[`Resolution`](#resolution) below.

Identifiers live inside the `GlobalRegistry`. AST nodes do not carry
bare `Identifier` clones — they carry `GlobalRegistryId` handles
that dereference into the registry. This keeps annotation sites
small and equality cheap; the registry is the single source of
truth for the underlying string-shaped data.

Type args remain _separate_ from the identifier and compose at use
sites: `List<Int>` is rendered on the AST as a `ResolvedType::Named`
holding a head `Resolution::Global(list_registry_id)` plus a
`type_args` vector containing the `ResolvedType::Named` for `Int`.

#### Resolution

```rust
pub enum Resolution {
    Global(GlobalRegistryId),
    Local(LocalId),
    TypeParam { owner: GlobalRegistryId, index: TypeParamIndex },
    #[default]
    Unresolved,
}
```

- `Global` indexes into the `GlobalRegistry`.
- `Local` indexes into the enclosing function's `LocalScope` (minted
  when parameters or `let`-introduced names enter scope). `LocalId`
  does not cross the IR boundary; koja-ir defines a sibling
  `IRLocalId` and translates one-to-one at lower time.
- `TypeParam` references one of an owning generic decl's type
  parameter slots. Seal asserts this variant only appears inside
  generic-decl bodies.
- `Unresolved` is the in-flight default state before resolve runs.

Each variant carries an opaque handle. Callers do not synthesize
handles by hand and do not reason about their numeric values.

#### Naming policy for synthesized path segments

In priority order:

1. Source-given when available.
2. Call-site-relative for anonymous expressions in argument
   positions: `iter.map(|x| ...)` → `["fn_name", "iter.map",
"<arg-0>"]`. Stable across cosmetic source edits.
3. Line/column fallback for inline anonymous expressions not in
   argument positions: `<closure@42:5>`.

Prefer positional-among-siblings over absolute-line/column so that
adding a comment at the top of a file doesn't invalidate every
cache key in it.

#### Stable IDs across builds

`Identifier` paths are derived from source positions (lexical
containment + naming policy), so they are stable across cosmetic
edits. `GlobalRegistryId` handles are stable within a build via the
registry; today they are sequential `u32`s assigned at insertion
time, with content-addressable derivation reserved as a future swap
that does not change the public surface (`GlobalRegistryId` is
opaque). Cross-package references stay valid across cache hits
because the registry roots them in package-qualified `Identifier`s.

#### Annotations live on AST nodes

Not in a parallel side-table. Choices:

- LSP reads annotations directly from AST nodes the cursor is on.
- Per-package source-lowering caching (see incremental section)
  serializes the annotated AST as one unit.
- The "stale side-table" failure mode is structurally impossible.

### Hard rules for AST annotations

1. Annotations record _decisions_, not _derivations_. If something
   can be computed from other annotations + the source, do not
   store it.
2. No backend handles in the AST. Annotations point to typecheck-
   layer concepts (`Identifier`, `Type`). Never `IRInstructionId`,
   `LLVMValueRef`, etc.
3. One annotation per decision kind per node.
4. Typecheck writes; consumers read. Annotations are immutable
   after typecheck.
5. Adding a new annotation field requires a use case existing
   fields cannot satisfy.

### Coercions

`Coercion` (defined in `koja-ast/src/coercion.rs`) is a
typecheck-layer vocabulary. It records "the value at this site
needs widening / wrapping / conversion before it can flow to its
consumer."

- One `IRInstruction::*` variant per `Coercion::*` variant
  (today: `Coercion::UnionWiden` ↔ `IRInstruction::UnionWrap`).
- The lowerer translates every `Coercion` annotation into the
  corresponding `IRInstruction` at the exact site.
- The sealed `IRProgram` contains zero `Coercion` metadata on any
  operand.
- `koja-ir-llvm` and `koja-ir-eval` do not import `Coercion`.
- Adding a new `Coercion` variant requires adding the paired
  `IRInstruction` and lowerer emitter in the same change.

A parallel `LiteralCoercion` annotation family lives alongside
`Coercion` in the same module. It handles per-expression numeric
literal width fitting (`UInt8 = 4` minting the const at `u8`
width rather than the default `i64`). Width fitting is structurally
distinct from value-conversion coercion: it changes the materialized
constant, not the data-flow shape. See the module doc in
`koja-ast/src/coercion.rs` for the full rationale.

### Polymorphism

Purely compile-time. Every generic call site is monomorphized
during the closure pass; every callee in a sealed `IRProgram`
resolves to a concrete mangled symbol.

What this rules out:

- Heterogeneous collections of trait objects (`List<dyn Trait>`).
  Sum types only: `enum Shape { Circle(...) | Square(...) }`.
- Inheritance-based virtual method dispatch.
- `Any`-typed values / runtime type erasure as a language feature.
- Any IR construct for dynamic dispatch (`DynamicCall`,
  `PolymorphicCall`, virtual tables).

### Sealing pattern

The compiler uses three seal pass-throughs:

| Seal                    | Phase     | Asserts                                                                    |
| ----------------------- | --------- | -------------------------------------------------------------------------- |
| `seal_ast`              | typecheck | every `Expr.resolution` resolved; every `RegistryEntry` complete           |
| `seal_program`          | koja-ir   | no `Stub`; every callee registered; no `Coercion` metadata; entry resolved |
| (codegen/eval implicit) | backend   | one match arm per `IRInstruction`; no fallback paths                       |

The seal contract is `panic on violation`. Seal failures indicate
upstream bugs, not recoverable conditions.

The exception is the LSP path: the LSP consumes typecheck-failure
ASTs best-effort and never invokes `seal_ast`. The seal applies to
the success path only.

### Per-phase debug emitters

Each pipeline phase exposes its sealed output via a `--emit-*`
flag on the CLI command that produces it. The flag short-circuits
the command at the corresponding phase, writes the artifact to
stdout in a defined text format, and exits successfully.

Note: these may not be implemented yet.

Convention:

| Command      | Available `--emit-*`                                                     |
| ------------ | ------------------------------------------------------------------------ |
| `koja lex`   | `--emit-tokens`                                                          |
| `koja parse` | `--emit-ast` (raw AST; annotations all `None`)                           |
| `koja check` | `--emit-ast` (sealed AST; annotations populated)                         |
| `koja build` | `--emit-package`, `--emit-ir`, `--emit-llvm`, `--emit-asm`, `--emit-obj` |

The same artifact name (`--emit-ast` on both `parse` and `check`)
is intentional: same printer, different content. The seal state
is what differs.

Mechanical commitments:

- Output goes to stdout by default (`koja check --emit-ast | jq
...` is composable). `-o <file>` redirects to a file when the
  caller wants persistent output.
- Each emit short-circuits the command. `koja build --emit-ir`
  does not also produce a binary; if both are wanted, run the
  command twice. Avoids output-collision design.
- Format conventions per artifact: tokens are one-per-line; ASTs
  are pretty-printed S-expressions with annotations rendered
  inline on the typed variant; IR uses Swift-SIL-style text
  (function-by-function, basic blocks labeled, type-annotated
  operands); LLVM uses the existing `.ll` format.
- `--emit-package` and the per-package on-disk cache (per the
  incremental-compilation section) share the same serialization.
  One implementation, two purposes.

These emitters are a free affordance of the sealed-boundary
architecture, not a separate feature. Sealing makes mid-pipeline
state reproducible and complete; emit flags expose it. Third-
party tooling (alternative backends, static analyzers,
differential testers) can be built against any sealed emit
output without integrating into the compiler.

### Incremental compilation

Granularity: **package**. The unit of source-lowering caching is a
package (a collection of files sharing a namespace).

```rust
// Cacheable
pub fn lower_package(...) -> Result<IRPackage, ...>;

// Always reconstructed
pub fn lower_program(packages: &[IRPackage], ...) -> Result<IRProgram, ...>;
```

Per-package cache validity: a package P's cached fragment is valid
iff

- P's source files' content hash is unchanged, **and**
- the hashes of P's dependency packages' fragments are unchanged
  (or, a refinement, the hashes of their public-interface portions
  are unchanged — implementation detail).

Closure pass + seal always run whole-program over the merged
result. They are LLVM-free and cheap.

Codegen function-level caching is layered on top: each emitted LLVM
function is keyed by `(mangled_name, hash(IRFunction))`. The same
mangled body produces the same bytecode regardless of which
package or build triggered it. Out of scope for the initial
pipeline; the architecture supports it without further changes.

REPL semantics fall out: each input is a fragment that gets added
to the running session's "ad-hoc package." Re-run `lower_program`
over `[user_packages..., stdlib..., repl_session_package]`; hand
the resulting sealed `IRProgram` to eval.

### Consumer-builds-its-own-indices

The sealed substrate (the typecheck pair `(sealed AST,
GlobalRegistry)` for ir/LSP; the sealed `IRProgram` for backends)
does not ship **derived** indices. Each consumer builds its own
purpose-shaped lookups on top:

| Consumer          | Sealed input                  | Builds                                                                  |
| ----------------- | ----------------------------- | ----------------------------------------------------------------------- |
| `koja-ir-llvm`    | sealed `IRProgram`            | mangled-name → `FunctionValue`, struct → LLVM type, etc.                |
| `koja-ir-eval`    | sealed `IRProgram`            | mangled-name → interpreter handle, etc.                                 |
| `koja-ir` (lower) | sealed AST + `GlobalRegistry` | per-function substitution context, mono registry, IR symbol table, etc. |
| LSP               | sealed AST + `GlobalRegistry` | name → references, file → symbols, ID → def site, etc.                  |
| (future) tools    | sealed AST + `GlobalRegistry` | whatever they need                                                      |

`GlobalRegistry` is _not_ a derived index — it is part of the
typecheck phase's sealed output, alongside the AST. Consumers
read from it; they do not re-derive its contents from sealed
annotations (that would be pure duplication of typecheck work).
What they build for themselves are **backend-shaped derived
indices** (LLVM type maps, IR symbol tables, mono caches, LSP
file→symbol maps) keyed by the same `Identifier` / `GlobalRegistryId`
handles the registry uses.

### Entry points

Every compiled program's entry is a type implementing `Process`.
`lower_program` takes the entry state's `Identifier` and synthesizes
a `FunctionKind::ProcessEntryWrapper` that constructs and spawns the
state; `seal_program` asserts the entry resolves to that wrapper.
There is no `fn main` entry shape — the former `ProjectEntry` enum
(`Function` vs `Process`) was collapsed when `fn main` was removed
(2026-06-09). Scripts (`.kojs`) are the separate `IRScript` path:
top-level statements lower to a synthesized entry function executed
by the script trampoline.

The entry point is a property of `IRProgram`, not of any
`IRPackage`. Library packages built standalone produce an
`IRPackage` and stop; only executable builds construct an
`IRProgram`.

There is no dedicated `MainEntry` `FunctionKind`. A script's entry
function is a normal `FunctionKind::Regular`; its body is
reachable to the closure pass like any other function. The
`FunctionKind` enum reserves variants for the IR-internal shapes
that need different lowering (closures, externs, intrinsics,
process / spawn wrappers).

## Mechanical checks

The following are grep/assertion-checkable in CI:

- `koja-ir-llvm` does not import `koja-typecheck`. Grep:
  `rg "use koja_typecheck" koja/crates/koja-ir-llvm/`
- `koja-ir-llvm` does not call any `monomorphize_*` planner. Grep:
  `rg "monomorphize_" koja/crates/koja-ir-llvm/`
- No `lazy_mono_count` field or its increment sites exist.
- `IRInstruction::Stub` does not exist.
- No `Ok(None)` paths in `koja-ir/src/lower/` that fall through to
  `Stub` emission.
- `koja-ir-llvm/src/stmt.rs::apply_coercion` does not exist.
- `koja-ir-llvm` does not import `Coercion`.
- For every `Coercion::*` variant, a paired `IRInstruction::*`
  variant exists. Grep both enums and diff the variant lists.
- `seal_program` is called exactly once in the lowering pipeline,
  and panics on violation.
- `seal_ast` is called exactly once on the typecheck-success path,
  and panics on violation.
- `lower_program` is the single entry point for constructing an
  `IRProgram` from packages.

If any of these greps return matches when they shouldn't (or no
matches when they should), the architecture has drifted. Fix by
deletion or relocation, not by adding exceptions to the doc.

## Pattern: when adding new...

### A new AST transformation

Walk the typecheck decision procedure (Phase 2 above). It returns
a sub-pass slot mechanically. If it returns "doesn't belong in
typecheck," see the next section.

### A new IR transformation

Add it as a sub-pass in koja-ir, ordered between existing
sub-passes by data dependencies. If it must run after the closure
pass and before seal, it lands in elaborate. If it must run before
the closure pass, it lands earlier.

### A new `IRInstruction` variant

Add the corresponding emission patterns to both backends
(`koja-ir-llvm` and `koja-ir-eval`) in the same change. If the
instruction materializes a coercion, also add the paired
`Coercion::*` variant to typecheck and the staging emitter to the
lowerer.

### A new `Coercion` variant

Same change: add the paired `IRInstruction::*` variant and the
lowerer's staging emitter. Both backends gain emission patterns
for the new instruction. The variant should not require any code
change in `koja-ir-llvm` other than the new instruction's
emission pattern.

### A new top-level pipeline phase

Don't. Find the existing phase that should grow to accept the new
responsibility. The four-phase shape is the commitment; if a new
responsibility doesn't fit any existing phase's seal contract,
the architecture has missed something and needs revisiting at the
discussion level.

## Out of scope / non-goals

- **Dynamic dispatch IR constructs.** Polymorphism is fully
  monomorphized; the IR has no `dyn`, no vtables, no
  `PolymorphicCall`.
- **Lazy backfill in codegen.** Codegen panics on missing symbols;
  it does not invoke planners.
- **Multiple `IRProgram` types** (`RawIRProgram`,
  `MonomorphizedIRProgram`, `SealedIRProgram`). Single struct;
  passes mutate in place; seal is a runtime check at one
  well-defined seam.
- **A dedicated preprocess crate** as a top-level pipeline phase.
  Its responsibilities live as typecheck sub-passes.
- **Stale side-tables shared across phases.** Each phase produces
  its own sealed output (the typecheck phase's sealed output is the
  pair (sealed AST, `GlobalRegistry`); the ir phase's sealed output
  is `IRProgram`). Downstream consumers build their own derived
  indices over those sealed inputs; nothing they build mutates
  upstream data.
- **Cross-phase mutation.** Each phase consumes its sealed input
  and produces its own sealed output. It does not mutate upstream
  data.

## Language semantics this architecture assumes

These are language-level rules the compiler architecture relies
on. They are pinned here so the architectural commitments don't
get violated by language changes that look benign.

- **Strict fn-local type escape.** Types declared inside a function
  cannot escape that function — neither as type references in
  external code, nor as values returned/passed to external code.
  Hoist to package level if you need external visibility.
- **Container-kind determines internal visibility.** Packages
  expose package-level decls within the package, requiring
  qualification or alias from outside. Types expose members
  (fields, methods, nested types) externally via the parent.
  Functions hide everything.
- **No runtime type unions outside compile-time-known sets.**
  Process message-type unions, monitor() interactions, and
  anything that might suggest runtime expansion of M must be
  expressible without re-typechecking the program. (Future work:
  Q9 from the discussion archive.)

## Reference

Discussion that produced this doc:
`archive/20260502-COMPILER-NORTHSTAR-QA.md`.

Existing related docs:

- `../LANGUAGE.md` — language specification.
- `archive/20260610-PACKAGE.md` — cookbook distribution model (archived; the roadmap has since committed to a conventional package manager).
- `TYPES.md` — type system.

Predecessor docs (archived; preserved for historical context, not
authoritative):

- `archive/20260502-COMPILER.md` — earlier self-hosted compiler
  architecture sketch (Phase 6A material; will be revisited under
  the northstar when self-hosting begins).
- `archive/20260502-EXPOIR-ROADMAP.md` — superseded KojaIR
  roadmap.
- `archive/20260427-EXPOIR.md` — original SIL-style design notes.

When this northstar conflicts with an older doc, the northstar
wins; the older doc is superseded for the topic in question and
should be updated or archived.
