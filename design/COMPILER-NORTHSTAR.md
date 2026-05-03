# Compiler Northstar

The destination architecture for the Expo compiler. This is **not** a
trajectory doc; it describes the system as it should exist, not how
to get there from where it is today.

Every claim in this doc reduces to a mechanical check (a grep, an
assertion in a `seal_*` function, a test, a deletable file). If a
section feels aspirational, it is wrong and needs to be tightened.

For the rationale and design conversation that produced this doc,
see `archive/20260502-COMPILER-NORTHSTAR-QA.md`.

## Pipeline shape

```text
                        (sealed AST)         (sealed IRProgram)
expo-parser → expo-typecheck ────────→ expo-ir ──────────────→ expo-codegen
                                                             ↘ expo-ir-eval
```

Five crates (counting both backends), four logical phases. Two
seal-asserted handoffs (`expo-typecheck → expo-ir`, `expo-ir →
backend`). The `expo-parser → expo-typecheck` handoff is raw AST;
the first sub-pass `expo-typecheck` runs is its own internal work
on that raw AST.

(`expo-lexer` exists as an implementation detail of `expo-parser`
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

- **Sealed AST.** The output of the typecheck phase on the
  typecheck-success path. Every relevant resolution annotation is
  populated; every `Expr.resolved_type` is populated. Asserted by
  `seal_ast`.
- **Sealed `IRProgram`.** The output of the expo-ir phase. No
  `Stub` instructions; every callee is resolved to a registered
  symbol; every coercion has been emitted as an explicit
  `IRInstruction`. Asserted by `seal_program`.
- **Closure pass.** The whole-program walk in `expo-ir/src/closure/`
  that discovers which generic instantiations the source actually
  references and registers them via the planners.
- **Planners.** The `monomorphize_*` functions in
  `expo-ir/src/lower/monomorphize.rs`. Take a generic decl + a
  type-arg vector and append a specialized `IRStruct` / `IREnum` /
  `IRFunction` to `IRProgram`. LLVM-free; idempotent.
- **`IRPackage`.** A per-package fragment produced by source
  lowering. The unit of incremental cache.
- **`Identifier`.** The single identifier type
  (`{ package: String, path: Vec<String> }`). Identifies any bound
  thing in the program — types, functions, methods, fields,
  variants, locals, type parameters, anonymous closures.
- **`Resolution`.** AST annotation type
  (`enum Resolution { Global(Identifier), Unresolved }`). Single-variant
  today as compiler-checked migration prep for future variants
  (`Local(LocalId)`, etc.).

## Phase 1: `expo-parser`

Input: source text (per file).
Output: raw AST (per file).

`expo-parser` produces an AST faithfully reflecting the source. No
semantic analysis. No name resolution. No type inference. No
synthesis.

`expo-parser` does not fail-fast on semantic errors that aren't
syntactic; those are `expo-typecheck`'s job.

## Phase 2: `expo-typecheck` (the semantic-analysis phase)

Input: raw AST + build configuration (target, cfg flags, etc.).
Output: sealed AST, with `Resolution` and type annotations on every
relevant node.

Crate name `expo-typecheck` for implementation continuity. The phase
is conceptually broader than its name — it does cfg-stripping,
synthesis of default protocol impls, name and type resolution,
type checking, annotation, and sealing. There is no separate
"preprocess" phase.

### Sub-pass order

```text
expo-typecheck:
  strip-cfg     -> remove @cfg-excluded nodes
  collect       -> register surviving top-level decls; assign
                   Identifier
  synthesize    -> generate AST for default protocol impls (Debug,
                   etc.) for surviving types only
  resolve       -> walk all bodies (user + synthesized); populate
                   Resolution + resolved_type annotations
  check         -> validate type compatibility
  annotate      -> populate any remaining annotations (coercion, etc.)
  seal          -> assert sealed-AST invariants per seal_ast
```

Order is forced by data dependencies, not preference.

### Decision procedure for new AST transformations

When a new transformation is proposed, find its slot mechanically:

1. Does it remove nodes that should never be checked?
   → strip-cfg time.
2. Does it need type info?
   → after collect (synthesize time or later).
3. Does it produce nodes that themselves need checking?
   → before resolve.
4. Does it touch invariants the seal asserts?
   → it doesn't belong in typecheck; it belongs in expo-ir or later.

### What typecheck owns

- All AST mutation between parse and seal.
- Name resolution for every identifier.
- Type resolution for every type expression.
- Overload / dispatch resolution for every call.
- Coercion annotation on every site that requires one.
- Synthesis of default protocol impl bodies.
- Registration of every top-level decl in its own indices
  (`TypeContext`).

### What typecheck does not own

- Generic specialization (monomorphization). Generic decls remain
  in the sealed AST with type-parameter references; expo-ir
  specializes them.
- Any IR construction.
- Any awareness of LLVM types or backend concerns.

### Seal contract

```rust
pub fn seal_ast(ast: &Ast) -> Result<(), SealViolation>
```

Walks every AST node and asserts the relevant resolution annotation
is `Some(_)` and every `Expr.resolved_type` is populated.

The seal applies on the **typecheck-success path**: typecheck OK →
seal*ast → expo-ir. Typecheck-\_failure* ASTs (partial annotations,
diagnostics attached) are consumed by the LSP best-effort and never
enter the lowering pipeline. The LSP never calls `seal_ast`.

## Phase 3: expo-ir (the lowering phase)

Input: sealed AST.
Output: sealed `IRProgram`.

### Sub-pass order

```text
expo-ir:
  collect-package -> per-package source-lowering produces an IRPackage
                     fragment (cacheable per package per D7's incremental
                     story)
  merge           -> merge IRPackages into a single working IRProgram
  closure         -> whole-program walk discovers required generic
                     instantiations; planners register them in IRProgram
  elaborate       -> later refinements (currently empty; reserved for
                     future passes)
  seal            -> assert sealed-IRProgram invariants per seal_program
```

### Pipeline entry points

```rust
pub fn lower_package(
    source: &PackageSource,
    deps: &[&IRPackage],
    type_ctx: &TypeContext,
) -> Result<IRPackage, LowerError>;

pub fn lower_program(
    packages: &[IRPackage],
    entry: EntryPointSpec,
    type_ctx: &TypeContext,
) -> Result<IRProgram, LowerError>;
```

`lower_package` is pure with respect to its inputs; cacheable per
package. `lower_program` is reconstructed every build; it is not
cached.

### What expo-ir owns

- Translation of sealed AST to IR instructions.
- Generic specialization (the closure pass discovers; planners
  produce specialized decls).
- All coercion emission. Each `Coercion` annotation in the AST
  becomes an explicit `IRInstruction` (today: `Coercion::UnionWiden`
  → `IRInstruction::UnionWrap`).
- Construction of the entry-point reference in the resulting
  `IRProgram`.

### What expo-ir does not own

- Name resolution (typecheck did it; expo-ir reads annotations).
- Type resolution (typecheck did it).
- Overload resolution (typecheck did it).
- Coercion _decisions_ (typecheck made them; expo-ir emits the
  corresponding instructions).

### `lower::*` helpers contract

The free functions in `expo-ir/src/lower/` are **pure translators**
under this architecture:

- They do not discover generics or bail on unregistered symbols.
- They panic on lookup misses. Misses are `seal` violations
  upstream, not recoverable conditions in the helpers.
- They have no `Ok(None) → Stub` paths. `IRInstruction::Stub` does
  not exist in the sealed IR.

### Seal contract

```rust
pub fn seal_program(prog: &IRProgram) -> Result<(), SealViolation>
```

Asserts:

- No `IRInstruction::Stub` anywhere.
- Every callee in `IRInstruction::Call` and friends resolves to a
  registered symbol in `prog.functions`.
- Every operand graph is free of `Coercion` metadata (all coercions
  emitted as instructions).
- The entry point referenced in `prog.entry_point` resolves to a
  registered function.

`seal_program` panics on violation; it is not advisory.

## Phase 4a: expo-codegen (LLVM emission)

Input: sealed `IRProgram`.
Output: LLVM IR / object code.

### What codegen owns

- Translation of each `IRInstruction` to its LLVM emission pattern.
- Codegen builds **its own indices** over the sealed `IRProgram`
  (mangled-name → `FunctionValue`, struct → LLVM type, etc.). It
  does not import any expo-ir or expo-typecheck side-tables.

### What codegen does not own

- Any monomorphization. There is no lazy backfill. There is no
  `lazy_mono_count`. If a callee resolves to a symbol not present
  in `IRProgram.functions`, that is a bug upstream — codegen panics
  with a clear "missing symbol" diagnostic.
- Any interpretation of `Coercion::*`. Codegen does not import
  `Coercion`. Each `IRInstruction` has a single direct LLVM
  emission pattern.
- Any imports of `expo-typecheck`. The sealed `IRProgram` is the
  full input contract.

## Phase 4b: expo-ir-eval (interpreter)

Input: sealed `IRProgram`.
Output: side effects from running the program (or returned values
in REPL mode).

Sibling to `expo-codegen`, not sequential. Both backends consume
the same sealed `IRProgram` and follow the same pattern: each
`IRInstruction` has one direct emission/interpretation; no
fallback paths; no awareness of `Coercion::*`.

REPL debugging features (e.g. dynamically inspecting running
processes) are interpreter-binary machinery built on top of sealed
IR. They are not IR features and do not introduce any
`IRInstruction::Dynamic*` constructs.

## Cross-cutting concerns

### Identifiers and resolution

One identifier type:

```rust
pub struct Identifier {
    package: String,
    path: Vec<String>,  // lexical containment chain
}
```

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

Type args remain _separate_ from the identifier and compose at use
sites: `List<Int>` is `(Identifier { package: "std", path:
["List"] }, type_args: [Type::Int])`.

AST annotation type:

```rust
pub enum Resolution {
    #[default]
    Unresolved,
    Global(Identifier),
}
```

Single-variant enum from day one. When a future memory optimization
adds `Local(LocalId)`, Rust's exhaustiveness checker flags every
read site that needs an arm.

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

Satisfied by construction. `Identifier` paths are derived from
source positions (lexical containment + naming policy). Cross-
package references stay valid across cache hits because they are
content-addressed, not opaque-counter-allocated.

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

`Coercion` (defined in `expo-typecheck`) is a typecheck-layer
vocabulary. It records "the value at this site needs widening /
wrapping / conversion before it can flow to its consumer."

- One `IRInstruction::*` variant per `Coercion::*` variant
  (today: `Coercion::UnionWiden` ↔ `IRInstruction::UnionWrap`).
- The lowerer translates every `Coercion` annotation into the
  corresponding `IRInstruction` at the exact site.
- The sealed `IRProgram` contains zero `Coercion` metadata on any
  operand.
- `expo-codegen` and `expo-ir-eval` do not import `Coercion`.
- Adding a new `Coercion` variant requires adding the paired
  `IRInstruction` and lowerer emitter in the same change.

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
| `seal_ast`              | typecheck | every annotation populated; every `resolved_type` set                      |
| `seal_program`          | expo-ir   | no `Stub`; every callee registered; no `Coercion` metadata; entry resolved |
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
| `expo lex`   | `--emit-tokens`                                                          |
| `expo parse` | `--emit-ast` (raw AST; annotations all `None`)                           |
| `expo check` | `--emit-ast` (sealed AST; annotations populated)                         |
| `expo build` | `--emit-package`, `--emit-ir`, `--emit-llvm`, `--emit-asm`, `--emit-obj` |

The same artifact name (`--emit-ast` on both `parse` and `check`)
is intentional: same printer, different content. The seal state
is what differs.

Mechanical commitments:

- Output goes to stdout by default (`expo check --emit-ast | jq
...` is composable). `-o <file>` redirects to a file when the
  caller wants persistent output.
- Each emit short-circuits the command. `expo build --emit-ir`
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

The substrate (sealed AST / sealed `IRProgram`) does not ship
indices. Each consumer builds its own purpose-shaped lookups:

| Consumer          | Sealed input       | Builds                                                   |
| ----------------- | ------------------ | -------------------------------------------------------- |
| `expo-codegen`    | sealed `IRProgram` | mangled-name → `FunctionValue`, struct → LLVM type, etc. |
| `expo-ir-eval`    | sealed `IRProgram` | mangled-name → interpreter handle, etc.                  |
| `expo-ir` (lower) | sealed AST         | per-function substitution context, mono registry, etc.   |
| LSP               | sealed AST         | name → references, file → symbols, ID → def site, etc.   |
| (future) tools    | sealed AST         | whatever they need                                       |

`TypeContext` is _typecheck's own_ indices — its purpose-built
lookup tables. It is not a privileged shared brain. Other
consumers can ignore it and build their own indices keyed by the
same `Identifier`s.

### Entry points

```rust
pub enum EntryPoint {
    LegacyMain(Identifier),     // legacy fn main; going away
    Process(Identifier),        // Process.run entry
    Eval(Identifier),           // whole-file eval entry
}
```

The entry point is a property of `IRProgram`, not of any
`IRPackage`. Library packages built standalone produce an
`IRPackage` and stop; only executable builds construct an
`IRProgram`.

`IRFunctionKind::MainEntry` does not exist. The user's `fn main`
(during the legacy-support window) is a normal `IRFunctionKind::Free`;
its body is reachable to the closure pass like any other function.

## Mechanical checks

The following are grep/assertion-checkable in CI:

- `expo-codegen` does not import `expo-typecheck`. Grep:
  `rg "use expo_typecheck" expo/crates/expo-codegen/`
- `expo-codegen` does not call any `monomorphize_*` planner. Grep:
  `rg "monomorphize_" expo/crates/expo-codegen/`
- No `lazy_mono_count` field or its increment sites exist.
- `IRInstruction::Stub` does not exist.
- No `Ok(None)` paths in `expo-ir/src/lower/` that fall through to
  `Stub` emission.
- `expo-codegen/src/stmt.rs::apply_coercion` does not exist.
- `expo-codegen` does not import `Coercion`.
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

Add it as a sub-pass in expo-ir, ordered between existing
sub-passes by data dependencies. If it must run after the closure
pass and before seal, it lands in elaborate. If it must run before
the closure pass, it lands earlier.

### A new `IRInstruction` variant

Add the corresponding emission patterns to both backends
(`expo-codegen` and `expo-ir-eval`) in the same change. If the
instruction materializes a coercion, also add the paired
`Coercion::*` variant to typecheck and the staging emitter to the
lowerer.

### A new `Coercion` variant

Same change: add the paired `IRInstruction::*` variant and the
lowerer's staging emitter. Codegen and eval gain emission patterns
for the new instruction. The variant should not require any code
change in `expo-codegen` other than the new instruction's
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
- **Side-tables shared across phases.** Each phase produces its
  own outputs; downstream consumers build their own indices.
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
- `PACKAGE.md` — package model.
- `TYPES.md` — type system.

Predecessor docs (archived; preserved for historical context, not
authoritative):

- `archive/20260502-COMPILER.md` — earlier self-hosted compiler
  architecture sketch (Phase 6A material; will be revisited under
  the northstar when self-hosting begins).
- `archive/20260502-EXPOIR-ROADMAP.md` — superseded ExpoIR
  roadmap.
- `archive/20260427-EXPOIR.md` — original SIL-style design notes.

When this northstar conflicts with an older doc, the northstar
wins; the older doc is superseded for the topic in question and
should be updated or archived.
