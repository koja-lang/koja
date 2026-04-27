# ExpoIR: Intermediate Representation

Design notes for Expo's intermediate representation between the typed AST and
codegen backends. Inspired by Swift's SIL (Swift Intermediate Language) rather
than Rust's MIR, because Expo's ownership model is closer to Swift's than
Rust's, and the SIL-style approach preserves high-level semantics that
multiple backends can lower independently.

---

## Motivation

### The current problem

`expo-codegen` does too many things at once:

- Monomorphizes generic types and functions
- Desugars closures into environment structs + function pointers
- Inserts ownership transfers and drops
- Lowers pattern matching to branches
- Lowers `for` loops to index arithmetic
- Builds string interpolation concatenation chains
- Emits LLVM IR via inkwell

All of this is interleaved. The `c.types.structs` registry (a
`HashMap<String, StructType>`) sits at the center, and every refactor attempt
has broken it because the pass is doing lowering and emission simultaneously.

### What an IR solves

Split the compiler into two clean stages:

```
Typed AST → Lowering → ExpoIR → Emission → LLVM IR (or Cranelift, C, WASM)
```

**Lowering** handles all the semantic complexity: monomorphization, closure
desugaring, drop insertion, pattern matching. It reads `resolved_type` from
the typed AST and produces flat, explicit IR.

**Emission** is mechanical: walk IR instructions, emit target-specific code.
A backend only needs to handle ~25 instruction types, not the full AST. No
string-key type lookups, no `find_type`, no `c.types.structs`.

### Why SIL-style, not MIR-style

Two levels of IR were considered:

