# koja-ir

Lowering phase built to the
[`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md) contract.

## Public surface

```rust
pub fn lower_program(
    checked: &CheckedProgram,
    entry_state: &Identifier,
) -> Result<IRProgram, LowerError>

pub fn lower_script(checked: &CheckedProgram) -> Result<IRScript, LowerError>
```

`lower_program` lowers a project and synthesizes wrappers for the concrete
`Process` state named by `entry_state`. `lower_script` lowers top-level
statements from script mode.

Successful outputs are sealed. Every identifier is unique, every CFG target
exists, SSA uses dominate their uses, operand and result types agree, callees
and declarations resolve, synthesized functions live in their explicit owner
package, and project entry symbols are registered. Seal failures panic because
they indicate compiler bugs.

`LowerError::Diagnostics` carries user-facing lowering failures.
`LowerError::EntryPointNotFound` reports a missing project entry state.

## Project pipeline

```text
lower packages
coalesce fragments that share a package label
validate the entry state, enqueue its methods, and synthesize wrappers
instantiate discovered generics to a fixpoint
assemble the working IRProgram
break recursive type-layout cycles
rewrite self tail calls
insert cooperative yield checks
elaborate ownership glue and runtime delivery arms
seal the complete IRProgram
```

Script lowering uses the same package, generic, cycle, merge, elaborate,
tail-call, yield-check, and seal passes around its inline body.

## Hard contracts

- Lowering consumes only sealed `CheckedProgram` data.
- Lowering and whole-program passes mint all final symbols and declarations.
- Backends never perform lazy monomorphization or semantic backfill.
- Every `IRInstruction` has a direct backend interpretation.
- Coercion decisions remain in typecheck and become explicit IR operations.
- Package ownership is explicit. Symbol prefixes and first-package fallbacks
  are not routing mechanisms.
