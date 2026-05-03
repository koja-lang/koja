# expo-ir-eval-v2

Tree-walking interpreter built to the [`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md)
contract. Sibling to the legacy `expo-ir-eval`; the two share **no code** and
**no types** — this crate consumes only the sealed `IRProgram` from
`expo-ir-v2`, never the v1 `expo-ir`.

## Public surface

Two entry points:

```rust
pub fn Interpreter::new(program: IRProgram) -> Self;
pub fn Interpreter::run(&self) -> Result<Value, RuntimeError>;
```

The input is **always sealed**: `expo-ir-v2::lower_program` enforces SSA
definition-before-use, terminator presence, and entry-point resolution before
constructing the `IRProgram`. The interpreter therefore performs **no**
program-level validation; missing values, missing entry points, or
unterminated blocks would be seal violations upstream and panic in
`expo-ir-v2::seal`, never surface here.

`run()` executes the program's `entry_function()` and returns its produced
[`Value`] (or `Value::Unit` if the entry returns nothing).

## Runtime errors

[`RuntimeError`] covers only conditions the program can reach at runtime
without a compiler bug:

- `DivisionByZero { op }` — `lhs / rhs` or `lhs % rhs` with `rhs == 0`.
- `IntegerOverflow { lhs, op, rhs }` — `i64` arithmetic outside range.
- `TypeMismatch { detail }` — a binary operator received operands whose
  runtime types it can't combine. POC eval only knows `Int op Int`.
- `Unsupported { detail }` — IR shapes the interpreter doesn't yet handle.
- `ValueUndefined { id }` — defensive guard; should be unreachable on a
  sealed program.

## Values

POC scope mirrors `expo-ir-v2::ConstValue`:

- `Value::Bool(bool)`
- `Value::Int(i64)`
- `Value::Unit`

New variants land as the IR vocabulary grows (lists, strings, structs,
enums, closures, …).

## What v2 covers today

`fn main; 2 + 2; end` end-to-end through `parse_program → check_program →
lower_program → Interpreter::run`. `tests/two_plus_two.rs` exercises
four POC scenarios: the canonical `2 + 2`, integer-arithmetic
combinations (`+ - * / %` plus parentheses), runtime division-by-zero,
and an empty `fn main` returning `Unit`.

## Hard contract

- **Zero dependency on `expo-ir` or `expo-ir-eval`.** Those crates are the
  legacy v1 path; v2 is a clean cut. Do not add either as a dep, do not
  import a single type, do not even glance at them for inspiration without
  first asking whether the v2 shape should differ.
- **Sealed input only.** The interpreter trusts the seal and skips its own
  validation. Bugs that surface as `ValueUndefined` are seal violations to
  fix upstream, not failure modes to handle here.
