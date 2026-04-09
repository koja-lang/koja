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

| Source concept | Lowered to |
| --- | --- |
| Generics (`List<Int32>`) | Monomorphized concrete types |
| Method calls (`p.distance()`) | Direct calls (`Point_distance(p)`) |
| Closures (`fn (x) -> x + n end`) | Environment struct + free function |
| `for` loops | `while` + `.get()` + `.length()` calls |
| `match` on enums | `switch_enum` with payload extraction |
| String interpolation (`"#{x}"`) | `.format()` + `.concat()` calls |
| Field access (`p.x`) | `struct_extract` |
| Struct construction | `struct` instruction |
| Ownership drops | Explicit `drop_value` at scope exits |
| Borrows | Explicit `borrow_value` / `end_borrow` |
| Moves | `move_value` (source becomes dead) |
| Clones | `clone_value` (deep copy, new owner) |
| `self` | Desugared to explicit first parameter |
| `Self` type alias | Resolved to concrete type |
| `impl` blocks | Flattened to free functions with mangled names |

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

| Instruction | Description |
| --- | --- |
| `struct $T (%fields...)` | Construct a struct value |
| `struct_extract %val, #T.field` | Extract a field (typed, no offset math) |
| `enum $T, #Variant [, %payload]` | Construct an enum value |
| `switch_enum %val, case #V: bb...` | Branch on enum tag, deliver payloads |
| `partial_apply @func, %env` | Create a closure from function + environment |
| `apply @func(%args...)` | Call a function |
| `builtin op(%args...)` | Primitive arithmetic/comparison |
| `string_literal "..."` | Create a string constant |

### Ownership operations

| Instruction | Description |
| --- | --- |
| `move_value %val` | Transfer ownership (source becomes dead) |
| `borrow_value %val` | Start a read-only borrow |
| `end_borrow %val` | End a borrow scope |
| `clone_value %val` | Deep copy, new independent owner |
| `drop_value %val` | Free the value (deterministic destructor) |

### Memory operations

| Instruction | Description |
| --- | --- |
| `alloca $T` | Stack allocation |
| `heap_alloc $T` | Heap allocation |
| `load %ptr` | Load from memory |
| `store %ptr, %val` | Store to memory |

### Control flow

| Instruction | Description |
| --- | --- |
| `return %val` | Return from function |
| `cond_br %cond, bb_then, bb_else` | Conditional branch |
| `jump bb` | Unconditional branch |
| `unreachable` | Marks dead code |

### Shared types (future)

| Instruction | Description |
| --- | --- |
| `shared_alloc $T` | Allocate shared (ARC) object, ref count = 1 |
| `shared_retain %val` | Increment reference count (atomic) |
| `shared_release %val` | Decrement reference count, free if zero |
| `shared_read %val` | Begin atomic read access |
| `shared_write %val` | Begin atomic write access |

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

| Compiler | IR levels | Primary motivation |
| --- | --- | --- |
| Rust | AST → HIR → MIR → LLVM IR | Borrow checker operates on MIR |
| Swift | AST → Raw SIL → Canonical SIL → LLVM IR | ARC optimization, generic specialization |
| Go | AST → SSA → machine code | Optimization, no LLVM dependency |
| Expo (current) | Typed AST → LLVM IR | (no IR, lowering and emission interleaved) |
| Expo (proposed) | Typed AST → Raw ExpoIR → Canonical ExpoIR → backend | Ownership optimization, multiple backends, clean codegen |

Apple's primary motivations for SIL, mapped to Expo:

| Apple's reason | Expo equivalent |
| --- | --- |
| ARC optimization (retain/release) | Clone/drop elimination, future shared type ARC |
| Semantic diagnostics (definite init) | Ownership verification, unreachable code |
| Generic specialization | Monomorphization (already needed) |
| Protocol devirtualization | Not needed (Expo is already statically dispatched) |
| Clean separation of concerns | Fixes `c.types.structs`, enables multiple backends |

---

## Timing and implementation strategy

ExpoIR is Phase 6 work in the roadmap, but the typed AST foundation (Phase 5)
is already done. The current crate boundaries (`expo-codegen` depends on
`expo-ast` + `expo-typecheck`) already support the separation.

### Incremental refactoring (preferred over big-bang rewrite)

Rather than building a parallel pipeline from scratch (~9,000-13,000 LOC of
new code before anything works), the preferred strategy is to refactor
`expo-codegen` in-place, splitting lowering from emission incrementally.
Tests pass at every step.

**Phase 1: TypeRegistry API** (~2 days)

`TypeRegistry` currently uses bare-name `HashMap<String, StructType>` keys
(`"Point"`, `"StopReason"`). There are ~10 insert sites and ~70 read sites.
Wrap the raw HashMap behind methods that accept `TypeIdentifier` and
internally key by `"package::TypeName"`. This is mechanical find-and-replace
across ~80 call sites.

This alone fixes the package-qualified type collision problem and unblocks
C FFI without waiting for the full IR.

**Phase 2: Extract decision types** (~1-2 weeks)

Many codegen functions are "heavily mixed" -- semantic decisions interleaved
with LLVM emission. For each, extract the decision into a separate function
that returns a small enum/struct, then the emission code matches on it.

For example, `compile_binary` (183 lines) interleaves "is this float or
int?" with `build_float_add` vs `build_int_add`. Split into:

```rust
enum ResolvedBinaryOp { IntAdd, FloatAdd, IntSub, FloatSub, ... }
fn resolve_binary_op(op: &str, lhs_ty: &Type) -> ResolvedBinaryOp
fn emit_binary_op(op: ResolvedBinaryOp, ...) -> BasicValueEnum
```

These decision types are the seeds of ExpoIR instructions -- the IR grows
organically from working code rather than being designed speculatively.

Current assessment of the ~12,600 LOC in `expo-codegen`:

- ~1,500 LOC is already pure logic (no LLVM): `resolve_closure_params`,
  `infer_method_type_args`, `infer_type_from_expr`, drop analysis, etc.
- ~500 LOC is already pure emission: `compile_literal`,
  `compile_string_concat`, `emit_drop_list`, layout helpers.
- ~1,500 LOC is heavily mixed and needs splitting: `compile_expr`,
  `compile_method_call`, `compile_receive`, `compile_closure_core`,
  `compile_statement`, `compile_binary`, `compile_enum_struct_eq`.
- The remaining ~9,000 LOC is moderately mixed and can be split file-by-file.

**Phase 3: Collect into `expo-ir` crate** (~2-3 days)

Once enough decision types exist, pull them into their own crate. Lowering
and emission stay in `expo-codegen` but the IR types live in `expo-ir`.
Mostly `git mv` and `use expo_ir::*`.

**Phase 4: Move lowering out** (~2-3 weeks, ongoing)

File by file, move the `resolve_*` / decision functions into `expo-ir` as a
lowering pass. Emission stays in `expo-codegen`. Eventually `expo-codegen`
is just "ExpoIR → LLVM" and the lowering lives in `expo-ir`.

### Why incremental over big-bang

- Tests pass at every step (no "nothing works for 4 weeks" phase).
- Phase 1 alone unblocks C FFI (2 days vs 4-6 weeks for the full IR).
- Natural IR discovery -- instructions emerge from real code, not speculation.
- Total effort ~4-6 weeks, comparable to big-bang but with continuous progress.
