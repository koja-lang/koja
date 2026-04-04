# Self-Hosted Compiler Architecture

Design notes for the Expo self-hosted compiler. The Rust bootstrap compiler
proves the language works; the self-hosted compiler proves the language is
expressive enough to build itself -- and should be better than the Rust version,
not just equivalent.

---

## Core insight: separate reads from accumulations

The Rust type checker threads `&mut TypeContext` through every function. This
creates a god object -- 16 HashMaps behind a single mutable reference. Every
function takes `ctx: &mut TypeContext` even when it only needs to read struct
definitions and push a diagnostic. `&mut` locks the entire struct: you can't
read `ctx.structs` while another function writes to `ctx.diagnostics`.

This causes a cascade of workarounds:

- `CheckEnv` exists as a separate struct because you need `&mut TypeContext`
  and `&mut CheckEnv` simultaneously -- if the env were inside TypeContext,
  you couldn't take two `&mut` borrows.
- `struct_names: Vec<String> = ctx.structs.keys().cloned().collect()` appears
  in 5 places because you must pre-extract keys before the mutable borrow
  starts.
- `struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect()`
  follows each extraction because downstream APIs want `&[&str]`.
- `CheckEnv` carries `struct_names: &'a [&'a str]` with a lifetime parameter,
  infecting every function that touches it.

The fix is architectural: **TypeContext is read-only after collection.
Everything written during checking is an accumulation that never feeds back
into the checking logic.**

Reads (never change during checking):

- `ctx.structs` -- struct field types and metadata
- `ctx.enums` -- enum variant info
- `ctx.functions` -- function signatures
- `ctx.protocols` -- protocol method signatures
- `ctx.type_aliases`, `ctx.generic_*_asts`, etc.

Accumulations (never read back during checking):

- `ctx.diagnostics` -- errors and warnings
- `ctx.closure_captures` -- captured variable info for codegen
- `ctx.coercions` -- numeric coercion info for codegen

The accumulations become separate actors -- tiny processes that receive
fire-and-forget messages:

```
actor DiagnosticCollector
  items: List<Diagnostic>

  fn handle(move self, msg: DiagMsg, from: Option<Ref<List<Diagnostic>>>) -> Self
    match msg
      Emit{diag} -> self.items.push(diag)
      EmitHint{message, hint, span} ->
        self.items.push(Diagnostic{
          severity: Severity.Error,
          message: message,
          hint: Option.Some(hint),
          span: span,
        })
    end
    from.map(fn (move f: Ref<List<Diagnostic>>) -> ()
      f.send(self.items.clone())
    end)
    self
  end
end
```

With accumulators split out, TypeContext is immutable. Immutable data in Expo
can be freely borrowed. Free functions read it without conflict:

```
fn infer_expr(expr: Expr, ctx: TypeContext, diags: Ref<DiagMsg, List<Diagnostic>>) -> Type
  match expr
    Expr.Binary{op, left, right, span} ->
      left_type = infer_expr(left, ctx, diags)
      right_type = infer_expr(right, ctx, diags)
      unless types_compatible(left_type, right_type)
        diags.cast(Emit{Diagnostic{
          severity: Severity.Error,
          message: "type mismatch: #{left_type.display()} vs #{right_type.display()}",
          span: span,
        }})
      end
      resolve_binary_result(op, left_type, right_type)
  end
end
```

`ctx` is borrowed (read-only) -- every function reads struct definitions,
function signatures, enum variants directly. No extraction, no cloning, no
lifetime workarounds. `diags` is a `Ref` -- fire-and-forget `cast`, no
blocking.

---

## What this eliminates from the Rust codebase

| Rust workaround                                           | Lines | Status in Expo                                    |
| --------------------------------------------------------- | ----- | ------------------------------------------------- |
| `CheckEnv` struct, lifetime, methods                      | ~150  | Gone (fields become local or on collector actors) |
| `struct_names`/`enum_names` extraction (x5 call sites)    | ~50   | Gone (read `ctx.structs` directly)                |
| `ctx: &mut TypeContext` in 31 function signatures         | ~31   | `ctx: TypeContext` (borrowed, no `&mut`)          |
| `ce: &mut CheckEnv` in 20+ function signatures            | ~20   | Gone                                              |
| `CheckEnv` construction at each function entry            | ~60   | Gone                                              |
| `Box<T>` in AST + deref in all pattern matches            | ~100+ | Gone (compiler handles heap placement)            |
| `#[derive(Debug, Clone, ...)]` on all types               | ~80   | Gone                                              |
| `pub` on all struct fields                                | ~120  | Gone (public by default)                          |
| `Compiler<'ctx>` lifetime param in 80+ codegen signatures | ~80   | Gone (no lifetime annotations)                    |

