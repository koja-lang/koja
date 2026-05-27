# koja-ir-eval

Tree-walking interpreter built to the
[`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md) contract.
Consumes the sealed `IRProgram` / `IRScript` produced by `koja-ir`.

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

The input is **always sealed**: `koja-ir::lower_program` / `lower_script`
enforce SSA definition-before-use, terminator presence, and (in the program
path) entry-point resolution before handing back the IR. The interpreter
therefore performs **no** program-level validation; missing values,
missing entry points, or unterminated blocks would be seal violations
upstream and panic in `koja-ir::seal`, never surface here.

A shared internal walker drives both entry points; only the
call-resolution closure differs (`IRProgram::function` vs
`IRScript::function`).

## Runtime errors

[`RuntimeError`] covers only conditions the program can reach at runtime
without a compiler bug:

- `DivisionByZero { op }` — `lhs / rhs` or `lhs % rhs` with `rhs == 0`.
- `IntegerOverflow { lhs, op, rhs }` — `i64` arithmetic outside range.
- `TypeMismatch { detail }` — a binary operator received operands whose
  runtime types it can't combine.
- `Unsupported { detail }` — IR shapes the interpreter doesn't yet handle.
- `ValueUndefined { id }` — defensive guard; should be unreachable on a
  sealed program.

## Values

`Value` mirrors `koja-ir::ConstValue` plus the runtime-only variants needed
for closures, heap structures, and externs. New variants land as the IR
vocabulary grows.

## Hard contract

- **Sealed input only.** The interpreter trusts the seal and skips its own
  validation. Bugs that surface as `ValueUndefined` are seal violations to
  fix upstream, not failure modes to handle here.