**MIR-style** (Rust's approach): very low-level, essentially structured
assembly. `load_tag` + `branch` + `extract_payload` for enums, raw
`load_field`/`store_field` for structs, manual environment construction for
closures.

**SIL-style** (Swift's approach): higher-level, preserving source semantics.
`switch_enum` for pattern matching, `struct_extract` for field access,
`partial_apply` for closures, explicit ownership instructions.

SIL-style is better for Expo because:

1. **Multiple backends benefit from high-level operations.** `switch_enum`
   lets LLVM emit a switch instruction, C emit a `switch` statement, and
   Cranelift emit its own branch table. With MIR-style, each backend
   reconstructs the high-level pattern from low-level tag checks.

2. **Ownership operations are first-class.** Expo has move semantics,
   borrow-by-default, `clone()`, and deterministic drop. Making these
   explicit in the IR enables optimization passes that eliminate redundant
   clones and drops. LLVM sees `clone`/`drop` as opaque function calls
   and can't optimize them.

3. **Two-level split (raw → canonical) simplifies lowering.** Raw ExpoIR
   is a mechanical translation from the typed AST -- conservative clones,
   drops at every scope exit. Optimization passes then tighten it. This is
   easier to implement than doing everything perfectly in one pass.

4. **Future ARC for shared types** (see below) requires exactly the kind
   of retain/release optimization that SIL was built for.

---

## What lowering does

The lowering pass reads the typed AST (where every `Expr` has `resolved_type`)
and produces ExpoIR. The following transformations happen during lowering:

| Source concept                   | Lowered to                                     |
| -------------------------------- | ---------------------------------------------- |
| Generics (`List<Int32>`)         | Monomorphized concrete types                   |
| Method calls (`p.distance()`)    | Direct calls (`Point_distance(p)`)             |
| Closures (`fn (x) -> x + n end`) | Environment struct + free function             |
| `for` loops                      | `while` + `.get()` + `.length()` calls         |
| `match` on enums                 | `switch_enum` with payload extraction          |
| String interpolation (`"#{x}"`)  | `.format()` + `.concat()` calls                |
| Field access (`p.x`)             | `struct_extract`                               |
| Struct construction              | `struct` instruction                           |
| Ownership drops                  | Explicit `drop_value` at scope exits           |
| Borrows                          | Explicit `borrow_value` / `end_borrow`         |
| Moves                            | `move_value` (source becomes dead)             |
| Clones                           | `clone_value` (deep copy, new owner)           |
| `self`                           | Desugared to explicit first parameter          |
| `Self` type alias                | Resolved to concrete type                      |
| `impl` blocks                    | Flattened to free functions with mangled names |

What lowering does NOT do:

- Register allocation (that's LLVM's job)
- Instruction selection (that's the backend's job)
- Memory layout (backends decide struct padding, alignment)

---

## IR structure

### Modules, functions, basic blocks

```
module "my_app"

fn Point_distance_squared(self: &Point) -> Int32:
  entry:
    %0 = struct_extract %self : $Point, #Point.x
    %1 = struct_extract %self : $Point, #Point.y
    %2 = builtin mul_i32(%0, %0)
    %3 = builtin mul_i32(%1, %1)
    %4 = builtin add_i32(%2, %3)
    return %4 : $Int32
```

- Functions have mangled names, explicit parameter types, explicit return types.
- Basic blocks are labeled (`entry:`, `loop_head:`, `then:`, etc.).
- Values use SSA form: each `%name` is assigned once.
- Types use `$` prefix for clarity in textual representation.
- `#Type.field` references a specific field on a specific type.

### Ownership instructions

```
%1 = move_value %0 : $String       // transfer ownership, %0 is dead
%2 = borrow_value %0 : $String     // read-only reference, %0 stays live
end_borrow %2 : $String            // end the borrow scope
%3 = clone_value %0 : $String      // deep copy, independent owner
drop_value %0 : $String            // free at scope exit
```

Raw ExpoIR inserts these conservatively. Optimization passes eliminate
redundant operations:

- Clone followed by drop of the original → move
- Borrow that outlives the original → error (caught as diagnostic)
- Drop of already-moved value → remove

### Struct operations

```
// Construction
%p = struct $Point (%x : $Int32, %y : $Int32)

// Field access
%0 = struct_extract %p : $Point, #Point.x
```

`struct` creates a value in one instruction. `struct_extract` is a typed
field access. Backends decide the implementation: LLVM uses `getelementptr`,
C uses `point.x`, WASM uses `struct.get`.

### Enum operations

```
// Construction
%some = enum $Option<Int32>, #Some, %val : $Int32
%none = enum $Option<Int32>, #None

// Pattern matching
switch_enum %opt : $Option<Int32>,
    case #Some: bb_some,
    case #None: bb_none

bb_some(%payload : $Int32):
    // payload arrives as a block argument
    ...

bb_none:
    ...
```

`switch_enum` replaces the AST's `match` expression. It dispatches on the
tag and delivers payloads as block arguments. No manual tag loading, no
offset-based payload extraction. The backend decides how to implement the
dispatch (comparison chain, jump table, etc.).

### Closure operations

```expo
fn make_adder(n: Int32) -> fn(Int32) -> Int32
  fn (x: Int32) -> Int32
    x + n
  end
end
```

```
// Lowered: closure becomes environment struct + free function
struct $__closure_make_adder_0_env (n: Int32)

fn __closure_make_adder_0(env: &__closure_make_adder_0_env, x: Int32) -> Int32:
  entry:
    %n = struct_extract %env : $__closure_make_adder_0_env, #n
    %0 = builtin add_i32(%x, %n)
    return %0 : $Int32

fn make_adder(n: Int32) -> FnRef:
  entry:
    %env = heap_alloc $__closure_make_adder_0_env
    %0 = struct $__closure_make_adder_0_env (%n : $Int32)
    store %env, %0
    %1 = partial_apply @__closure_make_adder_0, %env
    return %1 : $FnRef
```

`partial_apply` captures the idea "bind these values to this function."
Backends decide whether the environment is heap-allocated, stack-allocated,
or inlined.

### Control flow

```expo
fn max(a: Int32, b: Int32) -> Int32
  if a > b
    a
  else
    b
  end
end
```

```
fn max(a: Int32, b: Int32) -> Int32:
  entry:
    %0 = builtin gt_i32(%a, %b)
    cond_br %0, then, else_

  then:
    return %a : $Int32

  else_:
    return %b : $Int32
```

All control flow is branches between basic blocks. No `if`, `while`, `for`,
`loop`, `cond`, or `match` in the IR.

### For loops

```expo
for item in list
  print(item)
end
```

```
entry:
    %len = apply @List_Int32_length(%list)
    %idx = alloca $Int32
    store %idx, const_i32 0
    jump loop_head

  loop_head:
    %i = load %idx
    %cond = builtin lt_i32(%i, %len)
    cond_br %cond, loop_body, loop_exit

  loop_body:
    %opt = apply @List_Int32_get(%list, %i)
    %item = switch_enum_extract %opt : $Option<Int32>, #Some
    apply @print(%item)
    %i2 = builtin add_i32(%i, const_i32 1)
    store %idx, %i2
    jump loop_head

  loop_exit:
    ...
```

### String interpolation

```expo
"hello #{name}, you are #{age} years old"
```

```
%0 = string_literal "hello "
%1 = apply @String_format(%name)
%2 = apply @String_concat(%0, %1)
%3 = string_literal ", you are "
%4 = apply @String_concat(%2, %3)
%5 = apply @Int_format(%age)
%6 = apply @String_concat(%4, %5)
%7 = string_literal " years old"
%8 = apply @String_concat(%6, %7)
```

---

## Instruction set summary

### Value operations

| Instruction                        | Description                                  |
| ---------------------------------- | -------------------------------------------- |
| `struct $T (%fields...)`           | Construct a struct value                     |
| `struct_extract %val, #T.field`    | Extract a field (typed, no offset math)      |
| `enum $T, #Variant [, %payload]`   | Construct an enum value                      |
| `switch_enum %val, case #V: bb...` | Branch on enum tag, deliver payloads         |
| `partial_apply @func, %env`        | Create a closure from function + environment |
| `apply @func(%args...)`            | Call a function                              |
| `builtin op(%args...)`             | Primitive arithmetic/comparison              |
| `string_literal "..."`             | Create a string constant                     |

### Ownership operations

| Instruction         | Description                               |
| ------------------- | ----------------------------------------- |
| `move_value %val`   | Transfer ownership (source becomes dead)  |
| `borrow_value %val` | Start a read-only borrow                  |
| `end_borrow %val`   | End a borrow scope                        |
| `clone_value %val`  | Deep copy, new independent owner          |
| `drop_value %val`   | Free the value (deterministic destructor) |

### Memory operations

| Instruction        | Description      |
| ------------------ | ---------------- |
| `alloca $T`        | Stack allocation |
| `heap_alloc $T`    | Heap allocation  |
| `load %ptr`        | Load from memory |
| `store %ptr, %val` | Store to memory  |

### Control flow

| Instruction                       | Description          |
| --------------------------------- | -------------------- |
| `return %val`                     | Return from function |
| `cond_br %cond, bb_then, bb_else` | Conditional branch   |
| `jump bb`                         | Unconditional branch |
| `unreachable`                     | Marks dead code      |

### Shared types (future)

| Instruction           | Description                                 |
| --------------------- | ------------------------------------------- |
| `shared_alloc $T`     | Allocate shared (ARC) object, ref count = 1 |
| `shared_retain %val`  | Increment reference count (atomic)          |
| `shared_release %val` | Decrement reference count, free if zero     |
| `shared_read %val`    | Begin atomic read access                    |
| `shared_write %val`   | Begin atomic write access                   |

---

## Optimization passes

### Mandatory passes (raw → canonical)

These must run for correctness:

1. **Ownership verification** -- every value is moved, borrowed, or dropped
   exactly once on every control flow path. Violations become compile errors.

2. **Clone/drop elimination** -- remove `clone_value` when the original is
   consumed immediately after (replace with `move_value`). Remove
   `drop_value` on already-moved values.

3. **Definite initialization** -- verify every variable is assigned before
   use on all paths. This diagnostic is easier on the IR's CFG than on the
   AST's tree structure.

### Optional passes (canonical → optimized)

These improve performance:

4. **Closure inlining** -- if a `partial_apply` result is only called once,
   inline the closure body and eliminate the environment allocation.

5. **Dead code elimination** -- remove unreachable basic blocks and unused
   values.

6. **Constant folding** -- evaluate `builtin add_i32(const 3, const 4)` at
   compile time.

### Future: ARC optimization passes

When shared types are added:

7. **Retain/release pairing** -- eliminate `shared_retain` immediately
   followed by `shared_release` on the same value.

8. **Retain sinking / release hoisting** -- move retain later and release
   earlier to minimize the window where the reference is held.

9. **Single-owner elision** -- if analysis proves a shared reference never
   escapes the creating process, replace atomic retain/release with non-atomic
   operations (or elide entirely).

10. **Read-only detection** -- if a process only performs `shared_read` and
    never `shared_write`, the backend can use read-optimized locking.

---

## Shared types and ARC

### The problem

Expo's concurrency model is process isolation with message passing. This
works well until you need a hot cache readable by many processes. The pure
actor approach (one process owns the cache, everyone sends `call` messages)
adds latency and bottlenecks the owner's mailbox.

Erlang solves this with ETS (Erlang Term Storage) -- a concurrent data
structure that lives outside any process's heap. It breaks the isolation
model, but it's one of the most important features of the BEAM.

### The proposal

ARC-based shared types give ETS-like capability while staying within the
ownership system:

```expo
shared_cache = SharedMap.new<String, User>()

# Passing to spawned processes clones the handle, not the data
ref = spawn Worker.start(WorkerConfig{cache: shared_cache})

# Multiple processes hold references to the same underlying data
# Reads/writes are atomic at the key level
# When the last reference is dropped, memory is freed
```

The handle (`shared_cache`) is a Copy type internally -- an atomic reference
count + pointer. Passing it to `spawn` or `cast` copies the handle and
increments the count. This is fundamentally different from normal move
semantics, which is why it needs language-level support.

### Why this requires ExpoIR

Without an IR, ARC operations are opaque LLVM calls that can't be optimized.
With ExpoIR, `shared_retain` and `shared_release` are first-class
instructions. The same optimization framework that eliminates redundant
`clone_value`/`drop_value` pairs also eliminates redundant retain/release
pairs. This is exactly what Apple built SIL for -- ARC optimization was their
#1 motivation.

---

## Connection to C FFI and self-hosting

### The dependency chain

```
ExpoIR → C FFI → shared types → incremental self-hosting
```

1. **ExpoIR** fixes the codegen type system (`c.types.structs` string-key
   problem). Types are fully resolved during lowering using `resolved_type`
   from the typed AST. Backends receive concrete `IRType` values, no string
   lookups.

2. **C FFI** requires stable codegen. Building FFI on the current codegen
   means redoing it when codegen migrates. Building it on ExpoIR means it's
   stable.

3. **Shared types** require ARC optimization in the IR. The IR must exist
   before shared types can be implemented efficiently.

4. **Incremental self-hosting** uses C FFI to bridge Expo code with the Rust
   compiler. ExpoIR is the natural bridge point -- it's flat and simple
   enough to serialize or expose via C FFI, unlike the deeply nested typed
   AST.

### Incremental self-hosting strategy

With ExpoIR as the bridge:

- **Front-first**: rewrite lexer → parser in Expo. The Expo parser produces
  AST that the Rust typechecker consumes (via C FFI or serialization).
- **Back-first**: the Rust compiler lowers to ExpoIR. An Expo-written backend
  consumes ExpoIR and emits target code (calling LLVM C API via FFI).
- **ExpoIR as the split point**: Rust handles `Source → Typed AST → ExpoIR`,
  Expo handles `ExpoIR → target`. This is clean because ExpoIR is designed
  to be simple and flat.

---

## Rust data structures (bootstrap compiler)

During the Rust bootstrap phase, ExpoIR is a Rust crate (`expo-ir`):

```rust
pub struct IRProgram {
    pub name: String,
    pub structs: Vec<IRStruct>,
    pub functions: Vec<IRFunction>,
}

pub struct IRFunction {
    pub name: String,
    pub params: Vec<(String, IRType)>,
    pub return_type: IRType,
    pub blocks: Vec<BasicBlock>,
}

pub struct BasicBlock {
    pub label: String,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

pub enum Instruction {
    Struct { dest: Var, ty: IRType, fields: Vec<Operand> },
    StructExtract { dest: Var, base: Operand, ty: IRType, field: String },
    Enum { dest: Var, ty: IRType, variant: String, payload: Option<Operand> },
    PartialApply { dest: Var, func: String, env: Var },
    Apply { dest: Option<Var>, func: String, args: Vec<Operand> },
    Builtin { dest: Var, op: BuiltinOp, args: Vec<Operand> },
    StringLiteral { dest: Var, value: String },

    MoveValue { dest: Var, source: Var },
    BorrowValue { dest: Var, source: Var },
    EndBorrow { value: Var },
    CloneValue { dest: Var, source: Var },
    DropValue { value: Var },

    Alloca { dest: Var, ty: IRType },
    HeapAlloc { dest: Var, ty: IRType },
    Load { dest: Var, ptr: Var },
    Store { ptr: Var, value: Operand },

    // Future: shared types
    SharedAlloc { dest: Var, ty: IRType },
    SharedRetain { value: Var },
    SharedRelease { value: Var },
}

pub enum Terminator {
    Return(Option<Operand>),
    CondBranch { cond: Operand, then_block: String, else_block: String },
    SwitchEnum { value: Operand, cases: Vec<(String, String)> },
    Jump(String),
    Unreachable,
}

pub enum Operand {
    Var(Var),
    ConstInt(i64),
    ConstFloat(f64),
    ConstStr(String),
    ConstBool(bool),
    Unit,
}

pub enum IRType {
    Named(TypeIdentifier),
    Primitive(Primitive),
    Function { params: Vec<IRType>, return_type: Box<IRType> },
    Ref(Box<IRType>),
    Unit,
}
```

~80 lines of type definitions. A codegen backend is a function
`fn emit(program: &IRProgram) -> Result<()>` that walks the structure.

This sketch describes the SIL-style instruction-level IR planned for
Phase 4c. The Wave 10 work landed a complementary
declaration-level container (`IRProgram` holding `IRStruct` / `IREnum`
/ `IRFunction`) that owns the monomorphized output of lowering. The
two are designed to compose: the Phase 4c instruction containers will
live inside `IRFunction.blocks` once `IRFunction` is extended beyond
its current AST-wrapped form. Until then, `IRFunction` keeps the
`expo_ast::ast::Function` body it monomorphized and `expo-codegen`
walks it directly.

---

## Comparison with other compilers

| Compiler        | IR levels                                           | Primary motivation                                       |
| --------------- | --------------------------------------------------- | -------------------------------------------------------- |
| Rust            | AST → HIR → MIR → LLVM IR                           | Borrow checker operates on MIR                           |
| Swift           | AST → Raw SIL → Canonical SIL → LLVM IR             | ARC optimization, generic specialization                 |
| Go              | AST → SSA → machine code                            | Optimization, no LLVM dependency                         |
| Expo (current)  | Typed AST → LLVM IR                                 | (no IR, lowering and emission interleaved)               |
| Expo (proposed) | Typed AST → Raw ExpoIR → Canonical ExpoIR → backend | Ownership optimization, multiple backends, clean codegen |

Apple's primary motivations for SIL, mapped to Expo:

| Apple's reason                       | Expo equivalent                                    |
| ------------------------------------ | -------------------------------------------------- |
| ARC optimization (retain/release)    | Clone/drop elimination, future shared type ARC     |
| Semantic diagnostics (definite init) | Ownership verification, unreachable code           |
| Generic specialization               | Monomorphization (already needed)                  |
| Protocol devirtualization            | Not needed (Expo is already statically dispatched) |
| Clean separation of concerns         | Fixes `c.types.structs`, enables multiple backends |

---

## Roadmap and current status

ExpoIR is Phase 6 work in [`ROADMAP.md`](ROADMAP.md), but the typed AST
foundation that unblocks it has been done since Phase 5, and substantial
ExpoIR foundation work has been pulled forward during the Phase 4 codegen
refactor. This section is the status-of-record for that pulled-forward
work, sister to the broader self-hosting context in
[`ROADMAP.md`](ROADMAP.md) Phase 6A.

### Where we are

The compiler does not yet construct or consume an instruction-level
SIL-style IR -- function bodies are still walked from the typed AST
during emission, with no `IRBasicBlock` / `IRInstruction` shape yet.
But the foundation has been substantively built: the `expo-ir` crate
exists, a decision-type vocabulary is extracted and in active use,
the LLVM-free semantic state and helpers have been lifted off
`Compiler` behind a `LowerCtx<'a>` borrow bundle, and as of Wave 10
a declaration-level IR (`IRProgram` / `IRStruct` / `IREnum` /
`IRFunction`) is constructed by the lowering planners and consumed
by emission for every monomorphized type and function.

The work began as a narrow `TypeRegistry` key migration to fix a
package-qualified type collision and grew into a 6-wave type-system +
codegen refactor because the entanglement was deeper than the original
plan assumed. Each wave preserved a green test suite. The result is that
the seam between "decide what to emit" and "emit it" is now visible in
code -- ~80 call sites cross the `Compiler::lower_ctx()` gateway, and
22 `Resolved*` types describe the lowering output in terms a future IR
will consume.

### Current IR surface

What actually lives in `expo-ir` today:

- **Active lowering helpers** (produced + consumed): `TypeLayouts`,
  `FnLowerState` (now including a per-function `block_counter` +
  `next_block_id()`, mirroring `closure_counter`), `LowerCtx`, ~51 free
  functions across 19 modules in
  `lower::{binary, calls, closures, conditionals, constants, debug, enums, fields, inference, loops, mangling, methods, monomorphize, naming, patterns, processes, stmt, strings, structs, types}`
  plus the small `util::parse_int_literal` helper. Reference:
  [`expo/crates/expo-ir/src/lower/`](../crates/expo-ir/src/lower/).
- **Active declaration-level IR containers** (produced by Wave 10
  planners, consumed by `expo-codegen` emitters): `IRProgram` plus
  `IRStruct` / `IREnum` / `IRFunction` and their `IRStructKind` /
  `IRFunctionKind` companions in
  [`expo-ir::program`](../crates/expo-ir/src/program.rs).
- **Active instruction-level IR scaffolding** (introduced in Wave 11
  by the `compile_unless` lift, extended in Wave 12 with the operand
  model): `IRBlockId`, `IRBasicBlock { id, label, instructions,
  terminator }`, and `IRTerminator` (`Branch` / `CondBranch` /
  `Unreachable`) in
  [`expo-ir::blocks`](../crates/expo-ir/src/blocks.rs); plus
  `IRValueId`, `IROperand` (`ConstBool` / `ConstFloat` / `ConstInt` /
  `ConstStr` / `Local` / `Unit`), and `IRInstruction` (single
  transitional `Stub { dest, expr }` variant) in
  [`expo-ir::values`](../crates/expo-ir/src/values.rs). The shared
  helper [`lower_expr_to_operand`](../crates/expo-ir/src/lower/values.rs)
  is the single seam every construct uses to thread an
  expression-shaped value into the IR: literal `Expr` shapes become
  inline `IROperand` constants emitting no instructions, every other
  shape mints a value id and pushes one `IRInstruction::Stub` onto
  the caller's instruction sequence. The first conditional construct
  lowered through this scaffold is
  [`IRUnless`](../crates/expo-ir/src/resolved/conditionals.rs)
  produced by [`lower_unless`](../crates/expo-ir/src/lower/conditionals.rs).
  `IRTerminator::CondBranch::cond` now holds an `IROperand`; emission
  resolves it through a per-block
  `HashMap<IRValueId, BasicValueEnum<'ctx>>` populated by walking the
  block's instructions before dispatching its terminator. Statement
  bodies remain AST stubs walked by `compile_statement`; later slices
  replace them with instruction-level lowerings.
- **Active decision-type vocabulary** (produced + consumed): ~33
  `Resolved*`/`Format*` types across 15 modules in
  `resolved::{calls, closures, conditionals, constants, construction, debug, enums, fields, loops, match_expr, methods, ops, patterns, processes, strings}`,
  plus 4 pure resolver functions (`resolve_binary_op`, `resolve_unary_op`,
  `resolve_compound_op`, `resolve_string`). Reference:
  [`expo/crates/expo-ir/src/resolved/`](../crates/expo-ir/src/resolved/).
- **Transitional identities** (in
  [`expo-ir::identity`](../crates/expo-ir/src/identity.rs)): `TypeIdentifier`
  for source-level package-qualified types; `MonomorphizedTypeIdentifier`
  for canonical type-cache keys (package-qualified for non-generics,
  mangled `Type_$Arg$` for generic instances); `FunctionIdentifier` for
  mangled function symbols; `VariantIdentifier` for `(enum, variant)`
  pairs. All four are newtype wrappers around `String` today; in Phase 5+
  they become interned `u32`s with no call-site changes.

The IR _instruction set_ is being filled in incrementally as
constructs lift. Wave 11 fixed the block/terminator vocabulary; Wave 12
introduced the operand model (`IRValueId`, `IROperand`,
`IRInstruction`) plus a single transitional `Stub` instruction
variant that bridges to AST-level expression emission. Each future
[`expo_ast::ast::ExprKind`] that learns to lower replaces its `Stub`
site with a typed `IRInstruction` variant; when the last consumer is
gone, `Stub` is deleted in one PR. The remaining constructs (`if`,
`ternary`, `cond`, `match`, loops) and the value-producing
instruction set both fill in as separate slices, each one extending
the IR only by what its consumer requires. An earlier attempt to
define the full instruction set top-down was deleted because it had
no producers and had already drifted from the `Resolved*` shapes
that emerged from real code paths.

#### Architectural invariant: control-flow negation lives in lowering

Slice 1 of Phase 4c committed to a canonicalization rule that every
later slice (and every backend) inherits: **control-flow negation is
expressed by branch-target ordering, not by an IR `Not` operator or a
`negated` flag**. `unless cond ... end` lowers to
`IRTerminator::CondBranch { cond, then: merge, otherwise: body }` --
the body block lives on `otherwise`, the merge block lives on `then`,
and that's the entire structural content of "unless-ness." When the
`if` slice lands, `if cond ... end` will lower to the same terminator
shape with the targets swapped (`then: body, otherwise: merge`).

The rationale is twofold. First, every backend implements one
cond-branch lowering and reuses it across every conditional construct
in the language; without canonicalization each construct's emission
encodes its own branch-direction knowledge, and each backend pays a
peephole-fold cost it does not need to. Second, the negation that
exists *to flip a branch direction* is conceptually distinct from the
negation that produces a value (`let x = !cond`); collapsing the
former into target ordering and leaving the latter as a unary op
keeps the two concerns visibly separate. Value-context negation is
unaffected by this invariant -- it remains a unary op handled by
`compile_unary`.

Crate sizes (approximate): `expo-codegen` ~17k LOC, `expo-ir` ~5.1k LOC.

### Phase status

- **Phase 1 -- Typed foundation: done.** The original `TypeRegistry`
  migration plus 5 followup waves. Net effect: typed AST throughout
  codegen, package-qualified type identities, `LLVMTypeCache` /
  `TypeLayouts` split, `FnState` / `FnLowerState` split, 9 LLVM-free
  helpers lifted off `Compiler` behind a `LowerCtx<'a>` borrow bundle.
  `Compiler` now exposes no semantic-decision methods. Remaining minor
  cleanup: ~35 `TypeContext::find_type(&str)` call sites in codegen --
  folded into Phase 4 since the touched files overlap.
- **Phase 2 -- Extract decision types: substantively done.** 22
  `Resolved*` types live in `expo-ir/src/resolved/`. The canonical
  example from the original plan -- the `compile_binary` split -- is
  in place: see `compile_binary` in `expo-codegen/src/ops.rs` consuming
  `resolve_binary_op` from `expo-ir/src/resolved/ops.rs`. Remaining: the
  `<'ctx>`-bound functions originally tagged "heavily mixed"
  (`compile_method_call`, `compile_receive`, `compile_closure_core`,
  `compile_enum_struct_eq`, parts of `compile_expr` / `compile_statement`)
  still need their decision/LLVM split -- folded into Phase 4b.
- **Phase 3 -- Collect into `expo-ir`: done as scoped.** The crate
  exists and hosts `resolved/`, `lower/`, `TypeLayouts`, `FnLowerState`,
  `VariantId`. Phase 3 was originally framed as "pull decision types
  into their own crate"; that part is complete. Defining the IR
  instruction containers (`IRFunction`, `IRBasicBlock`, `IRInstruction`,
  etc.) was deliberately left undone -- Phase 4c will design them
  bottom-up from real consumers rather than top-down from speculation.
- **Phase 4 -- Move lowering out: substantively done.** Four pure
  resolvers moved to `expo-ir` (in `resolved::ops` and
  `resolved::strings`); the 9 Wave 6 helpers, ~28 Wave 7 helpers,
  the Wave 8a-8d structural splits in `lower::*`, the Wave 10
  monomorphization registry + final two `<'ctx>`-bound resolver lifts,
  and Wave 11's first instruction-level scaffold + `unless` lift.
  Remaining work is the rest of the Phase 4c construct ladder:
  - **4b (structural): done.** All five resolvers in the original
    cluster are lifted. Three (`resolve_field_ptr`,
    `resolve_payload_info`, `resolve_closure`) closed in Wave 8d; the
    final two (`resolve_method_call`, `resolve_static_call`) closed
    in Wave 10 once monomorphization itself lifted into `expo-ir`.
    The lifted resolvers return `ResolvedMethodCall` /
    `ResolvedStaticCall` carrying optional `PendingMethodMono` /
    `PendingTypeMono` payloads, which the caller drains against the
    existing `monomorphize_*` shims before the LLVM `FunctionValue`
    lookup. The original "request-payload handshake" rejection still
    stands for naive lifts; what changed is that the request payloads
    now describe deferred monomorphization steps inside a real IR
    pipeline, not a side-channel back into `Compiler`.
  - **4c (the actual handoff): in progress, slice 1 of N landed.**
    Wave 11 lifted `compile_unless` as the smallest construct that
    forces a real block/terminator vocabulary, fixing the IR shape
    (`IRBlockId`, `IRBasicBlock`, `IRTerminator`,
    `FnLowerState::block_counter`) plus the canonicalization
    invariant for control-flow negation. Subsequent slices reuse the
    vocabulary without further IR commitment beyond filling in
    expression / instruction-level types: slice 2 lifts `compile_if`
    (no value, no else) reusing the same `CondBranch` with body on
    the truthy target; slice 3 introduces `IRPhi` (or value-merging)
    for `compile_if`/`compile_ternary` with else; slice 4 lifts
    `compile_cond` (N-arm); slice 5+ tackles `compile_match`,
    `compile_while`, etc. The ultimate destination is unchanged:
    lowering produces a function-level IR; emission consumes it and
    walks it; the `Lowerer<'a>` driver becomes real; `closure_site_path`
    and `package` move off `Compiler`; and TCO ambient flags collapse
    into a `tail` field on whatever the call instruction ends up
    being named.
- **Phase 5+ -- Opaque IR identities: foundation laid (Wave 9).**
  `MonomorphizedTypeIdentifier`, `FunctionIdentifier`, and the renamed
  `VariantIdentifier` now wrap every cache key in
  `Compiler`/`LLVMTypeCache`/`TypeLayouts` and every `mangled_*` field
  on `Resolved*`. Today the wrappers are `String` newtypes; the actual
  interning (to `u32`) happens incrementally in Phase 5+ behind the
  same call-site signatures.

### Wave history

The 13 waves completed so far, condensed (full prose lives in commit
history):

- **Wave 1 -- TypeRegistry migration.** `TypeRegistry.concrete` rekeyed
  to `HashMap<TypeIdentifier, StructType>`; monomorphized generics kept
  in a separate `monomorphized` map. Fixed the package-qualified type
  collision and unblocked C FFI without waiting for the full IR.
- **Wave 2 -- `lower_struct_field` extraction.** First pure-semantic
  helper lifted into `expo-ir::lower::fields` as a free function. Set
  the pattern that all subsequent waves followed.
- **Wave 3 -- `LLVMTypeCache` rename + `TypeLayouts` extraction.**
  `TypeRegistry` renamed to `LLVMTypeCache` and `Compiler.types` to
  `Compiler.llvm_types` so every call site advertises that the surviving
  cache is purely an LLVM-handle store. Semantic struct/enum layouts
  moved into `expo-ir::TypeLayouts`.
- **Wave 4 -- `enum_variant_payloads` split + `VariantIdentifier`.** Variant
  ordering (= tag value) owned solely by `TypeLayouts`; LLVM payload
  table rekeyed from positional `Vec` to identity-keyed
  `HashMap<VariantIdentifier, Option<StructType>>`. Drift between the two
  stores is now structurally impossible. (Originally introduced as
  `VariantId`; renamed in Wave 9 for naming uniformity.)
- **Wave 5 -- `FnLowerState` extraction + `TailCallCtx` dissolved.**
  Semantic per-function fields (`process_msg_type`, `return_type_hint`,
  `self_type_name`, `type_subst`, TCO ambient flags + 7 traversal
  methods) moved into `expo-ir::FnLowerState`. `TailCallCtx` dissolved
  entirely rather than carved into a parallel sub-struct -- its only
  cohesion was the impending split.
- **Wave 6 -- 9 helpers lifted + `LowerCtx`.** Nine LLVM-free
  semantic-decision helpers (type/name resolution, symbol naming,
  closure metadata lookup) moved off `Compiler` into
  `expo_ir::lower::{types, naming, closures}` as free functions taking a
  `&LowerCtx<'_>` borrow bundle. `Compiler::lower_ctx()` is now the
  single gateway between the LLVM-bound driver and the LLVM-free
  lowering surface; ~80 call sites updated.
- **Wave 7 -- Phase 4a sweep.** ~28 pure-semantic `resolve_*`/`lower_*`
  helpers moved out of `expo-codegen` into ten new `lower::*` modules
  (`binary`, `constants`, `debug`, `enums`, `methods`, `patterns`,
  `processes`, `stmt`, `strings`, `structs`, plus additions to existing
  `closures`/`fields`/`mangling`). `LowerCtx` grew a `&TypeLayouts`
  field so layout-aware lowering can run as free functions; LLVM-bound
  state (variable type maps, per-backend function caches) is threaded
  in via small closures rather than coupling `expo-ir` to a backend.
  Companion change: ~7 codegen-local `Resolved*`/`Format*` decision
  types (binary segments, concat kind, format info, ref/receive
  metadata) moved into `expo-ir::resolved::*` so the lifted helpers can
  return them. `expo-codegen`'s remaining `resolve_*`/`lower_*`
  functions are now exclusively the `<'ctx>`-bound cluster slated for
  Phase 4b.
- **Wave 8a -- `resolve_enumerable_info` split.** First bite of the
  Phase 4b structural cluster. The `<'ctx>`-bound resolver in
  `expo-codegen/src/control/loops.rs` (one caller, one LLVM dependency)
  was split along its `to_llvm_type(...)` seam: the pure-decision half
  is now `expo_ir::lower::loops::resolve_enumerable_info(&LowerCtx, &Type)`
  returning `expo_ir::resolved::loops::ResolvedEnumerable`; the caller
  in `compile_for` derives `elem_llvm_ty` itself. New `lower::loops` /
  `resolved::loops` modules established for future iteration-protocol
  work. Pattern-setter for the rest of the cluster.
- **Wave 8b -- three easy resolver lifts.** The next three structurally
  simple resolvers in the Phase 4b cluster moved as a batch, each one
  hoisting a single LLVM-bound call up to its caller:
  `resolve_union_member` is now `expo_ir::lower::stmt::resolve_union_member`
  returning `resolved::fields::ResolvedUnionMember`; `compile_union_wrap`
  does the `llvm_types.get_monomorphized` lookup itself.
  `resolve_spawn_info` is now `expo_ir::lower::processes::resolve_spawn_info`
  with `ResolvedSpawn` promoted into `resolved::processes`;
  `compile_spawn` precomputes `mangled_state` via the LLVM-bound
  `spawn::resolve_mangled_state`. `resolve_struct_name` (and the
  `struct_name_from_type` helper) is now in
  `expo_ir::lower::structs::resolve_struct_name`; `compile_method_call`
  precomputes the LLVM struct name as `Option<&str>` and threads
  variable-type lookups in via the same closure pattern used by
  `resolve_field_path`. Cluster backlog: nine resolvers down to six.
- **Wave 8c -- `resolve_call` lift.** The largest of the easy-to-medium
  Phase 4b resolvers split along four LLVM-cache seams. The pure-decision
  half is now `expo_ir::lower::calls::resolve_call(&LowerCtx, name,
  is_struct_constructor, function_exists, variable_type,
  is_generic_function)` returning a new lifetime-free
  `expo_ir::resolved::calls::ResolvedCall` (and `BuiltinCall`); the
  variants now carry only pure-semantic data, with `Direct::mangled_name`
  letting the caller do exactly one `FunctionValue` lookup post-dispatch
  and `ClosureVariable` dropping its `PointerValue` (the caller re-fetches
  it from `fn_state.variables`). `compile_call` builds the four closures
  inline so the LLVM-bound predicates (`llvm_types.get_concrete` /
  `contains_monomorphized`, `functions.contains_key`,
  `fn_state.variables.get`, `generic_fn_asts.contains_key`) stay on the
  emission side. New `lower::calls` / `resolved::calls` modules established
  for future call-related splits (`resolve_method_call`,
  `resolve_static_call`). Cluster backlog: six resolvers down to five.
- **Wave 8d -- closure lift + emission-helper re-classification.**
  Three of the remaining five Phase 4b items closed out as a batch.
  `closure_counter` migrated from `expo-codegen`'s `FnState` into
  `expo_ir::FnLowerState` (it was always pure-semantic state);
  `resolve_closure` is now `expo_ir::lower::closures::resolve_closure(
  &LowerCtx, &[ClosureParam], Type, Span, closure_index: usize)`
  returning the existing `ResolvedClosure`. The caller in
  `compile_closure_core` reads-and-bumps `compiler.fn_lower.closure_counter`
  itself and threads the index in, keeping the resolver pure (no
  `&mut` dependency at the IR seam). The two remaining items in the
  cluster -- `resolve_field_ptr` and `resolve_payload_info` -- were
  re-classified rather than lifted: both are emission tail. The
  former (renamed to `field_ptr` in `expo-codegen::stmt`) just walks
  the GEP chain for a dotted path; the semantic decision was already
  lifted in earlier waves as `expo_ir::lower::fields::resolve_field_path`.
  The latter is pure LLVM cache lookup with no decision content. Doc
  comments now flag both as emission-only. Cluster backlog: five
  resolvers down to two; the survivors are blocked on
  monomorphization-in-IR rather than on a missing seam.
- **Wave 9 -- opaque mono identities (registry foundation).**
  Renamed `VariantId` to `VariantIdentifier` and introduced
  `MonomorphizedTypeIdentifier` and `FunctionIdentifier` in
  `expo_ir::identity`, mirroring the `VariantIdentifier` newtype shape
  (`Clone+Debug+Eq+Hash+PartialEq` around `String` today, opaque
  interned later). Migrated every cache that keys on a mangled string --
  `Compiler::functions`, `Compiler::fn_ref_thunks`,
  `LLVMTypeCache::monomorphized`, `LLVMTypeCache::enum_variant_payloads`,
  `TypeLayouts::mono_struct_info`, `TypeLayouts::mono_enum_variants` --
  plus every `mangled_*` field across `resolved::{calls, processes,
  fields, loops, construction, patterns, methods, enums}`. Pure type-only
  refactor: 25/25 lang-suite tests pass. Sets up Wave 10's monomorphization
  registry to use stable typed identifiers from day one.
- **Wave 10 -- monomorphization registry + final Phase 4b lifts.**
  Introduced declaration-level IR containers in `expo_ir::program`:
  `IRProgram` (a flat insertion-ordered collection keyed by typed
  identifiers), `IRStruct`, `IREnum`, `IRFunction`, plus `IRStructKind`
  / `IRFunctionKind` for stdlib-intrinsic vs. user-source dispatch.
  Added pure-semantic monomorphization planners in
  `expo_ir::lower::monomorphize` (`monomorphize_struct`,
  `monomorphize_enum`, `monomorphize_function`,
  `monomorphize_impl_method`) that take `&LowerCtx` + `&mut IRProgram`
  and append `IR*` declarations in dependency order. `Compiler` grew
  an `ir: IRProgram` field plus a `lower_ctx_and_ir(&mut self)` helper
  that hands out the disjoint borrows for plan-then-emit; the original
  `monomorphize_*` functions in `expo-codegen::generics` are now thin
  shims that drive the IR planner first and then call new
  `emit_ir_struct` / `emit_ir_enum` / `emit_ir_function` /
  `emit_ir_impl_method` functions to lower the recorded `IR*` decl to
  LLVM. Lifted the last two Phase 4b resolvers
  (`resolve_method_call` -> `expo_ir::lower::methods`,
  `resolve_static_call` -> `expo_ir::lower::calls`) along with the
  type-inference helpers they depended on
  (`infer_arg_expo_type`, `expand_mangled_arg_type`,
  `lookup_method_type_params`, `infer_method_type_args`,
  `infer_static_struct_type_args_from_args`,
  `infer_static_method_return_type`) into a new
  `expo_ir::lower::inference` module; the resolvers return
  `ResolvedMethodCall` / `ResolvedStaticCall` with optional
  `PendingMethodMono` / `PendingTypeMono` payloads that the
  `expo-codegen` caller drains against the monomorphize shims before
  the LLVM `FunctionValue` lookup. Renamed the existing forward-looking
  IR sketch from `IrModule` / `IrFunction` / `IrType` to `IRProgram` /
  `IRFunction` / `IRType` so the design and the code share one casing
  convention. 25/25 lang-suite tests pass. Phase 4b closes here; the
  next handoff is Phase 4c.
- **Wave 11 -- Phase 4c slice 1: `compile_unless` lift + block /
  terminator vocabulary.** First instruction-level lowering. Picked
  the smallest control-flow construct in the codebase (`unless` --
  no phi, no value crossing blocks, ~38 LOC of LLVM emission) as a
  discovery vehicle to land the minimum block/terminator vocabulary
  Phase 4c needs. Introduced `IRBlockId(u32)` (function-scoped, opaque
  identifier minted by the new `FnLowerState::next_block_id`),
  `IRBasicBlock { id, label, terminator }`, and `IRTerminator { Branch,
  CondBranch { cond: Box<Expr>, then, otherwise }, Unreachable }` in
  a new `expo_ir::blocks` module. Added `IRUnless` in
  `resolved::conditionals` and the pure-semantic `lower_unless` in
  `lower::conditionals`. Refactored `compile_unless` in
  `expo-codegen::control::conditionals` into a thin shim plus an
  `emit_unless` walker that materializes LLVM blocks from a
  `HashMap<IRBlockId, BasicBlock<'ctx>>` and dispatches through a
  new shared `emit_terminator` helper in `control::terminator`. The
  helper interprets `IRTerminator` uniformly across constructs and
  is the dispatch point that future conditional-construct lifts
  (slice 2+) will reuse.

  Architectural commitment: control-flow negation is canonicalized
  into branch-target ordering. The previous `build_not(cond)` LLVM
  call is gone; "unless-ness" is encoded entirely by `lower_unless`
  placing `merge_block` on `then` and `body_block` on `otherwise`.
  No `Not` operator and no `negated` flag exist anywhere in the IR.
  Cond-branch emission performs zero per-construct branch-direction
  knowledge -- the cond AST is coerced to i1 and routed to the
  terminator's `then` / `otherwise` slots in declared order. This
  keeps the cond-branch shape uniform across every conditional
  construct in the language and removes a peephole-fold cost from
  every backend.

  Slice 1 explicitly defers: the cond AST (still walked by
  `compile_expr` inside `emit_terminator`) and the body statements
  (still walked by `compile_statement` inside `emit_unless`) remain
  AST stubs. These are slice 2's natural targets. The endpoint
  landed at is "B" from the discovery framing -- terminator-explicit
  -- because the negation analysis showed slice 2's `emit_if` would
  otherwise reimplement equivalent cond-branch knowledge. The
  promotion from the implicit Endpoint A was small (one new enum,
  three variants) and pays off the moment a second construct lifts.
  25/25 lang-suite tests pass.
- **Wave 12 -- Phase 4c slice 1.5: operand model + literal
  fast-path.** Foundational pre-work for slice 2 (`compile_if` no
  else): introduced `IRValueId(u32)` (function-scoped, opaque,
  minted by a new `FnLowerState::next_value_id` mirroring the
  `block_counter`), `IROperand` (`ConstBool` / `ConstFloat` /
  `ConstInt` / `ConstStr` / `Local(IRValueId)` / `Unit`), and
  `IRInstruction` with a single transitional `Stub { dest, expr }`
  variant in a new `expo_ir::values` module. Migrated
  `IRTerminator::CondBranch::cond` from `Box<Expr>` to `IROperand`
  and added `instructions: Vec<IRInstruction>` to `IRBasicBlock`
  (forward-compat; the `IRUnless` shape today carries
  `entry_instructions` directly).

  The single seam between expression-shaped values and the IR is the
  new construct-agnostic helper
  `expo_ir::lower::values::lower_expr_to_operand(state,
  &mut instructions, expr) -> IROperand`: literal expressions return
  inline operand constants emitting no instructions, every other
  expression mints a value id and pushes one `IRInstruction::Stub`
  onto the caller's sequence. Slice 2's `lower_if` will reuse this
  helper unchanged. Updated `lower_unless` to call it, and updated
  `emit_unless` / `emit_terminator` to (a) walk a block's
  `instructions` populating a `HashMap<IRValueId, BasicValueEnum<'ctx>>`,
  and (b) materialize `IROperand` to LLVM via that map plus inline
  literal constant materialization on the LLVM context.

  Why the transitional `Stub` variant: the alternative -- block every
  operand-shaped slot until the entire instruction set is defined --
  forces a single mega-slice that designs the IR against speculation
  rather than real consumers. A side table was considered (and
  rejected) for the bridge: side tables divorce execution order from
  the instruction stream and require the consumer to consult two
  stores. A first-class `Stub` variant keeps the stream
  single-source-of-truth and gives the migration a clear, greppable
  retirement marker (`IRInstruction::Stub`). All stdlib + lang-suite
  tests pass, including `lib/std/test/control_flow_test.expo`'s
  `test_unless` which exercises both the literal fast path
  (`unless true` / `unless false`) and the Stub fallback (`unless
  x > 20`).

### Next: Phase 4c slicing plan

The block/terminator vocabulary landed in Wave 11 is sized for the
slice ladder. Each subsequent slice picks the next-smallest construct
and extends the IR only where its consumer requires:

- **Slice 2 -- `compile_if` (no else).** Reuses
  `IRTerminator::CondBranch` with body on `then` and merge on
  `otherwise`. Reuses the Wave 12 operand model
  (`lower_expr_to_operand` + the per-block instruction sequence)
  unchanged. No new IR types expected; validates the canonicalized
  branch shape across two constructs and exercises the operand
  model's claim that the seam is construct-agnostic.
- **Slice 3 -- `compile_if` with else / `compile_ternary`.** Introduces
  the value-merging story (`IRPhi` or equivalent), forcing the
  predecessor-handle question that slice 1 deliberately sidestepped
  by choosing a phi-free construct.
- **Slice 4 -- `compile_cond`.** N-arm enumeration; same
  `CondBranch` shape but a dynamic chain of test blocks. Tests the
  IR scaffold scaling beyond fixed-N constructs.
- **Slice 5+ -- `compile_match`, `compile_while`, `compile_loop`.**
  Pattern bindings (variable-scope save/restore in IR vs. emission),
  loop headers, break/continue. By the time these lift, the operand
  model and value-producing instruction set will be filling in from
  slices 2-4 demand.

When the ladder completes, `IRFunction` carries blocks instead of an
AST, `expo-codegen` becomes a pure consumer of `IRProgram`, and the
`Lowerer<'a>` driver from the early plan finally has somewhere to live.

### Why incremental over big-bang

- Tests pass at every step (no "nothing works for 4 weeks" phase).
- Phase 1 (done) unblocked C FFI without waiting for the full IR.
- Natural IR discovery -- instructions emerge from real code, not speculation.
- Total effort ~4-6 weeks, comparable to big-bang but with continuous progress.
