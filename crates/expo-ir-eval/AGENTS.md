# expo-alpha-ir-eval

Tree-walking interpreter built to the [`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md)
contract. Alpha-track sibling to the legacy `expo-ir-eval`; the two share **no
code** and **no types** — this crate consumes only the sealed `IRProgram` from
`expo-alpha-ir`, never the v1 `expo-ir`.

## Public surface

Two entry points, one per IR shape:

```rust
pub fn Interpreter::run_program(program: IRProgram) -> Result<Value, RuntimeError>;
pub fn Interpreter::run_script(script: IRScript) -> Result<Value, RuntimeError>;
```

`run_program` executes the program's `entry_function()` and returns its
produced [`Value`] (or `Value::Unit` if the entry returns nothing).
`run_script` executes the script's implicit body (`script.blocks`) the
same way and returns the value of its trailing expression.

The input is **always sealed**: `expo-alpha-ir::lower_program` /
`lower_script` enforce SSA definition-before-use, terminator presence,
and (in the program path) entry-point resolution before handing back
the IR. The interpreter therefore performs **no** program-level
validation; missing values, missing entry points, or unterminated
blocks would be seal violations upstream and panic in
`expo-alpha-ir::seal`, never surface here.

A shared internal walker drives both entry points; only the
call-resolution closure differs (`IRProgram::function` vs
`IRScript::function`).

## Runtime errors

[`RuntimeError`] covers only conditions the program can reach at runtime
without a compiler bug:

- `DivisionByZero { op }` — `lhs / rhs` or `lhs % rhs` with `rhs == 0`.
- `IntegerOverflow { lhs, op, rhs }` — `i64` arithmetic outside range.
- `TypeMismatch { detail }` — a binary operator received operands whose
  runtime types it can't combine. Eval today only knows `Int op Int`,
  `Bool op Bool`, and the comparison/equality forms.
- `Unsupported { detail }` — IR shapes the interpreter doesn't yet handle.
- `ValueUndefined { id }` — defensive guard; should be unreachable on a
  sealed program.

## Values

Today's scope mirrors `expo-alpha-ir::ConstValue`:

- `Value::Bool(bool)`
- `Value::Int(i64)`
- `Value::Unit`

New variants land as the IR vocabulary grows (lists, strings, structs,
enums, closures, …).

## What alpha covers today

Both shapes of `2 + 2` end-to-end:

- Project mode: `fn main; 2 + 2; end` through `parse_program →
  check_program → lower_program → Interpreter::run_program`.
- Script mode: bare `2 + 2\n` through `parse_program → check_program
  → lower_script → Interpreter::run_script`.

`tests/two_plus_two.rs` exercises both paths; `tests/calls.rs` and
`tests/boolean_ops.rs` mirror the same project-mode coverage and
add focused script-mode regressions for calls and boolean / comparison
operators.

## Hard contract

- **Zero dependency on `expo-ir` or `expo-ir-eval`.** Those crates are the
  legacy v1 path; alpha is a clean cut. Do not add either as a dep, do not
  import a single type, do not even glance at them for inspiration without
  first asking whether the alpha shape should differ.
- **Sealed input only.** The interpreter trusts the seal and skips its own
  validation. Bugs that surface as `ValueUndefined` are seal violations to
  fix upstream, not failure modes to handle here.
