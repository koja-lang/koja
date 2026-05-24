# koja-shell

Interactive REPL driving `koja-parser → koja-typecheck → koja-ir →
koja-ir-eval`.

## Public surface

One entry point:

```rust
pub fn run();
```

Reads stdin until `:quit` or EOF, evaluates each input through the
pipeline, and prints trailing-expression values. Recovers cleanly from
parse / typecheck / lower / runtime errors by rolling the session back to
its pre-input state.

## Architecture

```
run                -> top-level loop: read_input + command dispatch + try_eval
read_input         -> one-input read using rustyline line-at-a-time;
                      first incomplete line triggers an ANSI rewrite so the
                      prompt + first line drop onto their own rows, then
                      reads continuation lines with an empty prompt until
                      is_input_complete flips true
erase_current_line -> ANSI escape sequence (cursor up, erase line) backing
                      the rewrite trick
Session            -> accumulating REPL state (statement history + counter)
is_input_complete  -> token-level check: are blocks/brackets/strings closed?
run_pipeline       -> drive one source string through the pipeline
format_check_failure / parse_diagnostics / format_block
                   -> render typecheck / parse failures as user-facing strings
```

Line editing runs through `rustyline::Editor` (no `Validator`, no
`Helper`): each `Editor::readline` returns one terminal line with full
editing + history. `read_input` decides when an input is complete via
`is_input_complete` against the accumulated buffer. When the first line
is incomplete (a block-opening keyword, unclosed bracket, dangling
string), `read_input` emits `\x1b[1A\r\x1b[2K` (cursor up, erase line)
on a TTY and reprints `koja(N)>` and the typed first line on their own
rows. Subsequent reads pass an empty prompt so the block reads as raw
code on the terminal.

The session is whole-program today: every input causes the entire history
to re-parse, re-typecheck, re-lower, and re-interpret. This is the simplest
shape that makes state "just work" without incremental machinery, and perf
is fine for the first few hundred lines. Future incremental work would
reshape `Session` around chunk boundaries; today's layout is the reference
shape.

The `rustyline` boundary is intentionally narrow — `Editor::readline`
gives us a single edited line, `add_history_entry` records the full
accepted buffer (multi-line blocks land as one entry) for up-arrow
recall. When `Session` eventually migrates to koja (or the line editor
itself gets rewritten), this boundary is the natural cut point.

## REPL commands

```
:help    show command list
:quit    exit (also Ctrl-D / EOF on a fresh prompt)
:reset   clear session state (or abandon a partial multi-line input)
:state   print number of accumulated statement blocks
```

`:reset` typed on a continuation line discards the in-flight buffer
and reprompts (no session-state change). `Ctrl-C` does the same.
`:quit`, `:help`, and `:state` only fire when typed on a fresh prompt
(they're checked against the trimmed final buffer). Up-arrow recalls
previous accepted inputs (multi-line blocks come back as one editable
entry); history is in-memory only and is discarded on exit.

## Hard contract

- **Self-contained.** No path back through `koja-driver` — the driver
  depends on this crate, never the other way round.
- **One public function.** Today only `run()` is `pub`; the pipeline
  driver and helpers stay private until a second consumer needs them.