~700 lines of pure ceremony removed. The Rust compiler is ~27,700 lines; the
self-hosted compiler should land around 50-55% of that (~14,000-15,000 lines)
for equivalent functionality.

---

## Actor-per-module architecture

Inspired by Elixir's `Kernel.ParallelCompiler`, which spawns one Erlang process
per source file. Modules compile in parallel; dependencies block via message
passing; crashes in one module don't affect others.

```
Supervisor
├── TypeRegistry actor         -- collects exported types from all modules
├── ModuleChecker (file_a)     -- parse + collect + check
├── ModuleChecker (file_b)     -- blocks on file_a's exports via registry
├── ModuleChecker (file_c)     -- parse + collect + check
└── CodegenPipeline actor      -- receives checked modules, emits LLVM IR
```

Each module checker:

1. Parses the source file.
2. Collects type signatures (functions, structs, enums) into a local
   TypeContext.
3. Publishes its exports to the TypeRegistry.
4. Requests imported module types from the TypeRegistry (blocks until
   available).
5. Merges imports into its local TypeContext (now read-only).
6. Type-checks all function bodies, sending diagnostics to collectors.
7. Sends the checked module + TypeContext to CodegenPipeline.

```
actor ModuleChecker
  stdlib_ctx: TypeContext

  fn handle(move self, msg: CheckRequest, from: Option<Ref<CheckResult>>) -> Self
    match msg
      Check{source, module_name, registry, diags} ->
        parsed = parse(source)
        ctx = collect(parsed.module)
        merge_stdlib(self.stdlib_ctx, ctx)

        for dep in parsed.imports
          dep_ctx = registry.call(GetModule{name: dep}, 30000)
          match dep_ctx
            Some(dep_ctx) -> merge_imports(ctx, dep_ctx)
            None -> diags.cast(Emit{Diagnostic{
              severity: Severity.Error,
              message: "unresolved import: module `#{dep}` not found",
              span: parsed.module.span,
            }})
          end
        end

        check_module(parsed.module, ctx, diags)
        from.map(fn (move f: Ref<CheckResult>) -> ()
          f.send(CheckResult{module: parsed.module, ctx: ctx})
        end)
    end
    self
  end
end
```

### Improvement over Elixir: no ETS escape hatch

Elixir's compiler uses ETS tables (concurrent hash maps outside the process
model) for the module registry because BEAM deep-copies all messages. Sending a
full TypeContext between processes would copy every struct definition, function
signature, and protocol implementation.

Expo doesn't need this. `send(registry, ModuleReady{name, move ctx})` transfers
ownership with zero copies. The registry actor owns all module contexts. Import
resolution is message passing, not shared mutable state.

### Codegen pipeline

Parsing and type-checking parallelize naturally (one actor per file). Codegen
converges to a pipeline because LLVM needs a single context for cross-module
optimization, function declarations, and struct type registration.

Checked modules stream into the codegen actor as they finish. Codegen can start
emitting IR for module A while modules B and C are still being type-checked:

```
ModuleChecker(A) ──done──► CodegenPipeline ──► LLVM Module ──► Object File
ModuleChecker(B) ──done──►       ▲
ModuleChecker(C) ──done──►       │
                          (processes in order)
```

### Parallel function body checking

Within a single module, independent function bodies can be checked in parallel.
After collection (which builds the shared TypeContext), each function body check
only reads the context and writes to collectors:

```
fn check_module(module: Module, ctx: TypeContext, diags: Ref<DiagMsg, List<Diagnostic>>)
  for func in module.functions()
    spawn check_function(func, ctx.clone(), diags)
  end
