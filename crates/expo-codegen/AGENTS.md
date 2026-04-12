# expo-codegen

LLVM IR generation from typed Expo AST via inkwell.

## Key files -- start here

- `compiler.rs` -- `Compiler` struct, module emission, `define_function`, orchestration (~1610 lines)
- `generics.rs` -- Monomorphization engine, name mangling (`Type_$args$`), `compile_function_body` (~860 lines)
- `expr.rs` -- Expression compilation, returns `TypedValue<'ctx>` (LLVM value + Expo Type) (~1468 lines)
- `stmt.rs` -- Statement compilation, coercions, scope-boundary drops
- `structs.rs` -- Method dispatch, struct/enum construction, field access (~977 lines)

## Supporting files

- `registration.rs` -- Multi-pass LLVM struct/enum type registration from TypeContext
- `types.rs` -- `to_llvm_type`: Expo `Type` -> LLVM type conversion
- `calls.rs` -- Function calls, method dispatch, closure invocation
- `drop.rs` -- Ownership drop insertion at scope exit. `drop_live_variables` with skip parameter
- `ops.rs` -- Binary/unary operators, string comparison
- `builtins.rs` -- Declares C stdlib / runtime extern symbols
- `debug.rs` -- Debug protocol `format` synthesis
- `debug_info.rs` -- DWARF metadata via `DIBuilder`
- `enums.rs` -- Enum variant construction and equality
- `util.rs` -- Int literal parsing, printf helpers

## Subdirectories

- `control/` -- conditionals.rs, loops.rs, patterns.rs (match lowering)
- `binary/` -- Binary/Bits literal construction and pattern matching
- `intrinsics/` -- Compiler-generated code for stdlib types:
  - `io.rs` -- File I/O intrinsics (follow this pattern for new file operations)
  - `socket.rs` -- Socket intrinsics
  - `string.rs` -- String/Binary conversion and parsing
  - `cptr.rs` -- CPtr and CString intrinsics
  - `hash.rs` -- Hash, Equality, Bitwise protocol intrinsics
  - `format.rs` -- Primitive debug formatting
  - `system.rs` -- Random and system helpers
- `process.rs` -- Ref/ReplyTo actor codegen, mailbox types
- `spawn.rs` -- spawn wrapper IR and entry-process generation
- `list.rs` / `map.rs` / `set.rs` / `hashtable.rs` -- Collection type codegen

## Key concepts

- `TypedValue<'ctx>` -- pairs `BasicValueEnum` with Expo `Type`, used everywhere
- `ExprResult<'ctx>` = `Result<Option<TypedValue<'ctx>>, String>`
- `Ownership` -- `Owned` (will be freed) vs `Unowned` (borrowed/parameter)
- Variables stored as `HashMap<String, (PointerValue, Type, Ownership)>`
- Monomorphized names use `_$...$` delimiters: `Pair_$i32.string$`
