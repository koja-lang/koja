# Entry Points, Inline Functions, and the `fn main` Problem

## Problem

`fn main` is the only free-floating function in Expo. All other functions must live inside `impl` blocks. This creates a fractal design inconsistency: the first thing every user learns is the one pattern that doesn't generalize. AI agents hit this hard -- they see `fn main` and assume free functions work everywhere, leading to codegen crashes when they try.

The language has three related open questions:

1. **Where does `fn main` live?** (IMPORT.md, lines 585-595)
2. **Should `impl Type` migrate to inline functions in type bodies?** (ROADMAP.md, lines 514-521, 538-542)
3. **How does `fn main` relate to `Process<C, M, R>`?** (PROCESS.md, lines 256-281; ROADMAP.md, lines 304-312)

This doc proposes a unified answer to all three.

---

## Two modes: projects and scripts

Expo has two execution modes with different file extensions and different semantics.

### `.expo` files -- structured modules

`.expo` files contain type definitions, `impl` blocks, constants, and protocol conformance. No top-level statements. No `fn main`. Every function lives on a type. These are the files that make up a compiled project.

### `.exps` files -- scripts

`.exps` files are scripts. The entire file body executes as a function -- like a bash script. Top-level statements, inline type definitions, no boilerplate. Run with `expo run script.exps`.

```expo
# hello.exps
IO.puts("Hello, Expo!")
```

This is Expo's equivalent of Elixir's `.exs` files. Scripts are for exploration, one-offs, and learning. They use real Expo semantics -- same types, same ownership, same `impl` blocks -- but the file itself is the entry point.

### Why two modes

The split exists because projects and scripts have different needs:

- **Projects** need structure, consistency, and the full `Process` model. The entry point is a type specified in `expo.toml`. No special cases.
- **Scripts** need zero ceremony. The file is the function body. No project config, no type scaffolding, no process protocol.

Conflating the two (allowing top-level statements in `.expo` files, or requiring `Process` in scripts) would compromise both.

---

## Proposal: project entry points are `Process` implementations

### Status: partially implemented

The dual entry mode is implemented. The `entry` field in `expo.toml` determines behavior by casing:

- **lowercase** (`entry = "main"`) -- existing behavior: resolves to a module file containing `fn main`
- **PascalCase** (`entry = "App"`) -- new behavior: names a type that implements `Process<C, M, R>`, codegen generates a C `main` that constructs and spawns it

Both modes coexist. Existing projects and tests are unchanged. The `fn main` path will be removed once `.exps` scripts are implemented (see below).

### `expo.toml`

```toml
[project]
entry = "App"
name = "my_app"
version = "0.1.0"
```

The `entry` field names the type whose `Process` implementation the runtime invokes at startup.

### Entry type

```expo
# src/app.expo
struct App
  name: String
end

enum AppMsg
  Greet(String)
end

impl Process<App, AppMsg, ()> for App
  fn new(config: App) -> Self
    config
  end

  fn handle(move self, msg: AppMsg, from: Option<ReplyTo<()>>) -> Self | StopReason
    match msg
      AppMsg.Greet(who) ->
        IO.puts("Hello, #{who}!")
        self
    end
  end

  fn handle_lifecycle(move self, event: Lifecycle) -> Self | StopReason
    match event
      Lifecycle.Shutdown ->
        IO.puts("Shutting down #{self.name}")
        StopReason.Shutdown
      _ -> self
    end
  end
end
```

### What this gives you

