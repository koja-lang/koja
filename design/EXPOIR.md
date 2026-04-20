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
   from the typed AST. Backends receive concrete `IrType` values, no string
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
pub struct IrModule {
    pub name: String,
    pub structs: Vec<IrStruct>,
    pub functions: Vec<IrFunction>,
}

pub struct IrFunction {
    pub name: String,
    pub params: Vec<(String, IrType)>,
    pub return_type: IrType,
    pub blocks: Vec<BasicBlock>,
}

pub struct BasicBlock {
    pub label: String,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

pub enum Instruction {
    Struct { dest: Var, ty: IrType, fields: Vec<Operand> },
    StructExtract { dest: Var, base: Operand, ty: IrType, field: String },
    Enum { dest: Var, ty: IrType, variant: String, payload: Option<Operand> },
    PartialApply { dest: Var, func: String, env: Var },
    Apply { dest: Option<Var>, func: String, args: Vec<Operand> },
    Builtin { dest: Var, op: BuiltinOp, args: Vec<Operand> },
    StringLiteral { dest: Var, value: String },

    MoveValue { dest: Var, source: Var },
    BorrowValue { dest: Var, source: Var },
    EndBorrow { value: Var },
    CloneValue { dest: Var, source: Var },
    DropValue { value: Var },

    Alloca { dest: Var, ty: IrType },
    HeapAlloc { dest: Var, ty: IrType },
    Load { dest: Var, ptr: Var },
    Store { ptr: Var, value: Operand },

    // Future: shared types
    SharedAlloc { dest: Var, ty: IrType },
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

pub enum IrType {
    Named(TypeIdentifier),
    Primitive(Primitive),
    Function { params: Vec<IrType>, return_type: Box<IrType> },
    Ref(Box<IrType>),
    Unit,
}
```

~80 lines of type definitions. A codegen backend is a function
`fn emit(module: &IrModule) -> Result<()>` that walks the structure.

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

The compiler does not yet construct or consume a SIL-style IR -- emission
is still synchronous from the typed AST, with no function-level
intermediate value materialized between lowering and codegen. But the
foundation has been substantively built: the `expo-ir` crate exists, a
decision-type vocabulary is extracted and in active use, and the
LLVM-free semantic state and helpers have been lifted off `Compiler`
behind a `LowerCtx<'a>` borrow bundle.

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
  `FnLowerState`, `LowerCtx`, ~43 free functions across 16 modules in
  `lower::{binary, calls, closures, constants, debug, enums, fields,
  loops, mangling, methods, naming, patterns, processes, stmt, strings,
  structs, types}` plus the small `util::parse_int_literal` helper.
  Reference: [`expo/crates/expo-ir/src/lower/`](../crates/expo-ir/src/lower/).
- **Active decision-type vocabulary** (produced + consumed): ~33
  `Resolved*`/`Format*` types across 14 modules in
  `resolved::{calls, closures, constants, construction, debug, enums, fields, loops, match_expr, methods, ops, patterns, processes, strings}`,
  plus 4 pure resolver functions (`resolve_binary_op`, `resolve_unary_op`,
  `resolve_compound_op`, `resolve_string`). Reference:
  [`expo/crates/expo-ir/src/resolved/`](../crates/expo-ir/src/resolved/).
- **Transitional identities**: `VariantId` (today
  `(String, String)`; in Phase 5+ becomes `(EnumId, u8)` with no
  call-site changes).

The IR _instruction set_ -- function/block/instruction containers, ops
on operands, terminators, etc. -- is intentionally undefined in code.
The "Instruction set" section above this one captures the design intent;
the actual containers will be designed bottom-up during Phase 4c, driven
by what `Resolved*` consumers need to be stitched together. An earlier
attempt to define them top-down was deleted because it had no producers
and had already drifted from the `Resolved*` shapes that emerged from
real code paths.

Crate sizes (approximate): `expo-codegen` ~33k LOC, `expo-ir` ~1.2k LOC.

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
- **Phase 4 -- Move lowering out: ~90% done.** Four pure resolvers
  moved to `expo-ir` (in `resolved::ops` and `resolved::strings`); the
  9 Wave 6 helpers, ~28 Wave 7 helpers, and the Wave 8a-8d structural
  splits in `lower::*`. Remaining work is the two monomorphization-bound
  resolvers in Phase 4b and the Phase 4c IR container design:
  - **4b (structural)**: 2 `<'ctx>`-bound resolvers remain
    (`resolve_method_call`, `resolve_static_call`), both blocked on
    monomorphization moving into IR. Lifting them today would require
    a request-payload handshake (the resolver returns a `MonomorphizeRequest`
    that the caller acts on before the LLVM lookup), which re-couples
    emission to a side-channel and was rejected as a band-aid -- the
    cleaner endpoint is monomorphization itself living in `expo-ir`,
    so the two resolvers wait for that work. The other three resolvers
    in this cluster (`resolve_field_ptr`, `resolve_payload_info`,
    `resolve_closure`) are done; see Wave 8d.
  - **4c (the actual handoff)**: design and build the IR instruction
    containers from the bottom up, driven by what `Resolved*` consumers
    need. Lowering produces a function-level IR; emission consumes it
    and walks it. This is where the `Lowerer<'a>` driver becomes real,
    where `closure_site_path` and `package` move off `Compiler` for
    good, and where TCO ambient flags collapse into a `tail` field on
    whatever the call instruction ends up being named.
- **Phase 5+ -- Opaque IR identities: not started.** `VariantId`
  becomes `(EnumId, u8)`; struct names become interned IDs; etc. Pure
  interning work, internal to `expo-ir`, with no call-site changes
  outside the crate.

### Wave history

The 10 waves completed so far, condensed (full prose lives in commit
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
- **Wave 4 -- `enum_variant_payloads` split + `VariantId`.** Variant
  ordering (= tag value) owned solely by `TypeLayouts`; LLVM payload
  table rekeyed from positional `Vec` to identity-keyed
  `HashMap<VariantId, Option<StructType>>`. Drift between the two
  stores is now structurally impossible.
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

### Next: monomorphization-in-IR, then Phase 4c

The two `<'ctx>`-bound resolvers still in the Phase 4b cluster
(`resolve_method_call`, `resolve_static_call`) interleave a pure
decision (mangled name + signature substitution) with `&mut Compiler`
mutation that emits LLVM (`monomorphize_impl_method`,
`monomorphize_struct`, `monomorphize_enum`). Lifting them with a
request-payload handshake was considered and rejected as a band-aid;
the cleaner move is to lift monomorphization itself into IR so both
resolvers fall out as pure functions. After that, Phase 4b closes and
Phase 4c (designing the IR instruction containers bottom-up from real
consumers) can begin.

### Why incremental over big-bang

- Tests pass at every step (no "nothing works for 4 weeks" phase).
- Phase 1 (done) unblocked C FFI without waiting for the full IR.
- Natural IR discovery -- instructions emerge from real code, not speculation.
- Total effort ~4-6 weeks, comparable to big-bang but with continuous progress.
