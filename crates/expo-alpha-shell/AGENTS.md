# expo-alpha-shell

Interactive REPL for the alpha pipeline. Alpha-track sibling to the legacy
`expo-shell`; the two share **no code** and **no types** — alpha is a clean
cut and drives `expo-parser → expo-alpha-typecheck → expo-alpha-ir →
expo-alpha-ir-eval` from scratch.

## Public surface

One entry point:

```rust
pub fn run();
```

Reads stdin until `:quit` or EOF, evaluates each input through the alpha
pipeline, and prints trailing-expression values. Recovers cleanly from
parse / typecheck / lower / runtime errors by rolling the session back to
its pre-input state.

## Architecture

```
run                -> stdin loop: prompt, multiline buffer, command dispatch
Session            -> accumulating REPL state (statement history + counter)
is_input_complete  -> token-level check: are blocks/brackets/strings closed?
run_pipeline       -> drive one source string through the alpha pipeline
format_check_failure / parse_diagnostics / format_block
                   -> render typecheck / parse failures as user-facing strings
```

The session is whole-program today: every input causes the entire history
to re-parse, re-typecheck, re-lower, and re-interpret. This is the simplest
shape that makes state "just work" without incremental machinery, and perf
is fine for the first few hundred lines. Future incremental work would
reshape `Session` around chunk boundaries; today's layout is the reference
shape.

## REPL commands

```
:help    show command list
:quit    exit (also Ctrl-D / EOF)
:reset   clear session state and discard the multiline buffer
:state   print number of accumulated statement blocks
```

`:reset` works mid-multiline to abandon a partial input. `:quit`, `:help`,
`:state` only fire on a fresh prompt (not inside a multiline buffer).

## Promotion plan

When the alpha shell grows file-input support (e.g. `pub fn eval_source(...)`
that reads a `.expo` file), `expo-driver/src/alpha.rs::cmd_eval` collapses
into a thin wrapper around it — same as `cmd_shell` does today for `run`.
At that point the driver's local `run_pipeline` copy disappears and this
crate becomes the single home for "drive a string through the alpha
pipeline." Eventually, when alpha reaches feature parity with v1, it
graduates to `expo-shell` and the legacy crate retires.

## Hard contract

- **Zero dependency on `expo-ir`, `expo-ir-eval`, `expo-typecheck`, or
  `expo-shell`.** Those crates are the legacy v1 path; alpha is a clean
  cut. Use only the alpha pipeline crates.
- **Self-contained.** No path back through `expo-driver` — the driver
  depends on this crate, never the other way round.
- **One public function.** Today only `run()` is `pub`; the pipeline driver
  and helpers stay private until a second consumer needs them.

## What alpha covers today

POC scope mirrors `expo-alpha-typecheck` / `expo-alpha-ir`: integer
literals, integer arithmetic (`+ - * / %`), parenthesized groups. Anything
richer typecheck-errors with a precise diagnostic, then the session rolls
back and the user can retry.