- **argv as config (implemented):** when `C = List<String>`, the runtime converts `argc`/`argv` (skipping the program name) into an Expo `List<String>` and passes it to `new`. Other config types are zero-initialized.
- **OS signals as lifecycle events (deferred):** `Lifecycle.Shutdown`, `Lifecycle.Interrupt`, `Lifecycle.Reload` are dispatched to `handle_lifecycle`. Graceful shutdown is an override of the default implementation. Not yet implemented -- requires a two-mailbox model in the runtime.
- **Exit codes via protocol (implemented):** the entry type's `StopReason` implements `ExitStatus`, mapping to OS exit codes. Default: `Normal -> 0`, `Shutdown -> 1`. Custom exit codes via `ExitStatus` protocol implementation. The spawn wrapper captures `run`'s return value, calls `ExitStatus.code()`, and stores the result in a global `@__expo_exit_code` that C `main` reads after `expo_rt_main_done()`.
- **Supervision:** the entry type can spawn child processes in `new`. `expo new --sup` scaffolds a supervision tree.

### No special cases

The entry type is just a type. It uses the same `Process` protocol as every other process. There's no `fn main`, no free-floating functions, no "except for the entry point" asterisk. The fractal design is preserved -- the program's top-level structure is the same as any spawned process.

---

## Process lifecycle

The entry type example above uses `Lifecycle`, `StopReason`, and `handle_lifecycle`. This section defines those types and explains how they connect OS signals, process shutdown, and exit codes.

### Lifecycle

```expo
enum Lifecycle
  Shutdown
  Interrupt
  Reload
end
```

`Lifecycle` abstracts OS signals into a platform-agnostic enum. On POSIX systems: `SIGTERM` maps to `Shutdown`, `SIGINT` to `Interrupt`, `SIGHUP` to `Reload`. Non-POSIX runtimes map their own shutdown/interrupt mechanisms to the same variants.

The entry process receives `Lifecycle` events from the OS runtime. Supervised children receive them from their supervisor. Same type, different sender.

### `handle_lifecycle`

The `Process` protocol has two dispatch points: `handle` for business messages, `handle_lifecycle` for lifecycle events.

```expo
protocol Process<C, M, R>
  fn new(config: C) -> Self

  fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Self | StopReason

  fn handle_lifecycle(move self, event: Lifecycle) -> Self | StopReason
    match event
      Lifecycle.Shutdown -> StopReason.Shutdown
      Lifecycle.Interrupt -> StopReason.Shutdown
      Lifecycle.Reload -> self
    end
  end
end
```

The mailbox internally accepts `M | Lifecycle`. The runtime dispatches `M` messages to `handle` and `Lifecycle` events to `handle_lifecycle`. This is an implementation detail -- the user declares `M`, not `M | Lifecycle`.

`handle_lifecycle` has a default implementation: stop on `Shutdown`/`Interrupt`, ignore `Reload`. Most processes never override it. Override for graceful drain, hot config reload, or custom shutdown sequencing.

This mirrors `run`, which also has a default implementation that most processes never touch. The pattern is: sensible defaults with opt-in customization.

### StopReason

```expo
enum StopReason
  Normal
  Shutdown
end
```

`handle` and `handle_lifecycle` both return `Self | StopReason`. Returning `Self` continues the process. Returning a `StopReason` variant stops it.

- `Normal` -- the process finished its work (e.g., a `Task` that completed its computation).
- `Shutdown` -- the process was told to stop (lifecycle event, supervisor directive).