end
```

Each spawned check borrows (or clones) the read-only TypeContext and has its own
local variable environment. Diagnostics flow to the shared collector actor.
No locks, no coordination, no data races.

---

## LSP: the compilation topology persists

The batch compiler's actor topology maps directly onto the LSP. Module checker
actors stay alive between edits instead of being spawned and discarded:

```
LSP Backend
├── StdlibContext (loaded once)
├── DocumentActor (file_a.expo)  -- owns DocumentState, handles edits
├── DocumentActor (file_b.expo)  -- notified when file_a's exports change
├── DocumentActor (file_c.expo)  -- independent, unaffected by A/B changes
└── TypeRegistry                 -- persistent module type cache
```

On file change, the LSP sends a `Recheck` message to that file's actor. The
actor re-parses, re-checks, publishes new diagnostics, and notifies dependents
via the registry. Other files' actors are unaffected -- no global lock, no
re-reading every import from disk.

Compare to the Rust LSP backend:

```rust
// Current: one lock for everything
pub struct Backend {
    pub documents: Arc<RwLock<HashMap<String, DocumentState>>>,
    // Every operation contends on this lock
}
```

The actor model eliminates `Arc<RwLock<HashMap<...>>>` entirely. Each document's
state is owned by its actor. Typing in file A sends messages to A's actor;
hovering in file B goes to B's actor. They run concurrently without shared
mutable state.

### Crash isolation

If type-checking a malformed file panics, the actor crashes and its supervisor
restarts it. Other documents are unaffected. In the Rust LSP, a panic in any
handler risks taking down the process (or requires `catch_unwind`, which doesn't
work across `.await` points).

### Incremental dependency tracking

The actor topology IS the dependency graph. Module A imports Module B? A's actor
knows B's actor via the registry. When B changes, the registry notifies A. No
need to re-scan imports from disk on every keystroke.

---

## Conciseness: where the lines go

Beyond the architectural wins, Expo syntax eliminates per-line ceremony that
compounds across the codebase:

**No `Box<T>` for recursive types.** The Rust AST has `Box<Expr>` in 14+ places.
Every match arm dereferences through boxes. Every constructor boxes. Expo handles
heap placement automatically.

**No lifetime annotations.** `Compiler<'ctx>` appears in 80+ function signatures
in the codegen. In Expo, the LLVM context outlives the compiler by construction.

**No `iter()` / `into_iter()` / `iter_mut()` selection.** Expo's `for` loop
borrows by default. `move` to consume.

**No `.as_str()` / `.as_ref()` / deref coercion friction.** Expo has one string
type. No `String` vs `&str` conversion.

**Union type aliases vs wrapper enums.** `type Item = Constant | EnumDecl | ...`
is one line. Rust needs a full enum with wrapper variants plus unwrapping at
every use site.

**String interpolation for diagnostics.** Hundreds of `format!()` calls become
inline `"#{expr}"` interpolations.

---

## Honest tradeoffs

### The `diags` parameter is still threaded

The accumulator actor eliminates `&mut TypeContext` but introduces
`diags: Ref<DiagMsg, List<Diagnostic>>` -- still a parameter passed through
every function. This is lighter than the Rust version (one simple Ref vs two
mutable god-object references), but it's not zero. The threading is:

```
Rust:    fn infer_expr(expr: &Expr, ctx: &mut TypeContext, ce: &mut CheckEnv) -> Type
Expo:    fn infer_expr(expr: Expr, ctx: TypeContext, diags: Ref<DiagMsg, List<Diagnostic>>) -> Type
```

Same parameter count, but `ctx` is read-only (no borrow conflicts) and
`CheckEnv` is gone (variable env is local to each function body check).

### No `&mut` means different patterns for sequential mutation

Expo has no mutable borrows. For the variable environment (per-function
mutable state during checking), the pattern is move-and-return:

```
fn check_body(stmts: List<Statement>, ctx: TypeContext, move env: Env, diags: Ref<DiagMsg, List<Diagnostic>>) -> Env
  for stmt in stmts
    env = check_statement(stmt, ctx, env, diags)
  end
  env
end
```

This is explicit about data flow but more verbose than Rust's `&mut env`. The
env is small and local (variable bindings for one function body), so the cost
is manageable.

### Lifetime safety for LLVM FFI

Rust's `Compiler<'ctx>` lifetime guarantees at compile time that no LLVM value
outlives its context. Expo can't express this. The self-hosted codegen must rely
on structural guarantees (the context lives for the entire compilation) rather
than type-level enforcement. For FFI-heavy code, this is a real loss in static
safety.

### Exhaustive match during bootstrap

Rust's compiler enforces exhaustive matches. Adding a new AST variant
immediately shows every match that needs updating. The self-hosted compiler
gets this guarantee only if its own exhaustiveness checker works correctly --
a bootstrap chicken-and-egg. The Rust compiler catches bugs in the self-hosted
compiler that the self-hosted compiler can't yet catch in itself.

### Ecosystem

The Rust compiler has `inkwell` (safe LLVM bindings), `tower-lsp` (LSP
framework), and thousands of compiler-related crates. The self-hosted compiler
will need to build LLVM bindings, LSP infrastructure, and supporting libraries
from scratch or through C FFI.
