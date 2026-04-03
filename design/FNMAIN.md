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

`fn main` is removed. The project entry point is a type that implements `Process<C, M, R>`, specified in `expo.toml`.

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

impl Process<App, Signal, ExitCode> for App
  fn new(config: App) -> Self
    config
  end

  fn handle(move self, msg: Signal, from: Option<ReplyTo<ExitCode>>) -> Self
    match msg
      Signal.Term ->
        IO.puts("Shutting down #{self.name}")
        reply(from, ExitCode.Ok)
      _ -> ()
    end
    self
  end
end
```

### What this gives you

- **argv as config:** `C` is the config type. The runtime constructs it from command-line arguments (or a config struct). Details TBD (see PROCESS.md, lines 304-312).
- **OS signals as messages:** `M` includes `Signal.Term`, `Signal.Int`, etc. Graceful shutdown is just another `handle` arm.
- **Exit codes as return values:** `R` is the exit code type. The program exits when `handle` replies.
- **Supervision:** the entry type can spawn child processes in `new`. `expo new --sup` scaffolds a supervision tree.

### No special cases

The entry type is just a type. It uses the same `Process` protocol as every other process. There's no `fn main`, no free-floating functions, no "except for the entry point" asterisk. The fractal design is preserved -- the program's top-level structure is the same as any spawned process.

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

impl Process<List<String>, Signal, ExitCode> for App
  fn new(config: List<String>) -> Self
    App{}
  end

  fn handle(move self, msg: Signal, from: Option<ReplyTo<ExitCode>>) -> Self
    IO.puts("Hello, Expo!")
    self
  end
end
```

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
- **PROCESS.md, lines 304-312 / ROADMAP.md, lines 304-312:** `fn main` as `Process<C,M,R>` -- the entry type *is* the `Process` impl. argv as config, signals as messages, exit code as reply.
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

---

## TBD: `project.exps` replacing `expo.toml`

The introduction of `.exps` scripts opens the possibility of replacing `expo.toml` with `project.exps` -- a script that evaluates to a `Project` struct. This would be analogous to Elixir's `mix.exs`: the project config is written in the language itself, with the ability to compute values (e.g., reading a version from a file). The compiler would execute `project.exps` to obtain the `Project` struct before compiling the project. Design details deferred.

### Status: exploration

Not yet implemented. This is a design exploration capturing the intended direction. Implementation would touch the parser, type checker, codegen, CLI, and project scaffolding, plus a migration of the stdlib and test suite.