There is no `Error` variant. Unrecoverable errors are panics -- the process crashes and the supervisor handles it (see [Error handling philosophy](#error-handling-philosophy)).

### ExitStatus

```expo
protocol ExitStatus
  fn code(self) -> Int
end
```

`ExitStatus` maps a stop reason to an OS exit code. Only the entry process needs this -- it's the boundary between the Expo runtime and the OS.

`StopReason` has a default `ExitStatus` implementation: `Normal -> 0`, `Shutdown -> 1`. User types can implement `ExitStatus` for custom exit codes (e.g., distinguishing between graceful shutdown and specific failure modes).

---

## Supervision

### ExitReason

```expo
enum ExitReason
  Normal
  Shutdown
  Crashed(String)
end
```

`ExitReason` is what a supervisor sees when a child process stops. It is type-erased -- the supervisor manages heterogeneous children with different `M` and `R` types, so it needs a uniform view of termination.

- `Normal` / `Shutdown` map directly from `StopReason`.
- `Crashed(String)` captures the panic message when a child process crashes.

### Shutdown propagation

The supervisor stores an opaque handle per child -- a closure that captures the child's `Ref` and sends `Lifecycle` into the child's mailbox:

```expo
struct ChildHandle
  send_lifecycle: fn (Lifecycle) -> ()
end
```

When the supervisor spawns a child with `Ref<DbPoolMsg, R>`, it captures `fn (event: Lifecycle) -> () ref.cast(event) end` and stores it. The supervisor doesn't know `M` -- it only sends `Lifecycle`, never business messages.

On shutdown, the supervisor iterates children, calls `child.send_lifecycle(Lifecycle.Shutdown)` for each. The child's runtime dispatches to `handle_lifecycle`, which returns `StopReason.Shutdown` (default) or runs custom cleanup before stopping.

### Fractal dispatch

The lifecycle mechanism is the same at every level:

- **OS runtime** sends `Lifecycle` to the entry process mailbox, dispatched to `handle_lifecycle`.
- **Supervisor** sends `Lifecycle` to child process mailboxes, dispatched to `handle_lifecycle`.

Same mechanism, different sender. The entry process is not special -- it's just a process whose lifecycle events happen to come from the OS instead of a supervisor.

### Pid scoping

`Pid` is an internal implementation detail of `Supervisor` and `Process.monitor`. It is not a user-facing type in normal application code. Users interact with processes through typed `Ref<M, R>` handles, which provide compile-time safety for message passing.

### Process discovery

Processes find each other through refs, not names. There are three patterns, matched to the supervisor type:

**Static supervisor:** children are known at compile time. The supervisor holds each child's `Ref` as a struct field and passes refs to siblings through config:

```expo
struct AppSupervisor
  db: Ref<DbMsg, DbResult>
  cache: Ref<CacheMsg, CacheResult>
end

# WebServer receives the db ref through its config -- no lookup needed
struct WebConfig
  db: Ref<DbMsg, DbResult>
end
```

No strings, no registry. Struct fields are the names. Typo `self.dv` instead of `self.db`? Compiler error. Wrong message type? Compiler error.

**DynamicSupervisor:** children are created at runtime (one per connection, one per job). The supervisor is type-parameterized -- all children share the same `M` and `R` types. `start_child` returns the `Ref` to the caller, who stores it in their own state:

```expo
struct DynamicSupervisor<C, M, R>
  fn start_child(move self, config: C) -> Pair<Self, Ref<M, R>>
  fn stop_child(move self, ref: Ref<M, R>) -> Self
end
```

The DynamicSupervisor's job is supervision (start, monitor, restart), not lookup. The caller manages its own mapping (e.g., connection ID to ref).

**Global registry (optional):** for well-known cross-cutting singletons (logger, metrics) where config threading is awkward. Uses constant strings to avoid typos:

```expo
const DB_POOL = "db_pool"

Process.register(ref, DB_POOL)

fn db_pool() -> Option<Ref<DbMsg, DbResult>>
  Process.whereis<DbMsg, DbResult>(DB_POOL)
end
```

The constant is defined once; the typed lookup helper pins both the name and the types. Most applications won't need the global registry -- refs through config and DynamicSupervisor cover the common cases.

---

## Error handling philosophy

Expo's error model has three layers, each progressively less precise:

1. **Types are the first line of defense.** `Result<T, E>`, `Option<T>`, and exhaustive `match` catch most errors at compile time. If a function can fail, its return type says so. The caller must handle the failure case -- the compiler enforces it.

2. **Crashes are the last resort.** Panics are for truly unrecoverable situations -- violations of invariants that the type system cannot express. There is no `StopReason.Error` variant because recoverable errors should be modeled as types, not process termination.

3. **Supervision is the safety net.** When a process does crash, the supervisor sees `ExitReason.Crashed(msg)` and restarts it according to the configured strategy. This handles the cases that types and careful programming cannot predict.

This differs from Erlang's "let it crash" philosophy. In Erlang, crashes are routine -- processes crash and restart as a normal control flow mechanism. In Expo, static types mean most error conditions are handled at compile time. Crashes should be rare, not routine. Supervision exists for the genuinely unexpected, not as a substitute for error handling.

---

## Proposal: inline functions on types

Functions can be defined directly inside `struct` and `enum` bodies. `impl` becomes the mechanism for *extending* a type from outside (other files, protocol conformance).

### Struct with inline functions

```expo
struct Parser
  input: String
  pos: Int

  fn new(move input: String) -> Self
    Parser{input: input, pos: 0}
  end

  fn parse(move self) -> Self
    self = self.skip_whitespace()
    self
  end

  priv fn skip_whitespace(move self) -> Self
    # ...
    self
  end
end
```

### Enum with inline functions

```expo
enum Token
  Num(Float)
  Plus
  Minus
  Star
  Slash

  fn is_operator?(self) -> Bool
    match self
      Num(_) -> false
      _ -> true
    end
  end
end
```

### `impl` as extension

`impl` survives for two purposes:

1. **Protocol conformance:** `impl Protocol for Type` -- adding protocol methods to a type.
2. **Cross-file extension:** `impl Type` in a different file -- adding methods from outside the defining file.

```expo
# protocol conformance
impl Debug for Parser
  fn format(self) -> String
    "Parser{pos: #{self.pos}}"
  end
end

# cross-file extension (in another file)
impl Parser
  fn from_file(path: String) -> Self
    content = File.read(path).unwrap()
    Parser.new(content)
  end
end
```

### Migration path

Existing `impl Type` blocks in the same file as the type definition can be inlined. The compiler could support both forms during a transition period, or bare `impl Type` in the same file could become a warning.

### Visibility

`priv fn` inside a type body means private to that type -- only callable from the type's own methods, same as today in `impl` blocks.

---

## Scripts (`.exps`)

### Semantics

A `.exps` file is a function body. The compiler wraps its contents into an anonymous entry point for codegen. This is an implementation detail -- the user just writes statements.

```expo
# calculator.exps
struct Calc
  result: Float

  fn add(move self, n: Float) -> Self
    self.result += n
    self
  end
end

c = Calc{result: 0.0}
c = c.add(10.0)
c = c.add(32.0)
IO.puts("Result: #{c.result}")
```

Scripts can define types inline and use them immediately. Types defined in a script are scoped to the script -- they don't leak anywhere.

### Running scripts

```bash
expo run hello.exps       # compile and execute
```

No `expo.toml` needed. No project structure. The file extension `.exps` tells the compiler to use script mode.

### Script limitations

- No multi-file support. A script is a single file.
- No dependency imports. Scripts use only the standard library.
- No `@test` annotations. Tests live in projects.
- No `expo build` output. Scripts are run, not compiled to binaries.

### REPL

The REPL (ROADMAP.md, lines 352-359) uses `.exps` semantics. A REPL session is an interactive script:

- Type definitions and statements are entered interactively.
- Types persist across REPL inputs.
- Statements execute immediately.
- Same scoping, same ownership rules, same semantics. No special REPL mode.

---

## Function-scoped types (future exploration)

Since `.exps` files already allow types inside a function body (the script *is* a function body), the fractal design question is: can *any* function body define types?

```expo
struct DataPipeline

  fn process(move self, raw: List<String>) -> List<Record>
    struct RawRow
      line: String
      line_num: Int

      fn parse(self) -> Record
        # ...
      end
    end

    rows: List<RawRow> = List.new()
    # ...
    rows.map(r -> r.parse())
  end
end
```

Function-scoped types would be visible only within the enclosing function body. They could not appear in the function's parameter or return types. This is a natural extension but not required for the core model -- it can be added later without breaking changes.

---

## Impact on `expo new`

`expo new my_app` scaffolds:

```
my_app/
  expo.toml
  src/
    app.expo
```

Where `src/app.expo` is:

```expo
struct App
end

enum AppMsg
end

impl Process<List<String>, AppMsg, ()> for App
  fn new(config: List<String>) -> Self
    App{}
  end

  fn handle(move self, msg: AppMsg, from: Option<ReplyTo<()>>) -> Self | StopReason
    IO.puts("Hello, Expo!")
    self
  end
end
```

`handle_lifecycle` is not shown -- the default implementation is sufficient for a hello world.

And `expo.toml` is:

```toml
[project]
entry = "App"
name = "my_app"
version = "0.1.0"
```

Future: `expo new my_app --sup` scaffolds a supervision tree with child specs in `new`.

---

## Impact on test files

Test files use `@test` annotations on functions inside types. They are `.expo` files with no top-level statements. No change needed.

---

## Cross-references

- **IMPORT.md, lines 585-595:** Open question about removing top-level functions -- this doc answers it. No free functions in `.expo` files. Top-level statements exist only in `.exps` scripts.
- **IMPORT.md, lines 203-205:** Top-level functions in flat scope -- eliminated. All functions on types.
- **PROCESS.md, lines 256-281:** `fn main` vs `Process` split -- resolved. Projects always use `Process`. Scripts use `.exps`.
- **PROCESS.md, lines 304-312 / ROADMAP.md, lines 304-312:** `fn main` as `Process<C,M,R>` -- the entry type *is* the `Process` impl. argv as config, lifecycle events via `handle_lifecycle`, exit codes via `ExitStatus` protocol.
- **ROADMAP.md, lines 514-521:** Open question about `impl Type` migrating to inline functions -- this doc proposes it.
- **ROADMAP.md, lines 538-542:** Type system philosophy, inline functions on types -- addressed here.
- **ROADMAP.md, lines 544-551:** Namespace unification -- compatible, modules and types both own functions.
- **ROADMAP.md, lines 352-359:** REPL design -- uses `.exps` script semantics.
- **PROJECT.md, lines 555-557:** Entry file may have top-level statements -- replaced: `.expo` files never have top-level statements; `.exps` files always do.

---

## Summary

| Today | Proposed |
|-------|----------|
| `fn main` is a special free-floating function | Entry point is a `Process` impl specified in `expo.toml` (TBD: `project.exps`) |
| Functions defined in separate `impl` blocks | Functions defined inline in `struct`/`enum` bodies |
| `impl Type` for primary methods | `impl` reserved for extensions and protocol conformance |
| Free functions disallowed (except `fn main`) | No free functions, period. Consistent rule. |
| Hello world requires `fn main` ceremony | Hello world: `IO.puts("Hello!")` in a `.exps` script |
| One file extension (`.expo`) | Two: `.expo` (structured modules) and `.exps` (scripts) |
| REPL is a separate design question | REPL is `.exps` semantics, interactive |
| OS signals handled ad-hoc (`Signal` placeholder) | `Lifecycle` enum with `handle_lifecycle` default impl on `Process` |
| Exit codes as reply type (`ExitCode` placeholder) | `StopReason` enum, `ExitStatus` protocol for OS exit code mapping |
| No supervision model | `ExitReason` for supervisors, `Lifecycle` propagation, fractal dispatch |

---

## TBD: `project.exps` replacing `expo.toml`

The introduction of `.exps` scripts opens the possibility of replacing `expo.toml` with `project.exps` -- a script that evaluates to a `Project` struct. This would be analogous to Elixir's `mix.exs`: the project config is written in the language itself, with the ability to compute values (e.g., reading a version from a file). The compiler would execute `project.exps` to obtain the `Project` struct before compiling the project. Design details deferred.

### Status: exploration

Not yet implemented. This is a design exploration capturing the intended direction. Implementation would touch the parser, type checker, codegen, CLI, and project scaffolding, plus a migration of the stdlib and test suite.
